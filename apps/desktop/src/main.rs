#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use daygrid::{Error, ipc};
use serde_json::{Value, json};

#[tauri::command]
async fn calendar_call(method: String, args: Value) -> Value {
    match tauri::async_runtime::spawn_blocking(move || dispatch(&method, args)).await {
        Ok(value) => value,
        Err(_) => Error::new("SERVICE_UNAVAILABLE", "日历服务任务失败。").value(),
    }
}
fn dispatch(method: &str, args: Value) -> Value {
    if !matches!(
        method,
        "daygrid_get_context"
            | "daygrid_list_items"
            | "daygrid_get_item"
            | "daygrid_create_item"
            | "daygrid_update_item"
            | "daygrid_set_item_status"
            | "desktop_delete"
            | "desktop_restore"
            | "desktop_history"
            | "desktop_connections"
            | "desktop_create_connection"
            | "desktop_set_connection"
    ) {
        return Error::new("PERMISSION_DENIED", "不允许的桌面操作。").value();
    }
    let result = (|| {
        let dir = ipc::data_dir()?;
        let token = ipc::secret(&dir, "desktop.key")?;
        let mut value = ipc::call(&dir, "desktop", &token, method, args);
        if method == "desktop_create_connection" && value["ok"] == true {
            let id = value["data"]["id"].as_str().unwrap();
            let credential = value["data"]["credential"].as_str().unwrap();
            if let Err(e) = ipc::write_credential(&dir, id, credential) {
                ipc::call(
                    &dir,
                    "desktop",
                    &token,
                    "desktop_set_connection",
                    json!({"id":id,"state":"revoked"}),
                );
                return Err(e);
            }
            value["data"] = json!({"id":id,"config":connection_config(id)});
        }
        if method == "desktop_connections"
            && value["ok"] == true
            && let Some(connections) = value["data"]["connections"].as_array_mut()
        {
            for connection in connections {
                connection["config"] = connection_config(connection["id"].as_str().unwrap());
            }
        }
        Ok(value)
    })();
    result.unwrap_or_else(|e: Error| e.value())
}
fn connection_config(id: &str) -> Value {
    let exe = std::env::current_exe().expect("application executable path");
    json!({"mcpServers":{"daygrid":{"command":exe.parent().unwrap().join("daygrid-mcp"),"args":["--connection",id]}}})
}
fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![calendar_call])
        .run(tauri::generate_context!())
        .expect("Daygrid desktop failed to start");
}
