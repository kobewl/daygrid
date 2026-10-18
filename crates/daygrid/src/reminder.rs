//! 事项提醒：服务（`daygrid-service`）与桌面进程共用的常量、类型和通知文本。
//!
//! 服务端负责「什么时候到期、是否已经触发」（见 `store/reminders.rs`），
//! 桌面进程只负责到点后调用 `desktop_take_due_reminders`，再用这里的函数生成通知文本。
use serde::{Deserialize, Serialize};

/// 数据目录下的提示文件：`{"v":1,"next_due_ms":<i64|null>}`。
pub const HINT_FILE: &str = "reminders.next";
/// mac-notification-sys 只认这个字面值为「系统默认提示音」；传 "default" 会被当成不存在的自定义声音，结果没有声音。
pub const DEFAULT_SOUND: &str = "NSUserNotificationDefaultSoundName";
/// 应用没运行期间错过的提醒：24 小时以内的补发一次，更早的静默作废。
pub const MISSED_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
/// 写入时的容差：早于当前时间 60 秒以内的提醒仍然有效（会立即触发）。
pub const PAST_TOLERANCE_MS: i64 = 60_000;
/// 迟到超过 2 分钟的提醒在正文前加「错过的提醒 · 」。
pub const LATE_LABEL_MS: i64 = 120_000;
/// 一次 take 最多取出的提醒数。
pub const TAKE_LIMIT: i64 = 200;
/// 一批到期提醒里单独弹出的条数，其余合并成一条。
pub const MAX_INDIVIDUAL: usize = 3;

const TITLE_MAX: usize = 100;
const EXCERPT_MAX: usize = 80;

/// 一条到期、需要通知的提醒。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Due {
    pub item_id: String,
    pub title: String,
    /// 事项日期 YYYY-MM-DD
    pub date: String,
    /// 规范串，分钟精度
    pub remind_at: String,
    /// 备注摘要，可能为空
    pub excerpt: String,
    /// max(0, (now − due_ms) / 1000)
    pub late_seconds: i64,
}

/// `desktop_take_due_reminders` 的返回数据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TakeDue {
    /// 日历时区下的今天（桌面进程不知道日历时区）。
    pub today: String,
    pub reminders: Vec<Due>,
    pub skipped: u32,
    pub next_due_ms: Option<i64>,
}

fn truncate(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// `\s+`：至少一个空白，返回剩余部分。
fn spaces(s: &str) -> Option<&str> {
    let rest = s.trim_start();
    (rest.len() < s.len()).then_some(rest)
}

/// 备注摘要：取第一条非空行，剥掉一次 Markdown 前缀，最多 80 个字符。语义等价于只匹配一次的正则
/// `^\s*(?:[-*+]\s+\[[ xX]\]\s+|[-*+>]\s+|#{1,6}\s+|\d+[.)]\s+)`，按分支顺序尝试。
pub fn excerpt(notes: &str) -> String {
    let Some(line) = notes.lines().find(|l| !l.trim().is_empty()) else {
        return String::new();
    };
    let s = line.trim_start();
    let task = || {
        let r = spaces(s.strip_prefix(['-', '*', '+'])?)?;
        let r = r.strip_prefix('[')?.strip_prefix([' ', 'x', 'X'])?;
        spaces(r.strip_prefix(']')?)
    };
    let bullet = || spaces(s.strip_prefix(['-', '*', '+', '>'])?);
    let heading = || {
        let n = s.bytes().take_while(|&c| c == b'#').count();
        if (1..=6).contains(&n) {
            spaces(&s[n..])
        } else {
            None
        }
    };
    let ordered = || {
        let n = s.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return None;
        }
        spaces(s[n..].strip_prefix(['.', ')'])?)
    };
    let body = task()
        .or_else(bullet)
        .or_else(heading)
        .or_else(ordered)
        .unwrap_or(s);
    truncate(body.trim(), EXCERPT_MAX)
}

/// 「10月20日 09:00」：直接切片规范串，月日去掉前导零。
pub fn zh_datetime(remind_at: &str) -> String {
    let parts = (
        remind_at.get(5..7).and_then(|m| m.parse::<u32>().ok()),
        remind_at.get(8..10).and_then(|d| d.parse::<u32>().ok()),
        remind_at.get(11..16),
    );
    match parts {
        (Some(m), Some(d), Some(time)) => format!("{m}月{d}日 {time}"),
        _ => remind_at.to_string(),
    }
}

