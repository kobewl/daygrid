use daygrid::{
    Error,
    ipc::{self, Request},
    store::Store,
    success,
};
use fs2::FileExt;
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::UnixListener,
    },
    time::{Duration, Instant},
};

fn main() {
    if let Err(e) = run() {
        eprintln!("{}: {}", e.code, e.message);
        std::process::exit(1)
    }
}
fn run() -> daygrid::Result<()> {
    let dir = ipc::data_dir()?;
    ipc::prepare(&dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join("service.lock"))
        .map_err(|_| Error::new("SERVICE_UNAVAILABLE", "无法锁定服务目录。"))?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(());
    }
    let token = ipc::secret(&dir, "desktop.key")?;
    let mut store = Store::open(&dir.join("calendar.db"), &token)?;
    fs::set_permissions(dir.join("calendar.db"), fs::Permissions::from_mode(0o600))
        .map_err(|_| Error::new("STORAGE_ERROR", "无法设置数据权限。"))?;
    let socket = dir.join("service.sock");
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)
        .map_err(|_| Error::new("SERVICE_UNAVAILABLE", "无法创建本地通信入口。"))?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
        .map_err(|_| Error::new("SERVICE_UNAVAILABLE", "无法设置通信权限。"))?;
    listener
        .set_nonblocking(true)
        .map_err(|_| Error::new("SERVICE_UNAVAILABLE", "无法监听服务。"))?;
    let mut last = Instant::now();
    while last.elapsed() < Duration::from_secs(300) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                last = Instant::now();
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                let mut line = String::new();
                let read = BufReader::new(&stream)
                    .take(ipc::MAX_FRAME + 1)
                    .read_line(&mut line);
                let value = if read.is_err()
                    || line.len() as u64 > ipc::MAX_FRAME
                    || !line.ends_with('\n')
                {
                    Error::new("VALIDATION_ERROR", "请求格式错误或超过 1MiB。").value()
                } else {
                    match serde_json::from_str::<Request>(&line) {
                        Ok(r) if r.ipc_version != ipc::IPC_VERSION => {
                            Error::new("VERSION_UNSUPPORTED", "IPC 版本不匹配。").value()
                        }
                        Ok(r) if r.method == "ping" => success(
                            json!({"ipc_version":ipc::IPC_VERSION,"service_version":env!("CARGO_PKG_VERSION"),"schema_version":1}),
                        ),
                        Ok(r) => store.call(&r.actor, &r.token, &r.method, r.args),
                        Err(_) => Error::new("VALIDATION_ERROR", "无法解析请求。").value(),
                    }
                };
                let _ = writeln!(stream, "{value}");
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(_) => break,
        }
    }
    let _ = fs::remove_file(socket);
    drop(lock);
    Ok(())
}
