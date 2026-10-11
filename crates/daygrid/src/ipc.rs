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
    sync::Mutex,
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
    let socket = UnixStream::connect(dir.join("service.sock")).map_err(io_error)?;
    exchange_on(socket, request)
}
fn exchange_on(mut socket: UnixStream, request: &Request) -> Result<Value> {
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
/// 版本不一致时（典型场景：升级应用后，旧版本服务还在空闲等待退出），
/// 终止旧服务，让新版本按需重新拉起。通过 socket 对端 PID 定位进程，
/// 只会作用于确实在这个数据目录的 socket 上应答过 Daygrid 握手的进程。
fn stop_service(dir: &Path) -> bool {
    let socket_path = dir.join("service.sock");
    let Ok(socket) = UnixStream::connect(&socket_path) else {
        return true;
    };
    let pid = peer_pid(&socket);
    drop(socket);
    let Some(pid) = pid else { return false };
    tracing::warn!(pid, "stopping stale daygrid-service from another version");
    // SAFETY: 向一个已确认的本用户进程发送 SIGTERM；失败只会让下面的等待超时。
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if UnixStream::connect(&socket_path).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    false
}
#[cfg(target_os = "macos")]
fn peer_pid(socket: &UnixStream) -> Option<i32> {
    use std::os::fd::AsRawFd;
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SOL_LOCAL = 0, LOCAL_PEERPID = 2（<sys/un.h>）
    // SAFETY: 缓冲区与长度均有效，仅读取对端 PID。
    let r = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            0,
            2,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut len,
        )
    };
    (r == 0 && pid > 0).then_some(pid)
}
#[cfg(target_os = "linux")]
fn peer_pid(socket: &UnixStream) -> Option<i32> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: 缓冲区与长度均有效，仅读取对端凭据。
    let r = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    (r == 0 && cred.pid > 0).then_some(cred.pid)
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn peer_pid(_: &UnixStream) -> Option<i32> {
    None
}

pub fn ensure_service(dir: &Path) -> Result<()> {
    let compatible = |v: &Value| -> bool {
        v["ok"] == true
            && v["data"]["ipc_version"] == IPC_VERSION
            && v["data"]["service_version"] == crate::VERSION
    };
    let incompatible = || {
        Error::new(
            "VERSION_UNSUPPORTED",
            "旧版本的日历服务仍在运行且无法自动停止，请退出日格后重试。",
        )
    };
    if let Ok(v) = ping(dir) {
        if compatible(&v) {
            return Ok(());
        }
        if !stop_service(dir) {
            return Err(incompatible());
        }
    }
    prepare(dir)?;
    let exe = std::env::current_exe().map_err(io_error)?;
    let service = exe.parent().unwrap().join("daygrid-service");
    // 服务自己写日志文件；stdout/stderr 全部丢弃，保证不会污染 MCP 的 stdio。
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
            return if compatible(&v) {
                Ok(())
            } else {
                Err(incompatible())
            };
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(Error::new(
        "SERVICE_UNAVAILABLE",
        "日历服务启动失败，请检查安装和数据目录权限，或查看日志目录。",
    ))
}

/// 最近一次确认过「服务在运行且版本兼容」的目录与时间。
/// 之后 30 秒内的调用直接连接，省掉每次调用前的 ping 往返；
/// 连接失败（服务已退出）时回退到完整流程，自动拉起新服务。
static VERIFIED: Mutex<Option<(PathBuf, Instant)>> = Mutex::new(None);
const VERIFY_TTL: Duration = Duration::from_secs(30);

fn recently_verified(dir: &Path) -> bool {
    VERIFIED
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .is_some_and(|(d, at)| d == dir && at.elapsed() < VERIFY_TTL)
}
fn set_verified(dir: &Path, ok: bool) {
    if let Ok(mut g) = VERIFIED.lock() {
        *g = ok.then(|| (dir.to_path_buf(), Instant::now()));
    }
}

pub fn call(dir: &Path, actor: &str, token: &str, method: &str, args: Value) -> Value {
    let request = Request {
        ipc_version: IPC_VERSION,
        actor: actor.into(),
        token: token.into(),
        method: method.into(),
        args,
    };
    (|| {
        if recently_verified(dir) {
            // 只有「连不上」才重试：连上之后的超时不能重发，否则非幂等的桌面操作会重复执行。
            match UnixStream::connect(dir.join("service.sock")) {
                Ok(socket) => return exchange_on(socket, &request),
                Err(_) => set_verified(dir, false),
            }
        }
        ensure_service(dir)?;
        set_verified(dir, true);
        exchange(dir, &request)
    })()
    .unwrap_or_else(|e: Error| e.value())
}
