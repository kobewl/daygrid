//! Actual SDK client -> child-process stdio bridge -> Unix socket -> SQLite.
use daygrid::{
    Error,
    ipc::{self, IPC_VERSION, Request},
};
use rmcp::{ServiceExt, model::*, transport::TokioChildProcess};
use serde_json::{Value, json};
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;

struct ServiceProcess(std::process::Child);
impl Drop for ServiceProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    ipc::prepare(dir.path()).map_err(|e| e.message)?;
    let token = ipc::secret(dir.path(), "desktop.key").map_err(|e| e.message)?;
    let target = std::env::current_exe()?
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let _service = ServiceProcess(
        Command::new(target.join("daygrid-service"))
            .env("DAYGRID_DATA_DIR", dir.path())
            .stdout(Stdio::null())
            .spawn()?,
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while ipc::ping(dir.path()).is_err() {
        assert!(Instant::now() < deadline, "Service failed to start");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let desktop = |method: &str, args: Value| -> Result<Value, Error> {
        ipc::exchange(
            dir.path(),
            &Request {
                ipc_version: IPC_VERSION,
                actor: "desktop".into(),
                token: token.clone(),
                method: method.into(),
                args,
            },
        )
    };
    let connection = desktop(
        "desktop_create_connection",
        json!({"label":"SDK smoke test","permission":"write"}),
    )
    .map_err(|e| e.message)?;
    let id = connection["data"]["id"].as_str().unwrap();
    ipc::write_credential(
        dir.path(),
        id,
        connection["data"]["credential"].as_str().unwrap(),
    )
    .map_err(|e| e.message)?;
    let mut command = tokio::process::Command::new(target.join("daygrid-mcp"));
    command
        .env("DAYGRID_DATA_DIR", dir.path())
        .args(["--connection", id]);
    let legacy = std::env::args().any(|arg| arg == "--legacy");
    let config = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("daygrid-smoke", env!("CARGO_PKG_VERSION")),
    )
    .with_protocol_version(if legacy {
        ProtocolVersion::V_2025_11_25
    } else {
        ProtocolVersion::default()
    });
    let client = config.serve(TokioChildProcess::new(command)?).await?;
    let tools = client.list_all_tools().await?;
    assert_eq!(tools.len(), 9);
    assert!(
        tools
            .iter()
            .all(|t| t.title.is_some() && t.output_schema.is_some())
    );
    let context = client
        .call_tool(CallToolRequestParams::new("daygrid_get_context"))
        .await?;
    let value = context.structured_content.unwrap();
    let generation = value["data"]["calendar_generation"].clone();
    let args = json!({"calendar_generation":generation,"idempotency_key":Uuid::new_v4().to_string(),"date":"2026-10-15","title":"MCP 创建事项","notes":"actual stdio client","color":"sage"});
    let call = CallToolRequestParams::new("daygrid_create_item")
        .with_arguments(args.as_object().unwrap().clone());
    let first = client.call_tool(call.clone()).await?;
    let replay = client.call_tool(call).await?;
    let first = first.structured_content.unwrap();
    let replay = replay.structured_content.unwrap();
    assert_eq!(first["ok"], true);
    assert_eq!(replay["replayed"], true);
    let item_id = first["data"]["item"]["id"].clone();
    let update = json!({"calendar_generation":generation,"idempotency_key":Uuid::new_v4().to_string(),"item_id":item_id,"expected_revision":1,"patch":{"date":"2026-10-16","color":"violet"}});
    let result = client
        .call_tool(
            CallToolRequestParams::new("daygrid_update_item")
                .with_arguments(update.as_object().unwrap().clone()),
        )
        .await?;
    let updated = result.structured_content.unwrap();
    assert_eq!(updated["data"]["item"]["revision"], 2);
    let native = desktop("daygrid_get_item", json!({"item_id":item_id})).map_err(|e| e.message)?;
    assert_eq!(native["data"]["item"]["date"], "2026-10-16");
    assert_eq!(native["data"]["item"]["color"], "violet");
    let read = client
        .call_tool(
            CallToolRequestParams::new("daygrid_get_item")
                .with_arguments(json!({"item_id":item_id}).as_object().unwrap().clone()),
        )
        .await?;
    assert_eq!(
        read.structured_content.unwrap()["data"]["item"]["revision"],
        2
    );
    let listed = client
        .call_tool(
            CallToolRequestParams::new("daygrid_list_items").with_arguments(
                json!({"from_date":"2026-10-01","to_date":"2026-10-31"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await?;
    assert_eq!(
        listed.structured_content.unwrap()["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let done=client.call_tool(CallToolRequestParams::new("daygrid_set_item_status").with_arguments(json!({"calendar_generation":generation,"idempotency_key":Uuid::new_v4().to_string(),"item_id":item_id,"expected_revision":2,"status":"done"}).as_object().unwrap().clone())).await?;
    assert_eq!(
        done.structured_content.unwrap()["data"]["item"]["status"],
        "done"
    );
    // ---- v0.2：概览、资源、提示、审批与长轮询 ----
    let overview = client
        .call_tool(
            CallToolRequestParams::new("daygrid_month_overview").with_arguments(
                json!({"from_date":"2026-10-01","to_date":"2026-10-31"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await?
        .structured_content
        .unwrap();
    assert_eq!(overview["data"]["total_items"], 1);
    assert_eq!(overview["data"]["days"][0]["done"], 1);
    assert_eq!(client.list_all_resources().await?.len(), 2);
    assert_eq!(client.list_all_resource_templates().await?.len(), 1);
    let ctx = client
        .read_resource(ReadResourceRequestParams::new("daygrid://context"))
        .await?;
    assert!(format!("{:?}", ctx.contents).contains("calendar_generation"));
    let day = client
        .read_resource(ReadResourceRequestParams::new("daygrid://day/2026-10-16"))
        .await?;
    assert!(format!("{:?}", day.contents).contains("MCP 创建事项"));
    assert!(
        client
            .read_resource(ReadResourceRequestParams::new("daygrid://nope"))
            .await
            .is_err()
    );
    let prompts = client.list_all_prompts().await?;
    assert_eq!(prompts.len(), 2);
    let brief = client
        .get_prompt(GetPromptRequestParams::new("daily_brief"))
        .await?;
    assert_eq!(brief.messages.len(), 1);
    // 删除必须经桌面确认：先得到 pending，再由「用户」在 700ms 后批准，
    // 助手用 wait_seconds 长轮询，应当在批准后立即拿到结果而不是等满超时。
    let delete = json!({"calendar_generation":generation,"idempotency_key":Uuid::new_v4().to_string(),"item_id":item_id,"expected_revision":3,"reason":"smoke test"});
    let pending = client
        .call_tool(
            CallToolRequestParams::new("daygrid_delete_item")
                .with_arguments(delete.as_object().unwrap().clone()),
        )
        .await?
        .structured_content
        .unwrap();
    assert_eq!(pending["data"]["pending_approval"], true, "{pending}");
    let proposal_id = pending["data"]["proposal"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        desktop("daygrid_get_item", json!({"item_id":item_id})).map_err(|e| e.message)?["ok"]
            == true
    );
    let (approve_dir, approve_token, approve_id) =
        (dir.path().to_path_buf(), token.clone(), proposal_id.clone());
    let approver = tokio::task::spawn_blocking(move || {
        std::thread::sleep(Duration::from_millis(700));
        ipc::exchange(
            &approve_dir,
            &Request {
                ipc_version: IPC_VERSION,
                actor: "desktop".into(),
                token: approve_token,
                method: "desktop_decide_proposal".into(),
                args: json!({"proposal_id":approve_id,"decision":"approve"}),
            },
        )
    });
    let started = Instant::now();
    let decided = client
        .call_tool(
            CallToolRequestParams::new("daygrid_get_proposal").with_arguments(
                json!({"proposal_id":proposal_id,"wait_seconds":10})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await?
        .structured_content
        .unwrap();
    approver.await?.map_err(|e| e.message)?;
    assert_eq!(
        decided["data"]["proposal"]["state"], "approved",
        "{decided}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "长轮询应在批准后立即返回"
    );
    assert_eq!(
        desktop("daygrid_get_item", json!({"item_id":item_id})).map_err(|e| e.message)?["error"]["code"],
        "NOT_FOUND"
    );
    // 审计：桌面能看到助手的这些调用。
    let activity = desktop("desktop_activity", json!({})).map_err(|e| e.message)?;
    assert!(activity["data"]["activity"].as_array().unwrap().len() >= 10);
    desktop("desktop_set_connection", json!({"id":id,"state":"paused"})).map_err(|e| e.message)?;
    let denied = client
        .call_tool(CallToolRequestParams::new("daygrid_get_context"))
        .await?;
    {
        assert_eq!(denied.is_error, Some(true));
        assert_eq!(
            denied.structured_content.unwrap()["error"]["code"],
            "PERMISSION_DENIED"
        )
    }
    println!(
        "PASS: SDK stdio client; {} tools; resources+prompts; create/replay/update/get/list/status/overview; delete approval + long-poll; audit trail; desktop sees same SQLite data; live pause denial; protocol {}",
        tools.len(),
        client.peer_info().unwrap().protocol_version
    );
    client.cancel().await?;
    Ok(())
}
