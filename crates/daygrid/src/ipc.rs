use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const IPC_VERSION: u32 = 1;
pub const MAX_FRAME: u64 = 1024 * 1024;
pub const MAX_RESPONSE: u64 = 16 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub ipc_version: u32,
    pub actor: String,
    pub token: String,
    pub method: String,
    pub args: Value,
}
pub fn data_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("DAYGRID_DATA_DIR") {
        return Ok(path.into());
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| Error::new("SERVICE_UNAVAILABLE", "无法找到用户目录。"))?;
    #[cfg(target_os = "macos")]
    let suffix = "Library/Application Support/Daygrid";
    #[cfg(not(target_os = "macos"))]
    let suffix = ".local/share/daygrid";
    Ok(PathBuf::from(home).join(suffix))
}
pub fn prepare(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).map_err(io_error)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(io_error)
}
fn io_error(_: std::io::Error) -> Error {
    Error::new(
        "SERVICE_UNAVAILABLE",
        "日历服务无法连接或本地文件无法访问。",
    )
}
pub fn secret(dir: &Path, name: &str) -> Result<String> {
    prepare(dir)?;
    let path = dir.join(name);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut f) => {
            let token = format!("{}{}", Uuid::new_v4(), Uuid::new_v4());
            f.write_all(token.as_bytes()).map_err(io_error)?;
            f.sync_all().map_err(io_error)?;
            Ok(token)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            fs::read_to_string(path).map_err(io_error)
        }
        Err(e) => Err(io_error(e)),
    }
}
pub fn write_credential(dir: &Path, id: &str, token: &str) -> Result<()> {
    crate::domain::uuid(id)?;
    let folder = dir.join("connections");
    prepare(&folder)?;
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(folder.join(format!("{id}.key")))
        .map_err(io_error)?;
    f.write_all(token.as_bytes()).map_err(io_error)?;
    f.sync_all().map_err(io_error)
}
pub fn credential(dir: &Path, id: &str) -> Result<String> {
    crate::domain::uuid(id)?;
    fs::read_to_string(dir.join("connections").join(format!("{id}.key")))
        .map_err(|_| Error::new("PERMISSION_DENIED", "连接凭据不存在，请在日格中创建连接。"))
}
pub fn exchange(dir: &Path, request: &Request) -> Result<Value> {
    let mut socket = UnixStream::connect(dir.join("service.sock")).map_err(io_error)?;
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(io_error)?;
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(io_error)?;
    let mut bytes = serde_json::to_vec(request).unwrap();
    if bytes.len() as u64 > MAX_FRAME {
        return Err(Error::new("VALIDATION_ERROR", "请求超过 1MiB。"));
    }
    bytes.push(b'\n');
    socket.write_all(&bytes).map_err(io_error)?;
    let mut line = String::new();
    BufReader::new(socket)
        .take(MAX_RESPONSE + 1)
        .read_line(&mut line)
        .map_err(io_error)?;
    if line.len() as u64 > MAX_RESPONSE || !line.ends_with('\n') {
        return Err(io_error(std::io::Error::other("invalid response")));
    }
    serde_json::from_str(&line)
        .map_err(|_| Error::new("VERSION_UNSUPPORTED", "服务返回了不兼容的消息。"))
}
pub fn ping(dir: &Path) -> Result<Value> {
    exchange(
        dir,
        &Request {
            ipc_version: IPC_VERSION,
            actor: String::new(),
            token: String::new(),
            method: "ping".into(),
            args: json!({}),
        },
    )
}
pub fn ensure_service(dir: &Path) -> Result<()> {
    let compatible = |v: Value| -> Result<()> {
        if v["ok"] == true
            && v["data"]["ipc_version"] == IPC_VERSION
            && v["data"]["service_version"] == env!("CARGO_PKG_VERSION")
        {
            Ok(())
        } else {
            Err(Error::new(
                "VERSION_UNSUPPORTED",
                "请退出旧日历服务后重新启动应用。",
            ))
        }
    };
    if let Ok(v) = ping(dir) {
        return compatible(v);
    }
    prepare(dir)?;
    let exe = std::env::current_exe().map_err(io_error)?;
    let service = exe.parent().unwrap().join("daygrid-service");
    // Drain stdout completely away from MCP. Service logs contain no calendar content.
    let child = Command::new(service)
        .env("DAYGRID_DATA_DIR", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(io_error)?;
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(v) = ping(dir) {
            return compatible(v);
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    Err(Error::new(
        "SERVICE_UNAVAILABLE",
        "日历服务启动失败，请检查安装和数据目录权限。",
    ))
}
pub fn call(dir: &Path, actor: &str, token: &str, method: &str, args: Value) -> Value {
    (|| {
        ensure_service(dir)?;
        exchange(
            dir,
            &Request {
                ipc_version: IPC_VERSION,
                actor: actor.into(),
                token: token.into(),
                method: method.into(),
                args,
            },
        )
    })()
    .unwrap_or_else(|e: Error| e.value())
}
