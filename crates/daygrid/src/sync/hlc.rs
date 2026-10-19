//! 混合逻辑时钟（HLC）：让不同机器上产生的时间戳"全局可比较、永不回退"。
//!
//! 用 Java 的话说：`System.currentTimeMillis()` 在两台机器之间不可比（时钟会漂、会回拨），
//! HLC 在毫秒墙钟之外加一个逻辑计数器和节点号，保证：
//! 1. 同一节点连续产生的时间戳严格递增（即使系统时钟停滞或回拨）；
//! 2. 收到别人的时间戳后，自己后续产生的一定更大（因果顺序）；
//! 3. 任意两个时间戳都能比出全序（节点号兜底，不会相等）。
//!
//! 字符串格式定长，字典序等于时间序：`{wall_ms:013}-{counter:05}-{node}`。
//! 算法参照 Actual Budget 的 `timestamp.ts` 注释中引用的 HLC 规范。
use std::cmp::Ordering;

/// 允许远端时间领先本机的最大毫秒数。超过则不采纳（防止本机时钟被"拉飞"）。
pub const MAX_DRIFT_MS: u64 = 300_000;
/// 计数器的上限：字符串格式是定长 5 位，超过就会破坏"字典序 = 时间序"。
pub const MAX_COUNTER: u32 = 99_999;
/// 墙钟的上限：13 位十进制。
pub const MAX_WALL_MS: u64 = 9_999_999_999_999;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hlc {
    pub wall_ms: u64,
    pub counter: u32,
    pub node: String,
}

impl Hlc {
    pub fn new(wall_ms: u64, counter: u32, node: impl Into<String>) -> Self {
        Self {
            wall_ms,
            counter,
            node: node.into(),
        }
    }
    /// 解析 `{wall}-{counter}-{node}`。node 里不允许再含 `-`。
    /// 只接受规范格式（定长补零、计数器与墙钟不超上限）：字符串比较必须等价于时间比较，
    /// 所以别人带来的"位数不对"的时间戳一律当作无效，而不是悄悄参与比较。
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.splitn(3, '-');
        let wall_ms: u64 = parts.next()?.parse().ok()?;
        let counter: u32 = parts.next()?.parse().ok()?;
        let node = parts.next()?;
        if node.is_empty() || node.contains('-') || parts.next().is_some() {
            return None;
        }
        if wall_ms > MAX_WALL_MS || counter > MAX_COUNTER {
            return None;
        }
        let h = Self::new(wall_ms, counter, node);
        (h.to_string() == text).then_some(h)
    }
    /// 夹紧：墙钟超过 `limit_ms` 时改成 `limit_ms`。返回是否发生夹紧。
    pub fn clamp_wall(&mut self, limit_ms: u64) -> bool {
        if self.wall_ms > limit_ms {
            self.wall_ms = limit_ms;
            true
        } else {
            false
        }
    }
}

impl std::fmt::Display for Hlc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:013}-{:05}-{}", self.wall_ms, self.counter, self.node)
    }
}

impl PartialOrd for Hlc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Hlc {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.wall_ms, self.counter, &self.node).cmp(&(other.wall_ms, other.counter, &other.node))
    }
}

/// 同一毫秒内的下一个 (墙钟, 计数器)。计数器满了就把墙钟进一毫秒、计数器归零，
/// 既保持严格递增，又绝不超出定长格式（也绝不会整数溢出）。
fn tick(wall: u64, counter: u32) -> (u64, u32) {
    if counter >= MAX_COUNTER {
        (wall.saturating_add(1).min(MAX_WALL_MS), 0)
    } else {
        (wall, counter + 1)
    }
}

/// 本机时钟。`now_ms` 由调用方传入，便于测试里模拟时钟回拨与漂移。
#[derive(Debug, Clone)]
pub struct Clock {
    last: Hlc,
}

