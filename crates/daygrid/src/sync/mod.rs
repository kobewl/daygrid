//! 多人协同：协议类型、混合逻辑时钟、合并算法，以及客户端同步引擎。
//!
//! 设计与取舍见 Obsidian 笔记「协同编辑」目录（调研、ADR-001、协议设计）。
//! 一句话：**服务端权威 + 字段级合并**。每个事项是 `字段 → (值, HLC 时间戳)` 的小表，
//! 并发修改不同字段自动合并；同一字段先尝试三方合并（仅备注），再按 HLC 做 LWW，败者进冲突表。
pub mod client;
pub mod hlc;
pub mod merge;
pub mod runtime;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PROTOCOL_VERSION: u32 = 1;
/// 一次推送最多携带的操作数。
pub const MAX_PUSH_OPS: usize = 200;
/// 一次拉取最多返回的事项数。
pub const PULL_PAGE: usize = 500;
/// 服务端为每个事项保留的历史版本数（三方合并的"共同祖先"来源）。
pub const VERSIONS_KEEP: i64 = 50;

/// 参与同步与合并的字段。
pub const FIELDS: [&str; 5] = ["date", "title", "notes", "color", "status"];

/// 事项的五个可合并字段（全部是字符串，便于统一比较与合并）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ItemFields {
    pub date: String,
    pub title: String,
    pub notes: String,
    pub color: String,
    pub status: String,
}

impl ItemFields {
    pub fn get(&self, field: &str) -> Option<&str> {
        Some(match field {
            "date" => &self.date,
            "title" => &self.title,
            "notes" => &self.notes,
            "color" => &self.color,
            "status" => &self.status,
            _ => return None,
        })
    }
    pub fn set(&mut self, field: &str, value: String) -> bool {
        match field {
            "date" => self.date = value,
            "title" => self.title = value,
            "notes" => self.notes = value,
            "color" => self.color = value,
            "status" => self.status = value,
            _ => return false,
        }
        true
    }
}

impl From<&crate::domain::Item> for ItemFields {
    fn from(i: &crate::domain::Item) -> Self {
        Self {
            date: i.date.clone(),
            title: i.title.clone(),
            notes: i.notes.clone(),
            color: i.color.clone(),
            status: i.status.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpKind {
    Create,
    Update,
    Delete,
    Restore,
}

impl OpKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OpKind::Create => "create",
            OpKind::Update => "update",
            OpKind::Delete => "delete",
            OpKind::Restore => "restore",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "create" => OpKind::Create,
            "update" => OpKind::Update,
            "delete" => OpKind::Delete,
            "restore" => OpKind::Restore,
            _ => return None,
        })
    }
}

/// 一次字段修改：新值 + 修改发生时的 HLC。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldEdit {
    pub v: String,
    pub ts: String,
    /// 用户改之前这个字段在本机显示的值（"我是在什么基础上改的"），三方合并的共同祖先。
    /// 逐字段携带，所以排在同一事项后面的修改不会因为前一条被确认而丢失基准；
    /// 缺省时服务端退回用 `base_rev` 对应的历史版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<String>,
}

/// 客户端推送的一条变更（Replicache 里的 mutation）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Op {
    pub op_id: String,
    pub item_id: String,
    pub kind: OpKind,
    /// 客户端修改时已知的服务端版本；`create` 为 0。三方合并的"共同祖先"由它定位。
    pub base_rev: i64,
    pub ts: String,
    #[serde(default)]
    pub fields: BTreeMap<String, FieldEdit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

/// 服务端权威状态里的一个事项（push 结果与 pull 响应共用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerItem {
    pub id: String,
    pub rev: i64,
    pub deleted: bool,
    pub fields: ItemFields,
    pub created_at: String,
    pub updated_at: String,
    /// 最近一次修改者的显示名。
    pub updated_by: String,
    /// 导致本次版本的操作时间戳（客户端用它推进自己的 HLC）。
    pub ts: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
}

/// 冲突原因。
pub const REASON_CONCURRENT: &str = "concurrent_edit";
pub const REASON_DELETED: &str = "item_deleted";
pub const REASON_INVALID: &str = "invalid";
/// 角色被降级成只读时，尚未上传的本地修改被放弃。
pub const REASON_READ_ONLY: &str = "read_only";

/// 一条冲突记录：`lost_value` 是被覆盖/丢弃的值（JSON 字符串形式，便于直接展示与恢复）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conflict {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub item_id: String,
    pub field: String,
    pub kept_value: String,
    pub lost_value: String,
    /// 保留方的显示名。
    #[serde(default)]
    pub kept_by: String,
    pub reason: String,
    #[serde(default)]
    pub at: String,
    /// 仅本地生成的冲突使用：服务器拒绝的原因。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpStatus {
    Applied,
    Merged,
    Noop,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpResult {
    pub op_id: String,
    pub status: OpStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ServerItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<Conflict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<OpError>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clamped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushRequest {
    pub client_id: String,
    pub ops: Vec<Op>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushResponse {
    pub head_seq: i64,
    pub results: Vec<OpResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullResponse {
    pub head_seq: i64,
    pub next_since: i64,
    pub has_more: bool,
    pub items: Vec<ServerItem>,
    pub conflicts: Vec<Conflict>,
    pub conflict_head: i64,
    /// 请求者当前在这个团队日历里的角色（owner / editor / viewer）。
    /// 客户端每次拉取都据此校正本地角色：被降级、被升级都不必等到一次 403 才发现。
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub client_id: String,
    pub user: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub item_id: Option<String>,
    #[serde(default)]
    pub editing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinResponse {
    pub token: String,
    pub user: UserInfo,
    pub workspace: WorkspaceInfo,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeResponse {
    pub user: UserInfo,
    pub workspaces: Vec<MeWorkspace>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeWorkspace {
    pub id: String,
    pub name: String,
    pub role: String,
}

pub fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}
