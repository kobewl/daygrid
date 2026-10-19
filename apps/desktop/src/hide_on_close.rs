//! macOS 关窗即隐藏，含全屏的情况。
//!
//! 全屏时直接 `hide()` 会留下一个黑色的全屏空间（tauri-apps/tauri#10580）。所以全屏时先
//! `set_fullscreen(false)`，等 AppKit 发出 `NSWindowDidExitFullScreenNotification`（退出动画
//! 结束）再隐藏。tao 的 `is_fullscreen()` 在调用 `set_fullscreen` 时就变成 false，`Resized`
//! 也会在动画中途触发，都不能用来判断动画结束。
use block2::RcBlock;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSNotification, NSNotificationCenter, NSOperationQueue, ns_string};
use std::{
    ptr::NonNull,
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};

/// 等待退出全屏后隐藏：从什么时候开始等。
static PENDING: Mutex<Option<Instant>> = Mutex::new(None);
/// 超过这个时间还没收到通知，视为退出全屏失败，下次关窗直接隐藏。
const EXIT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CloseAction {
    Hide,
    ExitFullscreenThenHide,
    /// 退出全屏的动画还在播放（例如连按了两次 ⌘W）：什么也不做。
    Wait,
}

pub(crate) fn close_action(fullscreen: bool, pending_for: Option<Duration>) -> CloseAction {
    match pending_for {
        Some(age) if age < EXIT_TIMEOUT => CloseAction::Wait,
        _ if fullscreen => CloseAction::ExitFullscreenThenHide,
        _ => CloseAction::Hide,
    }
}

fn pending() -> std::sync::MutexGuard<'static, Option<Instant>> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner())
}

fn hide_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
        let _ = window.emit("daygrid://window-hidden", ());
    }
}

/// 主窗口的 `CloseRequested`（调用方已经 `prevent_close`）。
pub(crate) fn on_close_requested(window: &tauri::Window) {
    let since = pending().map(|at| at.elapsed());
    match close_action(window.is_fullscreen().unwrap_or(false), since) {
        CloseAction::Hide => {
            cancel_pending();
            hide_main(window.app_handle());
        }
        CloseAction::ExitFullscreenThenHide => {
            *pending() = Some(Instant::now());
            if let Err(e) = window.set_fullscreen(false) {
                tracing::warn!(error = %e, "exit fullscreen failed; hiding directly");
                cancel_pending();
                hide_main(window.app_handle());
            }
        }
        CloseAction::Wait => {}
    }
}

/// 在 setup 里调用一次：主窗口退出全屏时，如果是关窗触发的，就在动画结束后隐藏。
/// 用户自己点绿灯退出全屏时没有待隐藏标记，什么也不做。
pub(crate) fn observe_fullscreen_exit(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let ns_window = match window.ns_window() {
        Ok(ptr) => ptr,
        Err(e) => {
            tracing::warn!(error = %e, "no NSWindow; fullscreen close will hide directly");
            return;
        }
    };
    let app = app.clone();
    let block = RcBlock::new(move |_: NonNull<NSNotification>| {
        let fresh = pending()
            .take()
            .is_some_and(|at| at.elapsed() < EXIT_TIMEOUT);
        if fresh {
            // 推迟到下一轮事件循环：让 tao 先做完它自己的 windowDidExitFullScreen: 收尾。
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || hide_main(&handle));
        }
    });
    // SAFETY: ns_window 是主窗口的 NSWindow；macOS 上主窗口从不真正关闭（关窗只是隐藏），
    // 指针在进程生命周期内有效。队列是主队列，block 只在主线程执行；它捕获的 AppHandle
    // 是 Send + Sync，PENDING 由 Mutex 保护。
    let observer = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(ns_string!("NSWindowDidExitFullScreenNotification")),
            Some(&*(ns_window as *const AnyObject)),
            Some(&NSOperationQueue::mainQueue()),
            &block,
        )
    };
    // 观察者一直保留到进程退出，从不移除。
    std::mem::forget(observer);
}

/// 点程序坞图标重新显示窗口时调用：放弃还没完成的「退出全屏后隐藏」。
pub(crate) fn cancel_pending() {
    *pending() = None;
}

#[cfg(test)]
mod tests {
    use super::{CloseAction::*, close_action};
    use std::time::Duration;

    #[test]
    fn close_action_waits_for_a_fresh_fullscreen_exit() {
        let young = Some(Duration::from_millis(100));
        let stale = Some(Duration::from_secs(5));
        for (fullscreen, pending, want) in [
            (false, None, Hide),
            (true, None, ExitFullscreenThenHide),
            (false, young, Wait),
            (true, young, Wait),
            (false, stale, Hide),
            (true, stale, ExitFullscreenThenHide),
        ] {
            assert_eq!(
                close_action(fullscreen, pending),
                want,
                "{fullscreen} {pending:?}"
            );
        }
    }
}
