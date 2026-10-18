use crate::{Error, Result};
use chrono::{
    DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc,
    offset::LocalResult,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
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
    /// 提醒时刻（日历时区、分钟精度、带偏移）；None = 不提醒。
    /// 不参与团队同步：它是本机的个人设置，和 visibility 一样只存在本机事项 JSON 里。
    #[serde(default)]
    pub remind_at: Option<String>,
}

pub const VISIBILITIES: [&str; 2] = ["private", "shared"];
pub fn private() -> String {
    "private".into()
}

pub fn decode<T: DeserializeOwned>(value: &Value) -> Result<T> {
    decode_nullable(value, &[])
}

/// 与 [`decode`] 相同，但 `allowed`（JSON Pointer 列表）指向的位置可以是 `null`。
pub fn decode_nullable<T: DeserializeOwned>(value: &Value, allowed: &[&str]) -> Result<T> {
    fn has_null(v: &Value, path: &mut String, allowed: &[&str]) -> bool {
        match v {
            Value::Null => !allowed.contains(&path.as_str()),
            Value::Object(m) => m.iter().any(|(k, v)| {
                let len = path.len();
                path.push('/');
                path.push_str(&k.replace('~', "~0").replace('/', "~1"));
                let found = has_null(v, path, allowed);
                path.truncate(len);
                found
            }),
            Value::Array(a) => a.iter().enumerate().any(|(i, v)| {
                let len = path.len();
                path.push('/');
                path.push_str(&i.to_string());
                let found = has_null(v, path, allowed);
                path.truncate(len);
                found
            }),
            _ => false,
        }
    }
    if has_null(value, &mut String::new(), allowed) {
        return Err(Error::new("VALIDATION_ERROR", "字段不能为 null。"));
    }
    serde_json::from_value(value.clone())
        .map_err(|_| Error::new("VALIDATION_ERROR", "参数类型错误、缺少字段或包含未知字段。"))
}
pub fn decode_create(v: &Value) -> Result<Create> {
    decode_nullable(v, &["/remind_at"])
}
pub fn decode_update(v: &Value) -> Result<Update> {
    decode_nullable(v, &["/patch/remind_at"])
}

/// 区分「没传」（外层 None，靠 #[serde(default)]）与「传了 null」（Some(None)）。
fn nullable<'de, D, T>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

// ---------- 提醒时间 ----------

/// 规范化后的提醒时刻：`text` 是 `YYYY-MM-DDTHH:MM:00±HH:MM`（日历时区），`epoch_ms` 是对应的 Unix 毫秒。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemindAt {
    pub text: String,
    pub epoch_ms: i64,
}

/// 写入时的时间上下文。`strict`：新的提醒时间不能早于 `now` 超过 60 秒。
#[derive(Debug, Clone, Copy)]
pub struct TimeCtx {
    pub tz: chrono_tz::Tz,
    pub now: DateTime<Utc>,
    pub strict: bool,
}

const REMIND_FORMAT: &str = "提醒时间格式无效，请使用 YYYY-MM-DDTHH:MM（日历时区）或带时区的 RFC 3339 时间，例如 2026-10-20T09:00 或 2026-10-20T09:00+08:00。";
const REMIND_RANGE: &str = "提醒时间需要在 1900–2100 年之间。";
const REMIND_GAP: &str = "这个时间在日历时区里不存在（夏令时切换），请换一个时间。";
const REMIND_PAST: &str = "提醒时间已经过去，请选择将来的时间。";

fn remind_error(message: &str, tz: chrono_tz::Tz, now: DateTime<Utc>) -> Error {
    Error::new("VALIDATION_ERROR", message).with_details(json!({
        "field":"remind_at","now":canonical_now(tz, now),"calendar_timezone":tz.name()
    }))
}

