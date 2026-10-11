//! 结构化日志：写入数据目录下的 `logs/`，按天滚动，最多保留 7 个文件。
//!
//! 隐私约定：日志只记录方法名、耗时、结果码等元数据，**绝不**记录事项标题、
//! 备注或任何日历内容。排查问题时可以放心把日志文件发给别人。
//!
//! 级别由环境变量 `DAYGRID_LOG` 控制（语法同 `RUST_LOG`，例如 `debug` 或
//! `daygrid=trace`），默认 `info`。
use std::path::{Path, PathBuf};
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{Builder, Rotation},
};
use tracing_subscriber::{
    EnvFilter, Layer, filter::LevelFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt,
};

pub const KEEP_FILES: usize = 7;

/// 日志目录。
pub fn logs_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}

/// 持有后台写线程；进程退出前 drop 才能保证最后几行落盘。
pub struct LogGuard {
    _guards: Vec<WorkerGuard>,
}

/// 初始化日志。`name` 决定文件名前缀（`service`、`mcp`、`desktop`）。
/// `stderr_warnings` 为真时，警告及以上同时输出到 stderr（MCP 客户端会展示它）。
///
/// 任何失败都只会让日志静默降级，绝不影响主流程。
pub fn init(data_dir: &Path, name: &str, stderr_warnings: bool) -> LogGuard {
    let mut guards = Vec::new();
    let dir = logs_dir(data_dir);
    let file_layer = crate::ipc::prepare(&dir).ok().and_then(|_| {
        Builder::new()
            .rotation(Rotation::DAILY)
            .filename_prefix(name)
            .filename_suffix("log")
            .max_log_files(KEEP_FILES)
            .build(&dir)
            .ok()
    });
    let filter =
        || EnvFilter::try_from_env("DAYGRID_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let file = file_layer.map(|appender| {
        let (writer, guard) = tracing_appender::non_blocking(appender);
        guards.push(guard);
        fmt::layer()
            .with_ansi(false)
            .with_target(false)
            .with_writer(writer)
            .with_filter(filter())
    });
    let stderr = stderr_warnings.then(|| {
        fmt::layer()
            .with_ansi(false)
            .with_target(false)
            .without_time()
            .with_writer(std::io::stderr)
            .with_filter(LevelFilter::WARN)
    });
    // 已经初始化过（例如测试里重复调用）时 try_init 返回错误，直接忽略。
    let _ = tracing_subscriber::registry()
        .with(file)
        .with(stderr)
        .try_init();
    let panic_name = name.to_string();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(process = %panic_name, "panic: {info}");
    }));
    LogGuard { _guards: guards }
}
