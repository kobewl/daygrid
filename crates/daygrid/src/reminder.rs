//! 事项提醒：服务（`daygrid-service`）与桌面进程共用的常量、类型和通知文本。
//!
//! 服务端负责「什么时候到期、是否已经触发」（见 `store/reminders.rs`），
//! 桌面进程只负责到点后调用 `desktop_take_due_reminders`，再用这里的函数生成通知文本。
//! 桌面进程的调度循环（`run_scheduler`）也放在这里，时钟和副作用都通过 `SchedulerEnv` 注入，
//! 不需要 Tauri 就能用虚拟时钟测试。
use serde::{Deserialize, Serialize};
use std::{io::Read, path::Path, time::Duration};

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

/// 提示文件最多读这么多字节；正常内容不到 100 B。
const HINT_MAX: u64 = 4 * 1024;
/// 没有已知的下一个提醒时，最长等这么久再看一次（也兜住睡眠唤醒和改时钟）。
const MAX_WAIT_MS: i64 = 60_000;
/// 服务不可用时的退避。
const BACKOFF_MS: i64 = 60_000;
/// 提前这么多毫秒以内就算到期（与服务端 `due_ms <= now + 999` 一致）。
const DUE_SLACK_MS: i64 = 999;
/// 连续取到「仍已到期」的 next 这么多次后暂停一下，防止忙循环。
const RETAKE_BURST: u32 = 5;
const RETAKE_PAUSE: Duration = Duration::from_secs(1);
/// 连续这么多次读不到有效提示时主动 take，让服务重写文件。
const HINT_MISSES: u8 = 3;

/// 读提示文件：外层 None 表示文件不存在或格式不对；内层是 `next_due_ms`。
pub fn read_hint(dir: &Path) -> Option<Option<i64>> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Hint {
        v: u32,
        next_due_ms: Option<i64>,
    }
    let mut text = String::new();
    std::fs::File::open(dir.join(HINT_FILE))
        .ok()?
        .take(HINT_MAX)
        .read_to_string(&mut text)
        .ok()?;
    let hint: Hint = serde_json::from_str(&text).ok()?;
    (hint.v == 1).then_some(hint.next_due_ms)
}

/// 监视数据目录：提示文件用 tmp + rename 替换，目录条目变化会触发目录的 `NOTE_WRITE`。
/// 监视的是目录而不是文件，因为 rename 会换掉文件的 inode。
pub struct HintWatcher {
    #[cfg(target_os = "macos")]
    kq: libc::c_int,
    #[cfg(target_os = "macos")]
    dir: libc::c_int,
}