/// 日历时区下的当前时间，秒精度：`YYYY-MM-DDTHH:MM:SS±HH:MM`（不带小数）。
/// 只用于 context.now 和错误 details.now；remind_at 的规范形式仍是分钟精度（秒恒为 00）。
pub fn canonical_now(tz: chrono_tz::Tz, now: DateTime<Utc>) -> String {
    now.with_timezone(&tz)
        .format("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

fn canonical_remind<T: TimeZone>(dt: &DateTime<T>) -> String
where
    T::Offset: std::fmt::Display,
{
    dt.format("%Y-%m-%dT%H:%M:00%:z").to_string()
}

/// 解析用户 / 助手输入并规范化（规则见 design D5）。require_future：早于 now 超过 60 s 时报错。
pub fn parse_remind_at(
    input: &str,
    tz: chrono_tz::Tz,
    now: DateTime<Utc>,
    require_future: bool,
) -> Result<RemindAt> {
    let format = || remind_error(REMIND_FORMAT, tz, now);
    let s = input.trim();
    if s.is_empty() || s.chars().count() > 40 || !s.is_ascii() || s.len() < 16 {
        return Err(format());
    }
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    // YYYY-MM-DD(T|t| )HH:MM
    if !(digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && matches!(b[10], b'T' | b't' | b' ')
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16))
    {
        return Err(format());
    }
    let mut rest = &s[16..];
    // 可选的 :SS，以及跟在秒后面的 . 加 1–9 位小数。
    let mut seconds = "00";
    if let Some(r) = rest.strip_prefix(':') {
        if r.len() < 2 || !r.as_bytes()[..2].iter().all(u8::is_ascii_digit) {
            return Err(format());
        }
        seconds = &r[..2];
        rest = &r[2..];
        if let Some(r) = rest.strip_prefix('.') {
            let n = r.bytes().take_while(u8::is_ascii_digit).count();
            if !(1..=9).contains(&n) {
                return Err(format());
            }
            rest = &r[n..];
        }
    }
    let local = format!("{}T{}:{seconds}", &s[..10], &s[11..16]);
    // 秒和小数直接舍去：精确到分钟。
    let minute = |n: NaiveDateTime| n.with_second(0).and_then(|n| n.with_nanosecond(0));
    // 带偏移的输入在它自己的固定偏移下舍去秒（固定偏移没有歧义），再换到日历时区。
    let fixed = |s: &str| {
        let d = DateTime::parse_from_rfc3339(s).ok()?;
        Some(d.with_second(0)?.with_nanosecond(0)?.with_timezone(&tz))
    };
    let instant: DateTime<chrono_tz::Tz> = match rest {
        "" => {
            let naive = NaiveDateTime::parse_from_str(&local, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .and_then(minute)
                .ok_or_else(format)?;
            if !(1900..=2100).contains(&naive.year()) {
                return Err(remind_error(REMIND_RANGE, tz, now));
            }
            match tz.from_local_datetime(&naive) {
                LocalResult::None => return Err(remind_error(REMIND_GAP, tz, now)),
                // 夏令时回拨造成的歧义取较早的那个。
                r => r.earliest().ok_or_else(format)?,
            }
        }
        "Z" | "z" => fixed(&format!("{local}Z")).ok_or_else(format)?,
        off if off.len() == 6
            && matches!(off.as_bytes()[0], b'+' | b'-')
            && off.as_bytes()[3] == b':'
            && off.as_bytes()[1..3].iter().all(u8::is_ascii_digit)
            && off.as_bytes()[4..6].iter().all(u8::is_ascii_digit) =>
        {
            fixed(&format!("{local}{off}")).ok_or_else(format)?
        }
        _ => return Err(format()),
    };
    if !(1900..=2100).contains(&instant.year()) {
        return Err(remind_error(REMIND_RANGE, tz, now));
    }
    let text = canonical_remind(&instant);
    let epoch_ms = remind_epoch_ms(&text).ok_or_else(format)?;
    if require_future && epoch_ms < now.timestamp_millis() - crate::reminder::PAST_TOLERANCE_MS {
        return Err(remind_error(REMIND_PAST, tz, now));
    }
    Ok(RemindAt { text, epoch_ms })
}

/// 规范串对应的 Unix 毫秒；无法解析时返回 None。
pub fn remind_epoch_ms(text: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|d| d.timestamp_millis())
}

/// 事项日期从 old 改到 new 时，提醒平移相同天数（日历时区的墙上时间不变）。
/// 平移后的时段不存在（夏令时跳过）时往后顺延 1 小时。无法解析时返回 None。
pub fn shift_remind_at(
    remind_at: &str,
    old_date: &str,
    new_date: &str,
    tz: chrono_tz::Tz,
) -> Option<String> {
    let old = NaiveDate::parse_from_str(old_date, "%Y-%m-%d").ok()?;
    let new = NaiveDate::parse_from_str(new_date, "%Y-%m-%d").ok()?;
    let local = DateTime::parse_from_rfc3339(remind_at)
        .ok()?
        .with_timezone(&tz)
        .naive_local()
        .checked_add_signed(Duration::days((new - old).num_days()))?;
    let shifted = tz.from_local_datetime(&local).earliest().or_else(|| {
        tz.from_local_datetime(&local.checked_add_signed(Duration::hours(1))?)
            .earliest()
    })?;
    Some(canonical_remind(&shifted))
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
    if let Some(r) = &item.remind_at {
        match DateTime::parse_from_rfc3339(r) {
            Ok(d) if d.second() == 0 && d.nanosecond() == 0 => {}
            _ => return Err(Error::new("VALIDATION_ERROR", "提醒时间无效。")),
        }
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
    /// 可选提醒；`null` 和不传都表示不提醒。
    #[serde(default)]
    pub remind_at: Option<String>,
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
    /// 外层 None：不变；Some(None)：清除；Some(Some(s))：设置。
    #[serde(default, deserialize_with = "nullable")]
    pub remind_at: Option<Option<String>>,
}
impl Patch {
    pub fn apply(self, item: &mut Item, ctx: &TimeCtx) -> Result<()> {
        if self.date.is_none()
            && self.title.is_none()
            && self.notes.is_none()
            && self.color.is_none()
            && self.remind_at.is_none()
        {
            return Err(Error::new("VALIDATION_ERROR", "修改至少需要一个字段。"));
        }
        let old_date = item.date.clone();
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
        match self.remind_at {
            Some(None) => item.remind_at = None,
            Some(Some(s)) => {
                let parsed = parse_remind_at(&s, ctx.tz, ctx.now, false)?;
                // 原值原样传回来不因为已经过去而被拒绝。
                let stored = item.remind_at.as_deref().and_then(remind_epoch_ms);
                if ctx.strict && stored != Some(parsed.epoch_ms) {
                    parse_remind_at(&s, ctx.tz, ctx.now, true)?;
                }
                item.remind_at = Some(parsed.text);
            }
            None => {
                if item.date != old_date
                    && date(&item.date).is_ok()
                    && let Some(r) = item.remind_at.clone()
                {
                    match shift_remind_at(&r, &old_date, &item.date, ctx.tz) {
                        Some(s) => item.remind_at = Some(s),
                        None => tracing::warn!("remind_at shift failed; kept as is"),
                    }
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::{America::New_York, Asia::Shanghai};

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }
    /// 上海 2026-10-19 08:00:00。
    fn now() -> DateTime<Utc> {
        at("2026-10-19T00:00:00Z")
    }
    fn parse(s: &str) -> Result<RemindAt> {
        parse_remind_at(s, Shanghai, now(), true)
    }
    fn text(s: &str) -> String {
        parse(s)
            .unwrap_or_else(|e| panic!("{s}: {}", e.message))
            .text
    }
    fn message(s: &str) -> String {
        parse(s)
            .err()
            .unwrap_or_else(|| panic!("{s} accepted"))
            .message
    }
    fn item() -> Item {
        Item {
            id: Uuid::nil().to_string(),
            date: "2026-10-20".into(),
            title: "周会".into(),
            notes: String::new(),
            color: "blue".into(),
            status: "planned".into(),
            revision: 1,
            created_at: String::new(),
            updated_at: String::new(),
            visibility: "private".into(),
            remind_at: None,
        }
    }
    fn ctx(strict: bool) -> TimeCtx {
        TimeCtx {
            tz: Shanghai,
            now: now(),
            strict,
        }
    }

    #[test]
    fn parses_local_and_offset_forms_and_normalizes_to_minutes() {
        let want = "2026-10-20T09:00:00+08:00";
        for s in [
            "2026-10-20T09:00",
            "2026-10-20t09:00",
            "2026-10-20 09:00",
            "2026-10-20T09:00:37",
            "2026-10-20T09:00:59.999999999",
            "2026-10-20T01:00Z",
            "2026-10-20T01:00:12z",
            "2026-10-20T01:00:00.5Z",
            "2026-10-20T03:00+02:00",
            "2026-10-19T20:00-05:00",
            "2026-10-20T09:00+08:00",
            "  2026-10-20T09:00  ",
            "\t2026-10-20T09:00:00+08:00\n",
        ] {
            assert_eq!(text(s), want, "{s}");
        }
        assert_eq!(text(" 2026-10-20t09:00Z "), "2026-10-20T17:00:00+08:00");
        let r = parse("2026-10-20T09:00").unwrap();
        assert_eq!(r.epoch_ms, at("2026-10-20T01:00:00Z").timestamp_millis());
    }

    #[test]
    fn rejects_other_formats_with_details() {
        let format = REMIND_FORMAT;
        for s in [
            "",
            "   ",
            "tomorrow 9am",
            "2026-10-20T09:00+0800",
            "2026-10-20T09:00+08",
            "2026-10-20T09:00 UTC",
            "2026-10-20T09:00Asia/Shanghai",
            "2026-10-20T09",
            "2026-10-20T9:00",
            "2026-10-20_09:00",
            "2026-10-20T09:00.5",
            "2026-10-20T09:00:00.",
            "2026-10-20T09:00:00.1234567890",
            "2026-10-20T09:00:5",
            "2026-02-30T09:00",
            "2026-10-20T24:00",
            "2026-10-20T09:60",
            "２０２６-10-20T09:00",
            "2026-10-20T09:00+25:00",
        ] {
            assert_eq!(message(s), format, "{s:?}");
        }
        // 超过 40 个字符（去掉首尾空白之后）
        let long = format!("2026-10-20T09:00:00.123456789+08:00{}", "0".repeat(6));
        assert_eq!(long.len(), 41);
        assert_eq!(message(&long), format);
        let e = parse("tomorrow 9am").err().unwrap();
        assert_eq!(e.code, "VALIDATION_ERROR");
        assert_eq!(
            e.details,
            json!({"field":"remind_at","now":"2026-10-19T08:00:00+08:00","calendar_timezone":"Asia/Shanghai"})
        );
    }

    #[test]
    fn year_range_is_checked_in_the_calendar_timezone() {
        assert!(parse_remind_at("1900-01-01T12:00", Shanghai, now(), false).is_ok());
        assert_eq!(text("2100-12-31T23:59"), "2100-12-31T23:59:00+08:00");
        assert_eq!(message("1899-12-31T23:59"), REMIND_RANGE);
        assert_eq!(message("2101-01-01T00:00"), REMIND_RANGE);
        assert_eq!(message("2100-12-31T23:00Z"), REMIND_RANGE);
        assert!(parse_remind_at("1899-12-31T23:59Z", Shanghai, now(), false).is_ok());
    }

    #[test]
    fn dst_gap_is_rejected_and_overlap_takes_the_earlier() {
        let now = at("2026-01-01T00:00:00Z");
        let e = parse_remind_at("2026-03-08T02:30", New_York, now, true)
            .err()
            .unwrap();
        assert_eq!(e.message, REMIND_GAP);
        assert_eq!(e.details["calendar_timezone"], "America/New_York");
        let r = parse_remind_at("2026-11-01T01:30", New_York, now, true).unwrap();
        assert_eq!(r.text, "2026-11-01T01:30:00-04:00");
        // 带偏移时没有歧义：-05:00 那一次也能明确指定
        let r = parse_remind_at("2026-11-01T01:30-05:00", New_York, now, true).unwrap();
        assert_eq!(r.text, "2026-11-01T01:30:00-05:00");
    }

    #[test]
    fn require_future_has_a_60_second_tolerance() {
        // now = 上海 08:00:00；07:59 正好早 60 秒，仍然有效
        assert_eq!(text("2026-10-19T07:59"), "2026-10-19T07:59:00+08:00");
        assert_eq!(message("2026-10-19T07:58"), REMIND_PAST);
        assert_eq!(message("2020-01-01T09:00"), REMIND_PAST);
        assert!(parse_remind_at("2020-01-01T09:00", Shanghai, now(), false).is_ok());
    }

    #[test]
    fn shift_keeps_wall_time_across_months_and_dst() {
        assert_eq!(
            shift_remind_at(
                "2026-10-31T09:00:00+08:00",
                "2026-10-31",
                "2026-11-02",
                Shanghai
            )
            .as_deref(),
            Some("2026-11-02T09:00:00+08:00")
        );
        assert_eq!(
            shift_remind_at(
                "2026-10-19T21:00:00+08:00",
                "2026-10-20",
                "2026-10-13",
                Shanghai
            )
            .as_deref(),
            Some("2026-10-12T21:00:00+08:00")
        );
        // 跨夏令时：墙上时间不变，偏移跟着变
        assert_eq!(
            shift_remind_at(
                "2026-03-07T09:00:00-05:00",
                "2026-03-07",
                "2026-03-09",
                New_York
            )
            .as_deref(),
            Some("2026-03-09T09:00:00-04:00")
        );
        // 平移进跳过的时段：往后顺延 1 小时
        assert_eq!(
            shift_remind_at(
                "2026-03-07T02:30:00-05:00",
                "2026-03-07",
                "2026-03-08",
                New_York
            )
            .as_deref(),
            Some("2026-03-08T03:30:00-04:00")
        );
        assert_eq!(
            shift_remind_at("bad", "2026-03-07", "2026-03-08", New_York),
            None
        );
        assert_eq!(
            shift_remind_at("2026-03-07T02:30:00-05:00", "x", "2026-03-08", New_York),
            None
        );
    }

    #[test]
    fn canonical_now_keeps_seconds_without_fraction() {
        assert_eq!(
            canonical_now(Shanghai, at("2026-10-20T01:00:37.5Z")),
            "2026-10-20T09:00:37+08:00"
        );
    }

    #[test]
    fn validate_rejects_invalid_stored_remind_at() {
        let mut i = item();
        i.remind_at = Some("2026-10-20T09:00:00+08:00".into());
        assert!(validate(&mut i).is_ok());
        for bad in ["2026-10-20T09:00:30+08:00", "2026-10-20T09:00", "garbage"] {
            i.remind_at = Some(bad.into());
            assert_eq!(validate(&mut i).err().unwrap().message, "提醒时间无效。");
        }
    }

    #[test]
    fn item_without_remind_at_reads_as_none_and_always_serializes_it() {
        let mut v = serde_json::to_value(item()).unwrap();
        assert!(v.as_object().unwrap().contains_key("remind_at"));
        assert!(v["remind_at"].is_null());
        v.as_object_mut().unwrap().remove("remind_at");
        let back: Item = serde_json::from_value(v).unwrap();
        assert_eq!(back.remind_at, None);
    }

    #[test]
    fn decode_nullable_only_allows_listed_paths() {
        let base = json!({"idempotency_key":"k","calendar_generation":"g","item_id":"i","expected_revision":1});
        let mut a = base.clone();
        a["patch"] = json!({"remind_at":null});
        assert_eq!(decode_update(&a).unwrap().patch.remind_at, Some(None));
        a["patch"] = json!({"remind_at":null,"title":null});
        assert_eq!(
            decode_update(&a).err().unwrap().message,
            "字段不能为 null。"
        );
        // 同名字段出现在别的位置不放行
        let mut a = base.clone();
        a["patch"] = json!({"title":"x"});
        a["remind_at"] = Value::Null;
        assert_eq!(
            decode_update(&a).err().unwrap().message,
            "字段不能为 null。"
        );
        assert!(decode::<Update>(&json!({"patch":{"remind_at":null}})).is_err());

        let c = json!({"idempotency_key":"k","calendar_generation":"g","date":"2026-10-20","title":"x","remind_at":null});
        assert_eq!(decode_create(&c).unwrap().remind_at, None);
        let mut c2 = c.clone();
        c2["notes"] = Value::Null;
        assert_eq!(
            decode_create(&c2).err().unwrap().message,
            "字段不能为 null。"
        );
        assert!(decode::<Create>(&c).is_err(), "decode 仍然拒绝任何 null");
        // 数组和需要转义的键也按 JSON Pointer 计算路径
        assert!(decode_nullable::<Value>(&json!({"a":[1,null]}), &["/a/1"]).is_ok());
        assert!(decode_nullable::<Value>(&json!({"a":[null,1]}), &["/a/1"]).is_err());
        assert!(decode_nullable::<Value>(&json!({"a/b":null}), &["/a~1b"]).is_ok());
        assert!(decode_nullable::<Value>(&json!({"a/b":null}), &["/a/b"]).is_err());
    }

    #[test]
    fn patch_distinguishes_absent_null_and_value() {
        let p: Patch = serde_json::from_value(json!({})).unwrap();
        assert_eq!(p.remind_at, None);
        let p: Patch = serde_json::from_value(json!({"remind_at":null})).unwrap();
        assert_eq!(p.remind_at, Some(None));
        let p: Patch = serde_json::from_value(json!({"remind_at":"2026-10-20T09:00"})).unwrap();
        assert_eq!(p.remind_at, Some(Some("2026-10-20T09:00".into())));
    }

    #[test]
    fn patch_apply_follows_the_four_steps() {
        let patch = |v: Value| -> Patch { serde_json::from_value(v).unwrap() };
        let mut i = item();
        assert_eq!(
            patch(json!({}))
                .apply(&mut i, &ctx(true))
                .err()
                .unwrap()
                .message,
            "修改至少需要一个字段。"
        );
        // 只改提醒也算一个字段
        patch(json!({"remind_at":"2026-10-20T09:00"}))
            .apply(&mut i, &ctx(true))
            .unwrap();
        assert_eq!(i.remind_at.as_deref(), Some("2026-10-20T09:00:00+08:00"));
        // 只改日期：平移
        patch(json!({"date":"2026-10-25"}))
            .apply(&mut i, &ctx(true))
            .unwrap();
        assert_eq!(i.remind_at.as_deref(), Some("2026-10-25T09:00:00+08:00"));
        // 日期和提醒一起改：以给出的提醒为准，不平移
        patch(json!({"date":"2026-10-28","remind_at":"2026-10-27T21:00"}))
            .apply(&mut i, &ctx(true))
            .unwrap();
        assert_eq!(i.remind_at.as_deref(), Some("2026-10-27T21:00:00+08:00"));
        // 非法日期：报日期错误，不平移
        let mut j = i.clone();
        assert!(
            patch(json!({"date":"2026-13-01"}))
                .apply(&mut j, &ctx(true))
                .is_err()
        );
        // null 清除
        patch(json!({"remind_at":null}))
            .apply(&mut i, &ctx(true))
            .unwrap();
        assert_eq!(i.remind_at, None);
        // 改日期但没有提醒：什么都不做
        patch(json!({"date":"2026-10-29"}))
            .apply(&mut i, &ctx(true))
            .unwrap();
        assert_eq!(i.remind_at, None);

        // 严格模式：新的过去时间被拒；原值（即使已经过去）原样传回可以保存
        let mut old = item();
        old.remind_at = Some("2026-10-18T09:00:00+08:00".into());
        let mut k = old.clone();
        let e = patch(json!({"remind_at":"2026-10-18T10:00"}))
            .apply(&mut k, &ctx(true))
            .err()
            .unwrap();
        assert_eq!(e.message, REMIND_PAST);
        let mut k = old.clone();
        patch(json!({"remind_at":"2026-10-18T09:00:00+08:00","title":"改标题"}))
            .apply(&mut k, &ctx(true))
            .unwrap();
        assert_eq!(k.remind_at, old.remind_at);
        // 非严格（批准时）：过去的时间也接受
        let mut k = old.clone();
        patch(json!({"remind_at":"2026-10-18T10:00"}))
            .apply(&mut k, &ctx(false))
            .unwrap();
        assert_eq!(k.remind_at.as_deref(), Some("2026-10-18T10:00:00+08:00"));
        // 格式错误在非严格模式下仍然报错
        let mut k = old.clone();
        assert!(
            patch(json!({"remind_at":"tomorrow"}))
                .apply(&mut k, &ctx(false))
                .is_err()
        );
    }
}