impl Clock {
    pub fn new(node: impl Into<String>) -> Self {
        Self {
            last: Hlc::new(0, 0, node),
        }
    }
    /// 从持久化的最近时间戳恢复；解析失败则从零开始。节点号始终以 `node` 为准。
    pub fn restore(node: &str, saved: Option<&str>) -> Self {
        let mut clock = Self::new(node);
        if let Some(h) = saved.and_then(Hlc::parse) {
            clock.last = Hlc::new(h.wall_ms, h.counter, node);
        }
        clock
    }
    pub fn last(&self) -> &Hlc {
        &self.last
    }
    /// 产生一个新的本地时间戳（本地事件 / 发送消息）。
    pub fn send(&mut self, now_ms: u64) -> Hlc {
        let wall = self.last.wall_ms.max(now_ms).min(MAX_WALL_MS);
        let (wall, counter) = if wall == self.last.wall_ms {
            tick(wall, self.last.counter)
        } else {
            (wall, 0)
        };
        self.last = Hlc::new(wall, counter, self.last.node.clone());
        self.last.clone()
    }
    /// 吸收远端时间戳（接收消息）。远端领先本机超过 [`MAX_DRIFT_MS`] 时不采纳，返回 `false`。
    pub fn recv(&mut self, remote: &Hlc, now_ms: u64) -> bool {
        if remote.wall_ms > now_ms.saturating_add(MAX_DRIFT_MS) {
            return false;
        }
        let wall = self
            .last
            .wall_ms
            .max(remote.wall_ms)
            .max(now_ms)
            .min(MAX_WALL_MS);
        let (wall, counter) = if wall == self.last.wall_ms && wall == remote.wall_ms {
            tick(wall, self.last.counter.max(remote.counter))
        } else if wall == self.last.wall_ms {
            tick(wall, self.last.counter)
        } else if wall == remote.wall_ms {
            tick(wall, remote.counter)
        } else {
            (wall, 0)
        };
        self.last = Hlc::new(wall, counter, self.last.node.clone());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_form_sorts_like_time_and_round_trips() {
        let a = Hlc::new(1_792_022_723_123, 1, "aaaa");
        let b = Hlc::new(1_792_022_723_123, 2, "aaaa");
        let c = Hlc::new(1_792_022_723_124, 0, "aaaa");
        assert!(a.to_string() < b.to_string() && b.to_string() < c.to_string());
        assert!(a < b && b < c);
        assert_eq!(Hlc::parse(&a.to_string()), Some(a.clone()));
        assert_eq!(a.to_string(), "1792022723123-00001-aaaa");
        // 墙钟位数不足时补零，不会因为位数不同而排错。
        assert!(Hlc::new(999, 0, "n").to_string() < Hlc::new(1000, 0, "n").to_string());
    }

    #[test]
    fn parse_rejects_garbage() {
        for bad in ["", "1-2", "x-1-n", "1-x-n", "1-2-", "1-2-a-b", "-1-2-n"] {
            assert!(Hlc::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn send_is_strictly_increasing_even_when_wall_clock_stalls_or_goes_back() {
        let mut clock = Clock::new("n1");
        let a = clock.send(1000);
        let b = clock.send(1000); // 同一毫秒
        let c = clock.send(900); // 时钟回拨
        let d = clock.send(1001);
        assert!(a < b && b < c && c < d);
        assert_eq!((c.wall_ms, c.counter), (1000, 2));
        assert_eq!((d.wall_ms, d.counter), (1001, 0));
    }

    #[test]
    fn recv_orders_after_remote_and_local() {
        let mut clock = Clock::new("n1");
        let local = clock.send(1000);
        let remote = Hlc::new(1500, 7, "n2");
        assert!(clock.recv(&remote, 1000));
        let next = clock.send(1000);
        assert!(next > remote && next > local);
        // 本机更新时，计数器取两者较大者 + 1。
        let mut clock = Clock::new("n1");
        clock.send(2000);
        clock.send(2000);
        assert!(clock.recv(&Hlc::new(2000, 9, "n2"), 1000));
        assert_eq!(clock.last().counter, 10);
    }

    #[test]
    fn recv_refuses_timestamps_far_in_the_future() {
        let mut clock = Clock::new("n1");
        let before = clock.send(1000);
        let future = Hlc::new(1000 + MAX_DRIFT_MS + 1, 0, "n2");
        assert!(!clock.recv(&future, 1000));
        assert_eq!(clock.last(), &before, "拒绝采纳时不能改动本机时钟");
        assert!(clock.recv(&Hlc::new(1000 + MAX_DRIFT_MS, 0, "n2"), 1000));
    }

    #[test]
    fn restore_never_goes_backwards_and_keeps_node() {
        let mut clock = Clock::new("n1");
        let saved = clock.send(5000);
        let mut again = Clock::restore("n1", Some(&saved.to_string()));
        let next = again.send(1000); // 重启后系统时间更早
        assert!(next > saved);
        assert_eq!(Clock::restore("n9", Some("garbage")).last().wall_ms, 0);
        assert_eq!(
            Clock::restore("n9", Some(&saved.to_string())).last().node,
            "n9"
        );
    }

    #[test]
    fn parse_only_accepts_canonical_bounded_timestamps() {
        // 计数器 / 墙钟超出定长格式、或不是规范写法的，全部无效。
        for bad in [
            "0000000000100-100000-n",
            "0000000000100-4294967295-n",
            "10000000000000-00000-n",
            "100-00000-n",
            "+000000000100-00000-n",
            "0000000000100-0-n",
        ] {
            assert!(Hlc::parse(bad).is_none(), "{bad}");
        }
        assert!(Hlc::parse("0000000000100-99999-n").is_some());
    }

    #[test]
    fn counter_never_overflows_or_breaks_ordering() {
        // 别人带来的最大计数器不会让本机 panic（debug 构建里 u32 加法会溢出），顺序仍然正确。
        let mut clock = Clock::new("n1");
        let remote = Hlc::new(2000, MAX_COUNTER, "n2");
        assert!(clock.recv(&remote, 1000));
        let a = clock.send(1000);
        let b = clock.send(1000);
        assert!(a > remote && b > a);
        assert!(a.counter <= MAX_COUNTER && b.counter <= MAX_COUNTER);
        assert!(a.to_string() < b.to_string());
        // 本机连续产生大量时间戳，计数器满后进位到下一毫秒。
        let mut clock = Clock::restore("n1", Some(&Hlc::new(5, MAX_COUNTER - 1, "n1").to_string()));
        let x = clock.send(0);
        let y = clock.send(0);
        assert_eq!((x.wall_ms, x.counter), (5, MAX_COUNTER));
        assert_eq!((y.wall_ms, y.counter), (6, 0));
        assert!(x.to_string() < y.to_string());
    }

    #[test]
    fn clamp_limits_future_wall_time() {
        let mut h = Hlc::new(9_000, 3, "n");
        assert!(h.clamp_wall(5_000));
        assert_eq!((h.wall_ms, h.counter), (5_000, 3));
        assert!(!h.clamp_wall(5_000));
    }
}
