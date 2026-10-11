//! 日历本地服务：数据库的唯一写入者。
//!
//! 并发模型（用 Java 的话说：一个 `ServerSocket.accept()` 循环 + 每连接一个线程，
//! 所有线程通过 `synchronized(store)` 串行访问同一个 `Connection`）：
//! - 读取请求帧、解析发生在锁外，所以一个慢客户端不会拖住其他调用方；
//! - 真正触碰 SQLite 的部分持有互斥锁，保证单写入者语义不变；
//! - 主线程用 `poll(2)` 带 5 秒超时等待新连接：空闲时几乎不占 CPU（旧实现每 10ms 轮询一次），
//!   空闲退出的判断与 `accept` 在同一个线程里，不存在「刚决定退出又来了请求」的竞态。
use daygrid::{
    Error, VERSION,
    ipc::{self, Request},
    logging,
    store::Store,
    success,
};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const IDLE_EXIT: Duration = Duration::from_secs(300);
const SLOW_REQUEST_MS: u128 = 200;

struct Shared {
    store: Mutex<Store>,
    started: Instant,
    /// 最近一次活动距启动的毫秒数。
    last_activity_ms: AtomicU64,
    inflight: AtomicUsize,
}

impl Shared {
    fn touch(&self) {
        self.last_activity_ms
            .store(self.started.elapsed().as_millis() as u64, Ordering::Relaxed);
    }
    fn idle_for(&self) -> Duration {
        let now = self.started.elapsed().as_millis() as u64;
        Duration::from_millis(now.saturating_sub(self.last_activity_ms.load(Ordering::Relaxed)))
    }
}

fn main() {
    let dir = match ipc::data_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("{}: {}", e.code, e.message);
            std::process::exit(1)
        }
    };
    let _logs = logging::init(&dir, "service", false);
    if let Err(e) = run(dir) {
        tracing::error!(code = e.code, "service failed: {}", e.message);
        drop(_logs);
        eprintln!("{}: {}", e.code, e.message);
        std::process::exit(1)
    }
}

fn run(dir: PathBuf) -> daygrid::Result<()> {
    ipc::prepare(&dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join("service.lock"))
        .map_err(|_| Error::new("SERVICE_UNAVAILABLE", "无法锁定服务目录。"))?;
    // 短暂等待锁：旧实例正在空闲退出时，新实例可以无缝接手；
    // 已经有健康实例在运行时，等待超时后安静退出（这是正常的竞争结果，不是错误）。
    let lock_deadline = Instant::now() + Duration::from_secs(3);
    while lock.try_lock_exclusive().is_err() {
        if Instant::now() >= lock_deadline {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let token = ipc::secret(&dir, "desktop.key")?;
    let db_path = dir.join("calendar.db");
    let mut store = Store::open(&db_path, &token)?;
    fs::set_permissions(&db_path, fs::Permissions::from_mode(0o600))
        .map_err(|_| Error::new("STORAGE_ERROR", "无法设置数据权限。"))?;
    match store.auto_backup() {
        Ok(Some(name)) => tracing::info!(%name, "automatic daily backup created"),
        Ok(None) => {}
        Err(e) => tracing::warn!(code = e.code, "automatic backup failed: {}", e.message),
    }
    let socket_path = dir.join("service.sock");
    let _ = fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)
        .map_err(|_| Error::new("SERVICE_UNAVAILABLE", "无法创建本地通信入口。"))?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))
        .map_err(|_| Error::new("SERVICE_UNAVAILABLE", "无法设置通信权限。"))?;
    tracing::info!(
        version = VERSION,
        pid = std::process::id(),
        "service started"
    );

    let shared = Arc::new(Shared {
        store: Mutex::new(store),
        started: Instant::now(),
        last_activity_ms: AtomicU64::new(0),
        inflight: AtomicUsize::new(0),
    });

    loop {
        if !wait_readable(&listener, 5000) {
            if shared.inflight.load(Ordering::Relaxed) == 0 && shared.idle_for() >= IDLE_EXIT {
                break;
            }
            continue;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                shared.touch();
                shared.inflight.fetch_add(1, Ordering::Relaxed);
                let shared = shared.clone();
                std::thread::spawn(move || {
                    serve(&shared, stream);
                    shared.touch();
                    shared.inflight.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "accept failed");
                break;
            }
        }
    }
    tracing::info!(
        uptime_s = shared.started.elapsed().as_secs(),
        "service stopped (idle)"
    );
    // 先摘掉 socket 文件再释放锁：此后新来的客户端会走「拉起新服务」分支，
    // 新服务会等到这把锁释放后接手。
    let _ = fs::remove_file(socket_path);
    drop(listener);
    drop(lock);
    Ok(())
}

/// 等待监听 socket 可读；超时或被信号打断返回 false。
fn wait_readable(listener: &UnixListener, timeout_ms: i32) -> bool {
    let mut pfd = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: pfd 是有效的单元素数组，fd 在调用期间由 listener 持有。
    let n = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    n > 0 && pfd.revents & libc::POLLIN != 0
}

fn serve(shared: &Shared, mut stream: UnixStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let mut line = String::new();
    let read = BufReader::new(&stream)
        .take(ipc::MAX_FRAME + 1)
        .read_line(&mut line);
    let started = Instant::now();
    let mut label = "invalid".to_string();
    let value = if read.is_err() || line.len() as u64 > ipc::MAX_FRAME || !line.ends_with('\n') {
        Error::new("VALIDATION_ERROR", "请求格式错误或超过 1MiB。").value()
    } else {
        match serde_json::from_str::<Request>(&line) {
            Ok(r) if r.ipc_version != ipc::IPC_VERSION => {
                Error::new("VERSION_UNSUPPORTED", "IPC 版本不匹配。").value()
            }
            Ok(r) if r.method == "ping" => {
                label = "ping".into();
                success(
                    json!({"ipc_version":ipc::IPC_VERSION,"service_version":VERSION,"schema_version":daygrid::store::SCHEMA_VERSION}),
                )
            }
            Ok(r) => {
                label = format!(
                    "{} {}",
                    if r.actor == "desktop" {
                        "desktop".to_string()
                    } else {
                        // 连接 ID 只取前 8 位：足够区分，又不把完整 ID 写进日志。
                        format!("conn:{}", r.actor.chars().take(8).collect::<String>())
                    },
                    r.method
                );
                // 锁中毒（某个线程曾在持锁时 panic）时仍继续服务：
                // SQLite 事务在 panic 展开时已回滚，数据是一致的。
                let mut store = shared.store.lock().unwrap_or_else(|e| e.into_inner());
                store.call(&r.actor, &r.token, &r.method, r.args)
            }
            Err(_) => Error::new("VALIDATION_ERROR", "无法解析请求。").value(),
        }
    };
    let _ = writeln!(stream, "{value}");
    let ms = started.elapsed().as_millis();
    let code = value["error"]["code"].as_str().unwrap_or("-");
    log_request(&label, ms, &value, code);
}

fn log_request(label: &str, ms: u128, value: &Value, code: &str) {
    if label == "ping" {
        tracing::debug!(ms, "ping");
    } else if value["ok"] != true {
        tracing::warn!(ms, code, "{label} failed");
    } else if ms >= SLOW_REQUEST_MS {
        tracing::warn!(ms, "{label} slow");
    } else {
        tracing::debug!(ms, "{label} ok");
    }
}
