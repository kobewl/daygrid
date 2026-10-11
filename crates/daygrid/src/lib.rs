pub mod domain;
pub mod ipc;
pub mod store;

use serde_json::{Value, json};

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
    pub fn value(&self) -> Value {
        json!({"schema_version":1,"ok":false,"error":{
            "code":self.code,"message":self.message,"details":self.details,
            "retryable":matches!(self.code,"SERVICE_UNAVAILABLE"|"STORAGE_ERROR")
        }})
    }
}

impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::new("STORAGE_ERROR", "本地存储操作失败，尚未确认保存。")
    }
}

pub type Result<T> = std::result::Result<T, Error>;
pub fn success(data: Value) -> Value {
    json!({"schema_version":1,"ok":true,"data":data})
}
