pub mod clients;
pub mod domain;
pub mod ipc;
pub mod logging;
pub mod reminder;
pub mod store;
pub mod sync;
pub mod tools;

use serde_json::{Value, json};

/// 对外版本：同时写进 IPC 握手、MCP serverInfo 与诊断信息。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
    pub details: Value,
}

impl Error {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: json!({}),
        }
    }
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
    /// 给调用方（尤其是 AI 助手）的恢复建议。写给模型看，所以用英文。
    pub fn hint(&self) -> Option<&'static str> {
        Some(match self.code {
            "CALENDAR_REPLACED" => {
                "Call daygrid_get_context to read the current calendar_generation, then retry."
            }
            "REVISION_CONFLICT" => {
                "details.current_item holds the latest content. Re-decide based on it, then retry with a NEW idempotency_key and the current revision."
            }
            "CURSOR_STALE" => {
                "The calendar changed during pagination. Restart from the first page without a cursor."
            }
            "IDEMPOTENCY_CONFLICT" => {
                "An idempotency_key may only be reused for an identical request. Generate a new UUID for a different request."
            }
            "PERMISSION_DENIED" => {
                "This connection may be read-only, paused or revoked. Ask the user to check Daygrid > Assistants."
            }
            "NOT_FOUND" => {
                "The item may have been deleted. Use daygrid_list_items to find it again."
            }
            "PROPOSAL_EXPIRED" => {
                "The user did not respond in time. Submit a new request if still needed."
            }
            "SERVICE_UNAVAILABLE" => {
                "Transient failure; retrying the same call (same idempotency_key) is safe."
            }
            _ => return None,
        })
    }
    pub fn value(&self) -> Value {
        let mut error = json!({
            "code":self.code,"message":self.message,"details":self.details,
            "retryable":matches!(self.code,"SERVICE_UNAVAILABLE"|"STORAGE_ERROR")
        });
        if let Some(hint) = self.hint() {
            error["hint"] = json!(hint);
        }
        json!({"schema_version":1,"ok":false,"error":error})
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        // 内容可能含 SQL 细节，只进日志，不返回给调用方。
        tracing::error!(error = %e, "sqlite error");
        Self::new("STORAGE_ERROR", "本地存储操作失败，尚未确认保存。")
    }
}

pub type Result<T> = std::result::Result<T, Error>;
pub fn success(data: Value) -> Value {
    json!({"schema_version":1,"ok":true,"data":data})
}