/// 一条通知的（标题，正文）。正文第一行是「今天 09:00」或「10月20日 09:00」，
/// 迟到超过 2 分钟时前面加「错过的提醒 · 」；有备注摘要时换行接摘要。
pub fn notification_text(due: &Due, today: &str) -> (String, String) {
    let title = truncate(&due.title, TITLE_MAX);
    // 规范串的前 10 个字节就是日历时区下的提醒日期。
    let when = match (due.remind_at.get(..10), due.remind_at.get(11..16)) {
        (Some(day), Some(time)) if day == today => format!("今天 {time}"),
        _ => zh_datetime(&due.remind_at),
    };
    let mut body = if due.late_seconds * 1000 > LATE_LABEL_MS {
        format!("错过的提醒 · {when}")
    } else {
        when
    };
    if !due.excerpt.is_empty() {
        body.push('\n');
        body.push_str(&due.excerpt);
    }
    (title, body)
}

/// 同一次取到的到期提醒（同一分钟的多个提醒，或启动时补发的多个错过提醒）：
/// 最早的 3 个各发一条，其余合并成一条「还有 N 个提醒」，正文是标题列表（「、」连接，最多 80 字符）。
pub fn plan_notifications(dues: &[Due], today: &str) -> Vec<(String, String)> {
    if dues.len() <= MAX_INDIVIDUAL {
        return dues.iter().map(|d| notification_text(d, today)).collect();
    }
    let mut out: Vec<_> = dues[..MAX_INDIVIDUAL]
        .iter()
        .map(|d| notification_text(d, today))
        .collect();
    let rest = &dues[MAX_INDIVIDUAL..];
    let titles: Vec<&str> = rest.iter().map(|d| d.title.as_str()).collect();
    out.push((
        format!("还有 {} 个提醒", rest.len()),
        truncate(&titles.join("、"), EXCERPT_MAX),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn due(title: &str, remind_at: &str, late_seconds: i64, excerpt: &str) -> Due {
        Due {
            item_id: "00000000-0000-4000-8000-000000000001".into(),
            title: title.into(),
            date: remind_at[..10].into(),
            remind_at: remind_at.into(),
            excerpt: excerpt.into(),
            late_seconds,
        }
    }

    #[test]
    fn excerpt_strips_one_markdown_prefix_like_the_regex() {
        // 每个分支
        assert_eq!(excerpt("- [ ] 买菜"), "买菜");
        assert_eq!(excerpt("* [x] 已买"), "已买");
        assert_eq!(excerpt("+ [X] 大写"), "大写");
        assert_eq!(excerpt("- 买菜"), "买菜");
        assert_eq!(excerpt("* 星号"), "星号");
        assert_eq!(excerpt("+ 加号"), "加号");
        assert_eq!(excerpt("> 引用"), "引用");
        assert_eq!(excerpt("## 标题"), "标题");
        assert_eq!(excerpt("###### 六级"), "六级");
        assert_eq!(excerpt("1. 第一步"), "第一步");
        assert_eq!(excerpt("12) 第十二步"), "第十二步");
        // - [ ] 优先于 -：不会留下「[ ] 买菜」
        assert_eq!(excerpt("-   [ ]   买菜"), "买菜");
        // 只剥一次
        assert_eq!(excerpt("- - 两层"), "- 两层");
        assert_eq!(excerpt("> > 嵌套"), "> 嵌套");
        // 前缀后必须有空白
        assert_eq!(excerpt("-买菜"), "-买菜");
        assert_eq!(excerpt("#标题"), "#标题");
        assert_eq!(excerpt("####### 七个井号"), "####### 七个井号");
        assert_eq!(excerpt("1.5 公里"), "1.5 公里");
        assert_eq!(excerpt("- [] 不是任务"), "[] 不是任务");
        // 第一条非空行、首尾空白
        assert_eq!(
            excerpt("\n   \n  ## 带上评审清单  \n第二行"),
            "带上评审清单"
        );
        assert_eq!(excerpt(""), "");
        assert_eq!(excerpt("  \n\t\n"), "");
        // 剥完为空
        assert_eq!(excerpt("- [ ] \n后面"), "");
        assert_eq!(excerpt("##  "), "");
        // 80 字截断
        let long = "字".repeat(81);
        assert_eq!(excerpt(&long), format!("{}…", "字".repeat(80)));
        assert_eq!(excerpt(&"字".repeat(80)), "字".repeat(80));
    }

    #[test]
    fn zh_datetime_drops_leading_zeros() {
        assert_eq!(zh_datetime("2026-10-20T09:00:00+08:00"), "10月20日 09:00");
        assert_eq!(zh_datetime("2026-01-05T23:59:00-05:00"), "1月5日 23:59");
        assert_eq!(zh_datetime("bad"), "bad");
    }

    #[test]
    fn notification_text_variants() {
        let today = "2026-10-20";
        let d = due("周会", "2026-10-20T09:00:00+08:00", 0, "带上评审清单");
        assert_eq!(
            notification_text(&d, today),
            ("周会".into(), "今天 09:00\n带上评审清单".into())
        );
        let d = due("周会", "2026-10-21T09:00:00+08:00", 0, "");
        assert_eq!(notification_text(&d, today).1, "10月21日 09:00");
        // 2 分钟以内不算迟到
        let d = due("周会", "2026-10-20T09:00:00+08:00", 120, "");
        assert_eq!(notification_text(&d, today).1, "今天 09:00");
        let d = due("周会", "2026-10-19T09:00:00+08:00", 121, "摘要");
        assert_eq!(
            notification_text(&d, today).1,
            "错过的提醒 · 10月19日 09:00\n摘要"
        );
        let d = due(&"长".repeat(101), "2026-10-20T09:00:00+08:00", 0, "");
        assert_eq!(
            notification_text(&d, today).0,
            format!("{}…", "长".repeat(100))
        );
        let d = due(&"长".repeat(100), "2026-10-20T09:00:00+08:00", 0, "");
        assert_eq!(notification_text(&d, today).0, "长".repeat(100));
    }

    #[test]
    fn plan_notifications_merges_beyond_three_per_batch() {
        let today = "2026-10-20";
        let three: Vec<Due> = (0..3)
            .map(|i| due(&format!("事项{i}"), "2026-10-20T09:00:00+08:00", 0, ""))
            .collect();
        let plan = plan_notifications(&three, today);
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[2].0, "事项2");
        assert!(plan_notifications(&[], today).is_empty());

        // 启动时补发的一批：5 个不同时刻（跨几小时）、迟到时间不同的错过提醒。
        let batch = vec![
            due("早会", "2026-10-20T06:00:00+08:00", 3 * 3600, ""),
            due("交周报", "2026-10-20T07:30:00+08:00", 5400, "附上数据"),
            due("买咖啡", "2026-10-20T08:15:00+08:00", 2700, ""),
            due("回邮件", "2026-10-20T08:40:00+08:00", 1200, ""),
            due("订会议室", "2026-10-20T08:59:00+08:00", 60, ""),
        ];
        let plan = plan_notifications(&batch, today);
        assert_eq!(plan.len(), 4);
        assert_eq!(plan[0], ("早会".into(), "错过的提醒 · 今天 06:00".into()));
        assert_eq!(
            plan[1],
            ("交周报".into(), "错过的提醒 · 今天 07:30\n附上数据".into())
        );
        assert_eq!(plan[2].0, "买咖啡");
        assert_eq!(plan[3], ("还有 2 个提醒".into(), "回邮件、订会议室".into()));

        // 合并条的正文最多 80 字符
        let many: Vec<Due> = (0..10)
            .map(|i| {
                due(
                    &format!("很长很长的事项标题{i:02}号"),
                    "2026-10-20T09:00:00+08:00",
                    0,
                    "",
                )
            })
            .collect();
        let plan = plan_notifications(&many, today);
        assert_eq!(plan[3].0, "还有 7 个提醒");
        assert_eq!(plan[3].1.chars().count(), 81);
        assert!(plan[3].1.ends_with('…'));
    }

    #[test]
    fn default_sound_is_the_ns_constant() {
        assert_eq!(DEFAULT_SOUND, "NSUserNotificationDefaultSoundName");
    }

    #[test]
    fn take_due_round_trips_through_json() {
        let t = TakeDue {
            today: "2026-10-20".into(),
            reminders: vec![due("周会", "2026-10-20T09:00:00+08:00", 3, "x")],
            skipped: 2,
            next_due_ms: Some(1_760_922_000_000),
        };
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v["reminders"][0]["late_seconds"], 3);
        assert_eq!(serde_json::from_value::<TakeDue>(v).unwrap(), t);
        let empty = TakeDue {
            today: "2026-10-20".into(),
            reminders: vec![],
            skipped: 0,
            next_due_ms: None,
        };
        let v = serde_json::to_value(&empty).unwrap();
        assert!(v["next_due_ms"].is_null());
        assert_eq!(serde_json::from_value::<TakeDue>(v).unwrap(), empty);
    }
}
