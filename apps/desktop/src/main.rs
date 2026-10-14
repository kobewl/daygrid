#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod update;

use daygrid::{Error, VERSION, clients, ipc, logging};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Instant,
};

/// 桌面通道的根令牌：每个进程只读一次文件。
static TOKEN: OnceLock<(PathBuf, String)> = OnceLock::new();

#[tauri::command]
async fn calendar_call(method: String, args: Value) -> Value {
    match tauri::async_runtime::spawn_blocking(move || dispatch(&method, args)).await {
        Ok(value) => value,
        Err(_) => Error::new("SERVICE_UNAVAILABLE", "日历服务任务失败。").value(),
    }
}

/// 有新的待确认请求而窗口不在前台时，让 Dock 图标跳动一下。
#[tauri::command]
fn app_attention(window: tauri::WebviewWindow) {
    let _ = window.request_user_attention(Some(tauri::UserAttentionType::Informational));
}

/// 备注里的链接用系统默认浏览器 / 邮件客户端打开。只放行 http、https、mailto，
/// 且必须以协议开头（不会被 `open` 当成命令行选项），不含空白与控制字符。
fn external_url(url: &str) -> Result<&str, Error> {
    let lower = url.to_ascii_lowercase();
    let allowed = ["http://", "https://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme));
    let clean = !url.chars().any(|c| c.is_whitespace() || c.is_control());
    if allowed && clean && url.len() <= 2048 {
        Ok(url)
    } else {
        Err(Error::new(
            "VALIDATION_ERROR",
            "只能打开 http、https 或 mailto 链接。",
        ))
    }
}

#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    let url = external_url(&url).map_err(|e| e.message)?;
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(target_os = "macos"))]
    let opener = "xdg-open";
    std::process::Command::new(opener)
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|_| "无法打开该链接。".to_string())
}

const SERVICE_METHODS: [&str; 32] = [
    "daygrid_get_context",
    "daygrid_list_items",
    "daygrid_get_item",
    "daygrid_create_item",
    "daygrid_update_item",
    "daygrid_set_item_status",
    "daygrid_month_overview",
    "desktop_delete",
    "desktop_restore",
    "desktop_set_visibility",
    "desktop_history",
    "desktop_connections",
    "desktop_create_connection",
    "desktop_set_connection",
    "desktop_activity",
    "desktop_proposals",
    "desktop_decide_proposal",
    "desktop_search",
    "desktop_pulse",
    "desktop_diagnostics",
    "desktop_backup",
    "desktop_get_settings",
    "desktop_set_settings",
    "desktop_list_day_marks",
    "desktop_set_day_mark",
    "desktop_sync_status",
    "desktop_sync_connect",
    "desktop_sync_disconnect",
    "desktop_sync_presence",
    "desktop_sync_conflicts",
    "desktop_sync_resolve_conflict",
    "desktop_sync_invite",
];

fn dispatch(method: &str, args: Value) -> Value {
    let started = Instant::now();
    let value = route(method, args).unwrap_or_else(|e| e.value());
    let ms = started.elapsed().as_millis();
    if value["ok"] != true {
        tracing::warn!(
            method,
            ms,
            code = value["error"]["code"].as_str().unwrap_or("-"),
            "call failed"
        );
    } else if ms >= 200 {
        tracing::warn!(method, ms, "call slow");
    } else {
        tracing::debug!(method, ms, "call ok");
    }
    value
}

