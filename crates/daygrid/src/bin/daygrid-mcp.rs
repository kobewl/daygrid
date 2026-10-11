//! Daygrid 的 MCP stdio 桥接。由 AI 客户端作为子进程启动：
//! `daygrid-mcp --connection <UUID>`。
//!
//! 它不碰数据库，只把 MCP 请求翻译成对本地服务的调用（服务才是唯一写入者），
//! 因此界面与助手看到的永远是同一份数据、同一套校验。
//!
//! 约定：stdout 只输出 MCP 协议；日志写入数据目录的 `logs/mcp.*.log`，
//! 警告及以上同时输出到 stderr（多数客户端会在其「MCP 日志」里展示）。
use daygrid::{VERSION, ipc, logging, tools};
use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    model::*,
    service::{RequestContext, RoleServer},
    transport::stdio,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// 长轮询上限：多数 MCP 客户端的单次调用超时是 60 秒，留足余量。
const MAX_WAIT_SECONDS: u64 = 25;

#[derive(Clone)]
struct Calendar {
    dir: PathBuf,
    connection: String,
    credential: String,
    tools: Vec<Tool>,
}

fn resources() -> Vec<Resource> {
    vec![
        Resource::new("daygrid://context", "context")
            .with_title("Calendar context")
            .with_description("Today's date, weekday, timezone, calendar_generation and limits.")
            .with_mime_type("application/json"),
        Resource::new("daygrid://today", "today")
            .with_title("Today's items")
            .with_description("Items scheduled for today in the calendar's timezone.")
            .with_mime_type("application/json"),
    ]
}
fn resource_templates() -> Vec<ResourceTemplate> {
    vec![
        ResourceTemplate::new("daygrid://day/{date}", "day")
            .with_title("Items of one day")
            .with_description("Items on a given date, YYYY-MM-DD.")
            .with_mime_type("application/json"),
    ]
}
fn prompts() -> Vec<Prompt> {
    vec![
        Prompt::new(
            "daily_brief",
            Some("Summarize what is planned and done on a day. Read-only."),
            Some(vec![
                PromptArgument::new("date")
                    .with_description("YYYY-MM-DD; defaults to today.")
                    .with_required(false),
            ]),
        ),
        Prompt::new(
            "plan_week",
            Some(
                "Look at the next two weeks and propose where new items could go. Asks before changing anything.",
            ),
            Some(vec![
                PromptArgument::new("focus")
                    .with_description("What you want to make room for, e.g. 'finish the report'.")
                    .with_required(false),
            ]),
        ),
    ]
}
fn prompt_text(name: &str, args: &serde_json::Map<String, Value>) -> Option<String> {
    let arg = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
    };
    match name {
        "daily_brief" => Some(format!(
            "Use the Daygrid tools to brief me on {}.\n\
             1. Call daygrid_get_context to get today's date and weekday.\n\
             2. Call daygrid_list_items for that single date.\n\
             3. Summarize what is planned and what is already done, grouped by color if it helps. \
             Do not modify the calendar.",
            arg("date").map_or("today".to_string(), |d| format!("the date {d}"))
        )),
        "plan_week" => Some(format!(
            "Help me plan the next two weeks with Daygrid.{}\n\
             1. Call daygrid_get_context for today's date.\n\
             2. Call daygrid_month_overview for today through the next 13 days to see which days are busy.\n\
             3. Call daygrid_list_items for the busy days when you need details.\n\
             4. Propose where new items fit. Do NOT create, change or delete anything until I confirm. \
             Deleting always needs my approval inside Daygrid.",
            arg("focus").map_or(String::new(), |f| format!(" Focus: {f}."))
        )),
        _ => None,
    }
}

fn mcp_error(message: &str) -> ErrorData {
    ErrorData::internal_error(message.to_string(), None)
}

