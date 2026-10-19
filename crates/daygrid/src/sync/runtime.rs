//! 同步引擎与界面之间共享的"易失"状态：连接请求、状态、在线成员、本机在线状态。
//!
//! 这些信息不需要持久化（重启后从头建立），所以不放数据库，放在一个带条件变量的小对象里。
//! 用 Java 的话说：一个 `Object` 监视器——`wait(timeout)` / `notifyAll()` 把同步线程挂起与唤醒。
use super::Peer;
use std::{
    sync::{Condvar, Mutex},
    time::Duration,
};

/// 界面发起的连接请求（由同步线程在锁外执行网络调用）。
#[derive(Debug, Clone)]
pub struct ConnectRequest {
    pub url: String,
    pub invite: Option<String>,
    pub token: Option<String>,
    pub display_name: String,
    /// 令牌加入了多个团队时，指定要连的团队（名称或 ID）。
    pub workspace: Option<String>,
    /// 是否把本机现有事项共享给团队。
    pub share_existing: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalPresence {
    pub date: Option<String>,
    pub item_id: Option<String>,
    pub editing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// `off` / `connecting` / `syncing` / `online` / `offline` / `error`
    pub state: &'static str,
    pub last_sync_at: Option<String>,
    pub last_error: Option<String>,
    pub peers: Vec<Peer>,
    pub events_connected: bool,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            state: "off",
            last_sync_at: None,
            last_error: None,
            peers: Vec::new(),
            events_connected: false,
        }
    }
}

#[derive(Default)]
struct State {
    wake: bool,
    connect: Option<ConnectRequest>,
    status: Status,
    presence: LocalPresence,
    presence_dirty: bool,
    /// 状态或在线成员每变一次加一；界面据此判断是否需要刷新。
    seq: u64,
}

#[derive(Default)]
pub struct Runtime {
    state: Mutex<State>,
    cv: Condvar,
}

impl Runtime {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// 唤醒同步线程（本地有新写入 / 收到 poke / 有连接请求）。
    pub fn wake(&self) {
        self.lock().wake = true;
        self.cv.notify_all();
    }
    /// 等待唤醒或超时；返回前清除唤醒标记。
    pub fn wait(&self, timeout: Duration) {
        let mut state = self.lock();
        if !state.wake {
            state = self
                .cv
                .wait_timeout(state, timeout)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        state.wake = false;
    }
    pub fn request_connect(&self, request: ConnectRequest) {
        {
            let mut s = self.lock();
            s.connect = Some(request);
            s.status.state = "connecting";
            s.status.last_error = None;
            s.seq += 1;
        }
        self.wake();
    }
    pub fn take_connect(&self) -> Option<ConnectRequest> {
        self.lock().connect.take()
    }
    pub fn status(&self) -> Status {
        self.lock().status.clone()
    }
    pub fn seq(&self) -> u64 {
        self.lock().seq
    }
    /// 更新状态；只有内容真的变了才推进 `seq`，避免界面无谓刷新。
    pub fn update(&self, f: impl FnOnce(&mut Status)) {
        let mut s = self.lock();
        let before = s.status.clone();
        f(&mut s.status);
        if s.status != before {
            s.seq += 1;
        }
    }
    /// 更新状态但不通知界面刷新：用于"最近同步时间"这类只读展示、每轮都会变的字段。
    pub fn update_quiet(&self, f: impl FnOnce(&mut Status)) {
        f(&mut self.lock().status);
    }
    pub fn set_presence(&self, presence: LocalPresence) {
        {
            let mut s = self.lock();
            if s.presence == presence {
                return;
            }
            s.presence = presence;
            s.presence_dirty = true;
        }
        self.wake();
    }
    pub fn presence(&self) -> LocalPresence {
        self.lock().presence.clone()
    }
    /// 取走"需要立即上报"标记。
    pub fn take_presence_dirty(&self) -> bool {
        std::mem::take(&mut self.lock().presence_dirty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Instant};

    #[test]
    fn wake_before_wait_is_not_lost() {
        let rt = Runtime::default();
        rt.wake();
        let started = Instant::now();
        rt.wait(Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn wait_times_out_and_wake_from_other_thread_returns_early() {
        let rt = Arc::new(Runtime::default());
        let started = Instant::now();
        rt.wait(Duration::from_millis(50));
        assert!(started.elapsed() >= Duration::from_millis(40));
        let other = rt.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            other.wake();
        });
        let started = Instant::now();
        rt.wait(Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(2));
        handle.join().unwrap();
    }

    #[test]
    fn seq_only_advances_on_real_changes() {
        let rt = Runtime::default();
        let s0 = rt.seq();
        rt.update(|s| s.state = "online");
        let s1 = rt.seq();
        assert!(s1 > s0);
        rt.update(|s| s.state = "online");
        assert_eq!(rt.seq(), s1);
        rt.set_presence(LocalPresence {
            date: Some("2026-10-14".into()),
            ..Default::default()
        });
        assert!(rt.take_presence_dirty());
        assert!(!rt.take_presence_dirty());
    }
}