#[cfg(target_os = "macos")]
impl HintWatcher {
    pub fn new(dir: &Path) -> std::io::Result<Self> {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(dir.as_os_str().as_bytes())?;
        // SAFETY: path 是有效的以 NUL 结尾的字符串，open 不保留这个指针。
        let dir_fd = unsafe { libc::open(path.as_ptr(), libc::O_EVTONLY | libc::O_CLOEXEC) };
        if dir_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: kqueue 没有参数；返回值在下面检查。
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            let e = std::io::Error::last_os_error();
            // SAFETY: dir_fd 是上面刚打开、还没交出去的描述符，只关闭一次。
            unsafe { libc::close(dir_fd) };
            return Err(e);
        }
        let watcher = Self { kq, dir: dir_fd };
        let change = libc::kevent {
            ident: dir_fd as libc::uintptr_t,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: libc::NOTE_WRITE,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: kq 和 dir_fd 都有效；changelist 指向 1 个有效的 kevent，不取事件（eventlist 为空）。
        let r = unsafe {
            libc::kevent(
                watcher.kq,
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if r < 0 {
            // watcher 在这里被 drop，两个描述符都会关闭。
            return Err(std::io::Error::last_os_error());
        }
        Ok(watcher)
    }

    /// 阻塞到目录发生变化（返回 true）或超时（返回 false）。被信号打断时返回 false。
    pub fn wait(&self, timeout: Duration) -> bool {
        let ts = libc::timespec {
            tv_sec: libc::time_t::try_from(timeout.as_secs()).unwrap_or(libc::time_t::MAX),
            tv_nsec: libc::c_long::from(timeout.subsec_nanos()),
        };
        // SAFETY: kevent 是纯数据结构，全零是合法值；内核只往里写。
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        // SAFETY: kq 有效；不提交变更，eventlist 指向 1 个可写的 kevent，ts 在调用期间有效。
        let n = unsafe { libc::kevent(self.kq, std::ptr::null(), 0, &mut event, 1, &ts) };
        n > 0
    }
}

#[cfg(target_os = "macos")]
impl Drop for HintWatcher {
    fn drop(&mut self) {
        // SAFETY: 两个描述符都由这个结构独占，Drop 只执行一次。
        unsafe {
            libc::close(self.kq);
            libc::close(self.dir);
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl HintWatcher {
    pub fn new(_dir: &Path) -> std::io::Result<Self> {
        Ok(Self {})
    }
    /// 非 macOS 平台没有实现目录监视：按超时睡眠，返回 false。
    pub fn wait(&self, timeout: Duration) -> bool {
        std::thread::sleep(timeout);
        false
    }
}

/// 每次醒来读到提示后调用。提示只在值「变化」时才覆盖 `next`：服务如果持续写不进文件，
/// 文件里会一直是一个过时的值，若每次都采用它，线程会每 ≤60 秒调用一次 take，服务永远无法空闲退出。
/// 返回 true 表示连续多次读不到有效提示，需要主动 take（让服务重写文件）。
pub fn adopt(
    hint: Option<Option<i64>>,
    last_hint: &mut Option<Option<i64>>,
    next: &mut Option<i64>,
    misses: &mut u8,
) -> bool {
    match hint {
        Some(h) if Some(h) != *last_hint => {
            *last_hint = Some(h);
            *next = h;
            *misses = 0;
            false
        }
        Some(_) => {
            *misses = 0;
            false
        }
        None => {
            *misses += 1;
            if *misses >= HINT_MISSES {
                *misses = 0;
                true
            } else {
                false
            }
        }
    }
}

/// 调度循环依赖的外部环境：时钟、等待、提示文件和 take。桌面进程用真实实现，测试用虚拟时钟。
pub trait SchedulerEnv {
    /// 当前 Unix 毫秒。
    fn now_ms(&self) -> i64;
    /// 阻塞到目录变化（true）或超时（false）；桌面实现里自带监视器降级和重建。
    fn wait(&mut self, timeout: Duration) -> bool;
    fn sleep(&mut self, d: Duration);
    fn read_hint(&mut self) -> Option<Option<i64>>;
    /// 调用 desktop_take_due_reminders 并发通知；返回 next_due_ms；Err = 服务不可用或解码失败。
    /// 调度循环对所有错误的处理都一样（退避 60 秒），细节由实现自己记日志，所以 Err 不带内容。
    #[allow(clippy::result_unit_err)]
    fn take_and_notify(&mut self) -> std::result::Result<Option<i64>, ()>;
    fn should_stop(&self) -> bool {
        false
    }
}

/// take 一次；返回的 next 仍已到期（单次最多处理 TAKE_LIMIT 条）时立即再取，
/// 连续 5 次后暂停 1 秒。服务不可用时退避 60 秒。
fn take(env: &mut impl SchedulerEnv) -> Option<i64> {
    let mut burst = 0;
    loop {
        match env.take_and_notify() {
            Err(()) => return Some(env.now_ms() + BACKOFF_MS),
            Ok(next) if next.is_some_and(|t| t <= env.now_ms()) && !env.should_stop() => {
                burst += 1;
                if burst >= RETAKE_BURST {
                    burst = 0;
                    env.sleep(RETAKE_PAUSE);
                }
            }
            Ok(next) => return next,
        }
    }
}

/// 桌面提醒线程的主循环：启动时先 take 一次（补发错过的提醒并拿到 next），
/// 之后阻塞等待到 next（最长 60 秒）或提示文件变化，每次醒来重读提示，到期就 take。没有轮询。
pub fn run_scheduler(env: &mut impl SchedulerEnv) {
    let mut next = take(env);
    let mut last_hint: Option<Option<i64>> = None;
    let mut misses = 0;
    while !env.should_stop() {
        let wait = match next {
            Some(t) => (t - env.now_ms()).clamp(0, MAX_WAIT_MS),
            None => MAX_WAIT_MS,
        };
        if wait > 0 {
            // wait 在 1..=60_000 之间，转换不会失败。
            env.wait(Duration::from_millis(u64::try_from(wait).unwrap_or(0)));
        }
        let hint = env.read_hint();
        if adopt(hint, &mut last_hint, &mut next, &mut misses) {
            next = take(env);
        }
        if next.is_some_and(|t| t <= env.now_ms() + DUE_SLACK_MS) {
            next = take(env);
        }
    }
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
    fn adopt_only_takes_changed_hints() {
        let (mut last, mut next, mut misses) = (None, Some(5), 0u8);
        // 第一次读到的有效提示一定被采用
        assert!(!adopt(Some(Some(100)), &mut last, &mut next, &mut misses));
        assert_eq!((last, next), (Some(Some(100)), Some(100)));
        // 值变化：采用（包括变成「没有提醒」）
        assert!(!adopt(Some(None), &mut last, &mut next, &mut misses));
        assert_eq!((last, next), (Some(None), None));
        // 值不变：保留 take 给出的 next，过时的提示不会把 next 拉回去引起反复 take
        next = Some(900);
        assert!(!adopt(Some(None), &mut last, &mut next, &mut misses));
        assert_eq!(next, Some(900));
        // 连续 3 次读不到：要求 take，并把计数清零
        assert!(!adopt(None, &mut last, &mut next, &mut misses));
        assert!(!adopt(None, &mut last, &mut next, &mut misses));
        assert_eq!(misses, 2);
        assert!(adopt(None, &mut last, &mut next, &mut misses));
        assert_eq!(misses, 0);
        assert_eq!(next, Some(900));
        // 读到有效值（哪怕没变）也会清零
        assert!(!adopt(None, &mut last, &mut next, &mut misses));
        assert!(!adopt(Some(None), &mut last, &mut next, &mut misses));
        assert_eq!(misses, 0);
    }

    #[test]
    fn read_hint_rejects_missing_invalid_and_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_hint(dir.path()), None);
        let write = |s: &str| std::fs::write(dir.path().join(HINT_FILE), s).unwrap();
        write(r#"{"v":1,"next_due_ms":1760922000000}"#);
        assert_eq!(read_hint(dir.path()), Some(Some(1_760_922_000_000)));
        write(r#"{"v":1,"next_due_ms":null}"#);
        assert_eq!(read_hint(dir.path()), Some(None));
        for bad in [
            "",
            "garbage",
            r#"{"v":2,"next_due_ms":null}"#,
            r#"{"v":1,"next_due_ms":"x"}"#,
            r#"{"v":1,"next_due_ms":null,"x":1}"#,
        ] {
            write(bad);
            assert_eq!(read_hint(dir.path()), None, "{bad}");
        }
        // 超过 4 KiB 的部分不读，截断后的 JSON 无效
        write(&format!(
            r#"{{"v":1,"next_due_ms":null{}}}"#,
            " ".repeat(5000)
        ));
        assert_eq!(read_hint(dir.path()), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn hint_watcher_wakes_on_rename_and_times_out_otherwise() {
        use std::time::Instant;
        let dir = tempfile::tempdir().unwrap();
        let watcher = HintWatcher::new(dir.path()).unwrap();
        // 没有写入：超时返回 false
        let started = Instant::now();
        assert!(!watcher.wait(Duration::from_millis(200)));
        assert!(started.elapsed() >= Duration::from_millis(150));
        // 另一个线程以 tmp + rename 写提示文件：1 秒内醒来
        let path = dir.path().to_path_buf();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let tmp = path.join(format!("{HINT_FILE}.tmp"));
            std::fs::write(&tmp, r#"{"v":1,"next_due_ms":null}"#).unwrap();
            std::fs::rename(&tmp, path.join(HINT_FILE)).unwrap();
        });
        let started = Instant::now();
        assert!(watcher.wait(Duration::from_secs(5)));
        assert!(started.elapsed() < Duration::from_secs(1));
        writer.join().unwrap();
        assert_eq!(read_hint(dir.path()), Some(None));
        assert!(HintWatcher::new(&dir.path().join("missing")).is_err());
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Ev {
        Take,
        Sleep,
    }

    /// 虚拟环境：wait / sleep 只推进虚拟时钟；take 模拟服务（取走到期的提醒、返回下一个）。
    struct FakeEnv {
        now: i64,
        end: i64,
        pending: Vec<i64>,
        /// 每次通知时的 (虚拟 now, 提醒时刻)
        fired: Vec<(i64, i64)>,
        waits: Vec<Duration>,
        events: Vec<Ev>,
        take_result: Option<std::result::Result<Option<i64>, ()>>,
        /// 模拟提示文件不存在（服务从没运行起来）。
        hint_missing: bool,
    }

    impl FakeEnv {
        fn new(end: i64, pending: Vec<i64>) -> Self {
            Self {
                now: 0,
                end,
                pending,
                fired: vec![],
                waits: vec![],
                events: vec![],
                take_result: None,
                hint_missing: false,
            }
        }
        fn takes(&self) -> usize {
            self.events.iter().filter(|e| **e == Ev::Take).count()
        }
        fn next(&self) -> Option<i64> {
            self.pending.iter().copied().min()
        }
    }

    impl SchedulerEnv for FakeEnv {
        fn now_ms(&self) -> i64 {
            self.now
        }
        fn wait(&mut self, timeout: Duration) -> bool {
            self.waits.push(timeout);
            self.now += i64::try_from(timeout.as_millis()).unwrap();
            false
        }
        fn sleep(&mut self, d: Duration) {
            self.events.push(Ev::Sleep);
            self.now += i64::try_from(d.as_millis()).unwrap();
        }
        fn read_hint(&mut self) -> Option<Option<i64>> {
            (!self.hint_missing).then(|| self.next())
        }
        fn take_and_notify(&mut self) -> std::result::Result<Option<i64>, ()> {
            self.events.push(Ev::Take);
            if let Some(r) = self.take_result {
                return r.map(|n| n.map(|_| self.now));
            }
            let now = self.now;
            let (due, rest): (Vec<i64>, Vec<i64>) =
                self.pending.iter().partition(|&&t| t <= now + 999);
            self.fired.extend(due.into_iter().map(|t| (now, t)));
            self.pending = rest;
            Ok(self.next())
        }
        fn should_stop(&self) -> bool {
            self.now >= self.end
        }
    }

    const MIN: i64 = 60_000;

    #[test]
    fn scheduler_fires_once_without_busy_looping() {
        let due = 10 * MIN + 123;
        let mut env = FakeEnv::new(120 * MIN, vec![due]);
        run_scheduler(&mut env);
        // 启动一次、到点一次
        assert_eq!(env.takes(), 2, "{:?}", env.events);
        assert_eq!(env.fired.len(), 1);
        let (at, t) = env.fired[0];
        assert!((at - t).abs() <= 1000, "fired at {at}, due {t}");
        assert!(env.waits.iter().all(|w| !w.is_zero()));
        assert!(env.waits.len() <= 121, "{}", env.waits.len());
    }

    #[test]
    fn scheduler_without_reminders_waits_a_minute_at_a_time() {
        let mut env = FakeEnv::new(120 * MIN, vec![]);
        run_scheduler(&mut env);
        assert_eq!(env.takes(), 1);
        assert!(env.waits.len() <= 121, "{}", env.waits.len());
        assert!(env.waits.iter().all(|w| *w == Duration::from_secs(60)));
    }

    #[test]
    fn scheduler_backs_off_a_minute_while_the_service_fails() {
        let mut env = FakeEnv::new(30 * MIN, vec![]);
        env.take_result = Some(Err(()));
        env.hint_missing = true;
        run_scheduler(&mut env);
        assert!(env.waits.iter().all(|w| *w == Duration::from_secs(60)));
        // 每分钟重试一次，加上启动那次
        assert_eq!(env.takes(), env.waits.len() + 1);
        assert!(!env.events.contains(&Ev::Sleep));
    }

    #[test]
    fn scheduler_pauses_after_five_already_due_takes() {
        let mut env = FakeEnv::new(3000, vec![]);
        // 每次 take 都说「下一个就是现在」
        env.take_result = Some(Ok(Some(0)));
        run_scheduler(&mut env);
        let expected: Vec<Ev> = [Ev::Take; 5]
            .into_iter()
            .chain([Ev::Sleep])
            .cycle()
            .take(env.events.len())
            .collect();
        assert_eq!(env.events, expected);
        assert_eq!(env.events.iter().filter(|e| **e == Ev::Sleep).count(), 3);
        assert!(env.waits.is_empty());
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