impl Calendar {
    /// 在阻塞线程里调用本地服务。
    async fn call(&self, method: &str, args: Value) -> Result<Value, ErrorData> {
        let (dir, actor, token, method) = (
            self.dir.clone(),
            self.connection.clone(),
            self.credential.clone(),
            method.to_string(),
        );
        tokio::task::spawn_blocking(move || ipc::call(&dir, &actor, &token, &method, args))
            .await
            .map_err(|_| mcp_error("Local service task failed"))
    }

    async fn read_resource_json(&self, uri: &str) -> Result<Value, ErrorData> {
        let invalid =
            || ErrorData::invalid_params(format!("Unknown Daygrid resource: {uri}"), None);
        let rest = uri.strip_prefix("daygrid://").ok_or_else(invalid)?;
        let value = match rest {
            "context" => self.call("daygrid_get_context", json!({})).await?,
            "today" => {
                let ctx = self.call("daygrid_get_context", json!({})).await?;
                let Some(today) = ctx["data"]["today"].as_str() else {
                    return Ok(ctx);
                };
                self.call(
                    "daygrid_list_items",
                    json!({"from_date":today,"to_date":today,"limit":100}),
                )
                .await?
            }
            _ => match rest.strip_prefix("day/") {
                Some(date) => {
                    self.call(
                        "daygrid_list_items",
                        json!({"from_date":date,"to_date":date,"limit":100}),
                    )
                    .await?
                }
                None => return Err(invalid()),
            },
        };
        if value["ok"] == true {
            Ok(value["data"].clone())
        } else {
            Err(ErrorData::invalid_params(
                value["error"]["message"]
                    .as_str()
                    .unwrap_or("Daygrid request failed")
                    .to_string(),
                Some(value["error"].clone()),
            ))
        }
    }
}

impl ServerHandler for Calendar {
    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        std::borrow::Cow::Borrowed(&[ProtocolVersion::V_2025_11_25])
    }
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_server_info(Implementation::new("daygrid", VERSION).with_title("日格 Daygrid"))
        .with_protocol_version(ProtocolVersion::V_2025_11_25)
        .with_instructions(tools::SERVER_INSTRUCTIONS)
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|t| t.name == name).cloned()
    }
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: self.tools.clone(),
            ..Default::default()
        })
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if self.get_tool(&request.name).is_none() {
            return Err(ErrorData::invalid_params("Unknown Daygrid tool", None));
        }
        let started = Instant::now();
        let mut args = request.arguments.unwrap_or_default();
        let name = request.name.to_string();
        // wait_seconds 只存在于桥接层：在这里长轮询，服务端接口保持无状态。
        let wait = if name == "daygrid_get_proposal" {
            args.remove("wait_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                .min(MAX_WAIT_SECONDS)
        } else {
            0
        };
        let deadline = Instant::now() + Duration::from_secs(wait);
        let value = loop {
            let value = self.call(&name, Value::Object(args.clone())).await?;
            let pending = value["data"]["proposal"]["state"] == "pending";
            if !(wait > 0 && value["ok"] == true && pending && Instant::now() < deadline) {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        };
        tracing::info!(
            tool = %name,
            ok = value["ok"] == true,
            code = value["error"]["code"].as_str().unwrap_or("-"),
            ms = started.elapsed().as_millis() as u64,
            "tool call"
        );
        let mut result = CallToolResult::success(vec![ContentBlock::text(value.to_string())]);
        result.is_error = Some(value["ok"] != true);
        result.structured_content = Some(value);
        Ok(result.into())
    }
    async fn list_resources(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(resources()))
    }
    async fn list_resource_templates(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(
            resource_templates(),
        ))
    }
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let data = self.read_resource_json(&request.uri).await?;
        tracing::info!(resource = %request.uri.split('/').nth(2).unwrap_or("?"), "resource read");
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(serde_json::to_string_pretty(&data).unwrap(), request.uri)
                .with_mime_type("application/json"),
        ])
        .into())
    }
    async fn list_prompts(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(prompts()))
    }
    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let args = request.arguments.unwrap_or_default();
        let text = prompt_text(&request.name, &args)
            .ok_or_else(|| ErrorData::invalid_params("Unknown Daygrid prompt", None))?;
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)]).into())
    }
}

