use crate::{Error, Result};
use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use uuid::Uuid;

pub const COLORS: [&str; 6] = ["blue", "sage", "amber", "rose", "violet", "gray"];
pub const STATUSES: [&str; 2] = ["planned", "done"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub id: String,
    pub date: String,
    pub title: String,
    pub notes: String,
    pub color: String,
    pub status: String,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
    /// `private`：只在这台电脑上，永远不会上传；`shared`：团队日历里所有成员可见。
    /// 没有连接协同时所有事项都只在本机，这个字段只在协同时才有意义；旧数据缺省为 `private`。
    #[serde(default = "private")]
    pub visibility: String,
}

pub const VISIBILITIES: [&str; 2] = ["private", "shared"];
pub fn private() -> String {
    "private".into()
}

pub fn decode<T: DeserializeOwned>(value: &Value) -> Result<T> {
    fn has_null(v: &Value) -> bool {
        match v {
            Value::Null => true,
            Value::Object(m) => m.values().any(has_null),
            Value::Array(a) => a.iter().any(has_null),
            _ => false,
        }
    }
    if has_null(value) {
        return Err(Error::new("VALIDATION_ERROR", "字段不能为 null。"));
    }
    serde_json::from_value(value.clone())
        .map_err(|_| Error::new("VALIDATION_ERROR", "参数类型错误、缺少字段或包含未知字段。"))
}
pub fn uuid(value: &str) -> Result<()> {
    Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| Error::new("VALIDATION_ERROR", "ID 必须为 UUID。"))
}
pub fn date(value: &str) -> Result<NaiveDate> {
    let d = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| Error::new("VALIDATION_ERROR", "请选择合法日期。"))?;
    if !(1900..=2100).contains(&d.year()) || d.format("%Y-%m-%d").to_string() != value {
        return Err(Error::new(
            "VALIDATION_ERROR",
            "日期范围为 1900–2100 年，格式为 YYYY-MM-DD。",
        ));
    }
    Ok(d)
}
pub fn validate(item: &mut Item) -> Result<()> {
    date(&item.date)?;
    item.title = item.title.trim().to_string();
    if item.title.is_empty()
        || item.title.chars().count() > 200
        || item.notes.chars().count() > 10000
    {
        return Err(Error::new(
            "VALIDATION_ERROR",
            "标题需要 1–200 个字符，备注最多 10000 个字符。",
        ));
    }
    if !COLORS.contains(&item.color.as_str()) || !STATUSES.contains(&item.status.as_str()) {
        return Err(Error::new("VALIDATION_ERROR", "颜色或状态无效。"));
    }
    if !VISIBILITIES.contains(&item.visibility.as_str()) {
        return Err(Error::new("VALIDATION_ERROR", "可见范围无效。"));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub idempotency_key: String,
    pub calendar_generation: String,
    pub date: String,
    pub title: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default = "blue")]
    pub color: String,
    /// 只有日格界面能指定；助手创建的事项一律是 `private`。
    #[serde(default)]
    pub visibility: Option<String>,
}
fn blue() -> String {
    "blue".into()
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub date: Option<String>,
    pub title: Option<String>,
    pub notes: Option<String>,
    pub color: Option<String>,
}
impl Patch {
    pub fn apply(self, item: &mut Item) -> Result<()> {
        if self.date.is_none()
            && self.title.is_none()
            && self.notes.is_none()
            && self.color.is_none()
        {
            return Err(Error::new("VALIDATION_ERROR", "修改至少需要一个字段。"));
        }
        if let Some(v) = self.date {
            item.date = v
        }
        if let Some(v) = self.title {
            item.title = v
        }
        if let Some(v) = self.notes {
            item.notes = v
        }
        if let Some(v) = self.color {
            item.color = v
        }
        validate(item)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub idempotency_key: String,
    pub calendar_generation: String,
    pub item_id: String,
    pub expected_revision: i64,
    pub patch: Patch,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub idempotency_key: String,
    pub calendar_generation: String,
    pub item_id: String,
    pub expected_revision: i64,
    pub status: String,
}
/// 改变可见范围（只对桌面界面开放）。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetVisibility {
    pub idempotency_key: String,
    pub calendar_generation: String,
    pub item_id: String,
    pub expected_revision: i64,
    pub visibility: String,
}
/// MCP 删除：必须经用户在日格里确认。`reason` 会展示给用户，帮助判断是否批准。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delete {
    pub idempotency_key: String,
    pub calendar_generation: String,
    pub item_id: String,
    pub expected_revision: i64,
    pub reason: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overview {
    pub from_date: String,
    pub to_date: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalRef {
    pub proposal_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Search {
    pub query: String,
    pub limit: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub proposal_id: String,
    pub decision: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lifecycle {
    pub idempotency_key: String,
    pub calendar_generation: String,
    pub item_id: String,
    pub expected_revision: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct List {
    pub from_date: String,
    pub to_date: String,
    pub query: Option<String>,
    pub color: Option<String>,
    pub status: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Get {
    pub item_id: String,
}
