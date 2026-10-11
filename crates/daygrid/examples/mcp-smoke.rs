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
        Implementation::new("daygrid-smoke", "0.1.0"),
    )
    .with_protocol_version(if legacy {
        ProtocolVersion::V_2025_11_25
    } else {
        ProtocolVersion::default()
    });
    let client = config.serve(TokioChildProcess::new(command)?).await?;
    let tools = client.list_all_tools().await?;
    assert_eq!(tools.len(), 6);
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
        "PASS: SDK stdio client; {} tools; create/replay/update/get/list/status; desktop sees same SQLite data; live pause denial; protocol {}",
        tools.len(),
        client.peer_info().unwrap().protocol_version
    );
    client.cancel().await?;
    Ok(())
}