const USAGE: &str = "Daygrid MCP 桥接\n\n\
用法：\n  daygrid-mcp --connection <UUID>   作为 MCP stdio 服务器运行（由 AI 客户端启动）\n  \
daygrid-mcp --connection <UUID> --check   自检：数据目录、凭据、本地服务与授权\n  \
daygrid-mcp --print-tools          打印工具目录（JSON）\n  \
daygrid-mcp --version\n\n连接 UUID 在日格应用的「助手连接」里创建。";

fn credential_for(dir: &Path, id: &str) -> Result<String, String> {
    ipc::credential(dir, id).map_err(|e| e.message)
}

/// `--check`：逐项检查并给出可操作的提示，退出码 0 表示全部通过。
fn check(dir: &Path, id: &str) -> bool {
    println!("Daygrid MCP 桥接 {VERSION}");
    println!("数据目录：{}", dir.display());
    let credential = match credential_for(dir, id) {
        Ok(c) => {
            println!("✓ 连接凭据存在");
            c
        }
        Err(m) => {
            println!("✗ {m}\n  提示：在日格「助手连接」里重新建立连接，并使用新生成的配置。");
            return false;
        }
    };
    if let Err(e) = ipc::ensure_service(dir) {
        println!(
            "✗ 日历服务不可用：{}\n  提示：查看日志目录 {}",
            e.message,
            logging::logs_dir(dir).display()
        );
        return false;
    }
    println!("✓ 日历服务可用");
    let context = ipc::call(dir, id, &credential, "daygrid_get_context", json!({}));
    if context["ok"] != true {
        println!(
            "✗ 连接未授权：{}\n  提示：连接可能已暂停或撤销，请在日格「助手连接」里检查。",
            context["error"]["message"].as_str().unwrap_or("未知错误")
        );
        return false;
    }
    println!(
        "✓ 连接已授权（今天 {}，时区 {}，确认策略 {}）",
        context["data"]["today"].as_str().unwrap_or("?"),
        context["data"]["calendar_timezone"].as_str().unwrap_or("?"),
        context["data"]["approval_mode"].as_str().unwrap_or("?"),
    );
    let tool_count = tools::catalog()["tools"].as_array().map_or(0, Vec::len);
    println!(
        "✓ {tool_count} 个工具，{} 个资源，{} 个提示",
        resources().len() + resource_templates().len(),
        prompts().len()
    );
    true
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("daygrid-mcp {VERSION}");
            return Ok(());
        }
        Some("--print-tools") => {
            println!("{}", serde_json::to_string_pretty(&tools::catalog())?);
            return Ok(());
        }
        Some("--help" | "-h") => {
            println!("{USAGE}");
            return Ok(());
        }
        _ => {}
    }
    let id = match args.as_slice() {
        [flag, id] | [flag, id, _] if flag == "--connection" => id.clone(),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    let dir = ipc::data_dir().map_err(|e| e.message)?;
    if args.get(2).map(String::as_str) == Some("--check") {
        std::process::exit(if check(&dir, &id) { 0 } else { 1 });
    }
    if args.len() != 2 {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let _logs = logging::init(&dir, "mcp", true);
    let credential = credential_for(&dir, &id).inspect_err(|m| tracing::error!("{m}"))?;
    let tools: Vec<Tool> = serde_json::from_value(tools::catalog()["tools"].clone())?;
    tracing::info!(version = VERSION, connection = %id.chars().take(8).collect::<String>(), "mcp bridge started");
    let calendar = Calendar {
        dir,
        connection: id,
        credential,
        tools,
    };
    let service = calendar.serve(stdio()).await?;
    service.waiting().await?;
    tracing::info!("mcp bridge stopped");
    Ok(())
}