fn route(method: &str, args: Value) -> Result<Value, Error> {
    let dir = ipc::data_dir()?;
    match method {
        // 这几个只涉及本机文件，不经过日历服务。
        "desktop_app_info" => return Ok(daygrid::success(app_info(&dir))),
        "desktop_reveal" => return reveal(&dir, &args),
        "desktop_client_status" => {
            let id = str_arg(&args, "connection_id")?;
            daygrid::domain::uuid(id)?;
            return Ok(daygrid::success(
                json!({"clients":clients::status(&home()?, id, &mcp_exe())}),
            ));
        }
        "desktop_client_install" => {
            let data = clients::install(
                &home()?,
                str_arg(&args, "client")?,
                str_arg(&args, "connection_id")?,
                &mcp_exe(),
            )?;
            return Ok(daygrid::success(data));
        }
        "desktop_client_uninstall" => {
            return Ok(daygrid::success(clients::uninstall(
                &home()?,
                str_arg(&args, "client")?,
            )?));
        }
        _ => {}
    }
    if !SERVICE_METHODS.contains(&method) {
        return Err(Error::new("PERMISSION_DENIED", "不允许的桌面操作。"));
    }
    let token = TOKEN.get_or_init(|| {
        let token = ipc::secret(&dir, "desktop.key").unwrap_or_default();
        (dir.clone(), token)
    });
    let token = token.1.as_str();
    let mut value = ipc::call(&dir, "desktop", token, method, args);
    if method == "desktop_create_connection" && value["ok"] == true {
        let id = value["data"]["id"].as_str().unwrap_or_default().to_string();
        let credential = value["data"]["credential"].as_str().unwrap_or_default();
        if let Err(e) = ipc::write_credential(&dir, &id, credential) {
            ipc::call(
                &dir,
                "desktop",
                token,
                "desktop_set_connection",
                json!({"id":id,"state":"revoked"}),
            );
            return Err(e);
        }
        value["data"] = json!({"id":id,"config":connection_config(&id)});
    }
    if method == "desktop_connections"
        && value["ok"] == true
        && let Some(connections) = value["data"]["connections"].as_array_mut()
    {
        for connection in connections {
            let id = connection["id"].as_str().unwrap_or_default().to_string();
            connection["config"] = connection_config(&id);
        }
    }
    Ok(value)
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, Error> {
    args[key]
        .as_str()
        .ok_or_else(|| Error::new("VALIDATION_ERROR", "缺少参数。"))
}
fn home() -> Result<PathBuf, Error> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error::new("SERVICE_UNAVAILABLE", "无法找到用户目录。"))
}
fn mcp_exe() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.join("daygrid-mcp")))
        .unwrap_or_else(|| PathBuf::from("daygrid-mcp"))
}
fn connection_config(id: &str) -> Value {
    json!({"mcpServers":{"daygrid":{"command":mcp_exe(),"args":["--connection",id]}}})
}
fn app_info(dir: &Path) -> Value {
    json!({
        "app_version":VERSION,
        "platform":std::env::consts::OS,
        "arch":std::env::consts::ARCH,
        "data_dir":dir.display().to_string(),
        "log_dir":logging::logs_dir(dir).display().to_string(),
        "mcp_path":mcp_exe().display().to_string(),
    })
}

/// 在访达里打开数据、日志或备份目录。只接受固定的几个目标，不接受任意路径。
fn reveal(dir: &Path, args: &Value) -> Result<Value, Error> {
    let target = match str_arg(args, "target")? {
        "data" => dir.to_path_buf(),
        "logs" => logging::logs_dir(dir),
        "backups" => dir.join("backups"),
        _ => return Err(Error::new("VALIDATION_ERROR", "未知的目录。")),
    };
    ipc::prepare(&target)?;
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(target_os = "macos"))]
    let opener = "xdg-open";
    std::process::Command::new(opener)
        .arg(&target)
        .spawn()
        .map_err(|_| Error::new("SERVICE_UNAVAILABLE", "无法打开该目录。"))?;
    Ok(daygrid::success(
        json!({"path":target.display().to_string()}),
    ))
}

fn main() {
    let _logs = ipc::data_dir()
        .ok()
        .map(|dir| logging::init(&dir, "desktop", false));
    tracing::info!(version = VERSION, "desktop started");
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(update::UpdateState::default())
        .invoke_handler(tauri::generate_handler![
            calendar_call,
            app_attention,
            open_external,
            update::check_update,
            update::install_update,
            update::restart_after_update,
            update::open_release_page
        ])
        .run(tauri::generate_context!())
        .expect("Daygrid desktop failed to start");
}

#[cfg(test)]
mod tests {
    use super::external_url;

    #[test]
    fn only_web_and_mail_links_are_opened() {
        for ok in [
            "https://leetcode.cn/problems/fibonacci-number/",
            "http://example.com/a?b=1&c=%E4%B8%AD#x",
            "HTTPS://EXAMPLE.COM",
            "mailto:me@example.com",
        ] {
            assert_eq!(external_url(ok).unwrap(), ok);
        }
        for bad in [
            "",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "-a Calculator",
            "/Applications/Calculator.app",
            "https://a.com/ b",
            "https://a.com/\n",
            "ftp://example.com",
            "tel:123",
        ] {
            assert!(external_url(bad).is_err(), "{bad:?}");
        }
        assert!(external_url(&format!("https://a.com/{}", "a".repeat(2100))).is_err());
    }
}
