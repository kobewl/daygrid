//! 事项提醒的桌面部分：一个常驻的 `reminders` 线程和原生通知。
//!
//! 调度逻辑在 `daygrid::reminder::run_scheduler`；这里只提供真实环境：系统时钟、
//! 监视数据目录的 `HintWatcher`、经 IPC 调用 `desktop_take_due_reminders`、用通知插件弹横幅。
//! 日志里只记数量和错误码，不记事项标题和备注。
use daygrid::{
    ipc,
    reminder::{self, DEFAULT_SOUND, HintWatcher, SchedulerEnv, TakeDue},
};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

/// mac-notification-sys 的 delegate 没有实现 shouldPresentNotification:，
/// 日格在前台时系统会吞掉横幅。给它的类补上这个方法，固定返回 YES。
/// 返回 true 表示类存在且方法已就位（新增成功，或者已经存在）。
#[cfg(target_os = "macos")]
pub(crate) fn present_while_frontmost() -> bool {
    use objc2::{
        ffi,
        runtime::{AnyClass, AnyObject, Bool, Imp, Sel},
        sel,
    };
    extern "C-unwind" fn yes(_: &AnyObject, _: Sel, _: *mut AnyObject, _: *mut AnyObject) -> Bool {
        Bool::YES
    }
    let Some(cls) = AnyClass::get(c"NotificationCenterDelegate") else {
        tracing::warn!(
            "notification delegate class not found; banners may be hidden while Daygrid is active"
        );
        return false;
    };
    let name = sel!(userNotificationCenter:shouldPresentNotification:);
    // BOOL 在 arm64 上是 bool（编码 B），在 x86_64 上是 signed char（编码 c）。
    #[cfg(target_arch = "aarch64")]
    const TYPES: &std::ffi::CStr = c"B@:@@";
    #[cfg(not(target_arch = "aarch64"))]
    const TYPES: &std::ffi::CStr = c"c@:@@";
    // SAFETY: cls 是已注册的 ObjC 类；yes 的签名与 (id self, SEL _cmd, id center, id note) -> BOOL 一致，
    // 转成 objc2 的无参 Imp 只是 ObjC 运行时要求的擦除写法；该类原本没有这个方法，
    // class_addMethod 只会新增，已存在时返回 NO 且不替换。
    let added = unsafe {
        ffi::class_addMethod(
            cls as *const AnyClass as *mut AnyClass,
            name,
            std::mem::transmute::<
                extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject, *mut AnyObject) -> Bool,
                Imp,
            >(yes),
            TYPES.as_ptr(),
        )
    };
    let present = added.as_bool() || cls.instance_method(name).is_some();
    tracing::debug!(
        added = added.as_bool(),
        present,
        "notification present-while-frontmost hook"
    );
    present
}

/// 启动唯一的提醒线程（在 setup 里调用一次）。
pub fn spawn(app: AppHandle) {
    let started = std::thread::Builder::new()
        .name("reminders".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&app))).is_err() {
                tracing::error!("reminder thread panicked; reminders paused until restart");
            }
        });
    if started.is_err() {
        tracing::error!("reminder thread failed to start");
    }
}

fn run(app: &AppHandle) {
    let dir = match ipc::data_dir().and_then(|d| ipc::prepare(&d).map(|_| d)) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(code = %e.code, "reminders disabled: data dir unavailable");
            return;
        }
    };
    let token = crate::desktop_token(&dir);
    let watcher = new_watcher(&dir, true);
    let mut env = DesktopEnv {
        app,
        dir,
        token,
        watcher,
        hint_warned: false,
    };
    reminder::run_scheduler(&mut env);
}

/// 第一次失败记 warn；之后降级期间每轮重建，失败不再记，成功记 info。
fn new_watcher(dir: &Path, first: bool) -> Option<HintWatcher> {
    match HintWatcher::new(dir) {
        Ok(w) => {
            if !first {
                tracing::info!("hint watcher restored");
            }
            Some(w)
        }
        Err(e) => {
            if first {
                tracing::warn!(kind = ?e.kind(), "hint watcher unavailable; reminder changes may be noticed up to 60s late");
            }
            None
        }
    }
}

struct DesktopEnv<'a> {
    app: &'a AppHandle,
    dir: PathBuf,
    token: &'static str,
    watcher: Option<HintWatcher>,
    hint_warned: bool,
}

impl SchedulerEnv for DesktopEnv<'_> {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .unwrap_or(0)
    }

    fn wait(&mut self, timeout: Duration) -> bool {
        match &self.watcher {
            Some(w) => w.wait(timeout),
            None => {
                // 降级：按同样的等待时间睡，到点准时性不受影响；每轮尝试重建监视器。
                std::thread::sleep(timeout);
                self.watcher = new_watcher(&self.dir, false);
                false
            }
        }
    }

    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }

    fn read_hint(&mut self) -> Option<Option<i64>> {
        let hint = reminder::read_hint(&self.dir);
        if hint.is_none() {
            if self.dir.join(reminder::HINT_FILE).exists() {
                if !self.hint_warned {
                    self.hint_warned = true;
                    tracing::warn!("reminder hint file is invalid");
                }
            } else {
                tracing::debug!("reminder hint file missing");
            }
        }
        hint
    }

    fn take_and_notify(&mut self) -> Result<Option<i64>, ()> {
        let value = ipc::call(
            &self.dir,
            "desktop",
            self.token,
            "desktop_take_due_reminders",
            json!({}),
        );
        if value["ok"] != true {
            tracing::warn!(
                code = value["error"]["code"].as_str().unwrap_or("-"),
                "take due reminders failed"
            );
            return Err(());
        }
        let Ok(r) = serde_json::from_value::<TakeDue>(value["data"].clone()) else {
            tracing::warn!(code = "VERSION_UNSUPPORTED", "take due reminders failed");
            return Err(());
        };
        let plan = reminder::plan_notifications(&r.reminders, &r.today);
        for (title, body) in &plan {
            // macOS 上 show() 只是把投递放进异步任务，固定返回 Ok；投递失败无法检测（at-most-once）。
            let shown = self
                .app
                .notification()
                .builder()
                .title(title)
                .body(body)
                .sound(DEFAULT_SOUND)
                .show();
            if shown.is_err() {
                tracing::warn!("notification request failed");
            }
        }
        if !plan.is_empty() {
            tracing::debug!(n = plan.len(), "notifications requested");
        }
        Ok(r.next_due_ms)
    }
}

/// 设置页「发送测试通知」。macOS 上插件无法反馈投递失败，固定返回 Ok。
#[tauri::command]
pub fn reminder_test(app: AppHandle) -> Result<(), String> {
    let shown = app
        .notification()
        .builder()
        .title("日格提醒测试")
        .body("看到这条通知，说明提醒可以正常显示。")
        .sound(DEFAULT_SOUND)
        .show();
    #[cfg(target_os = "macos")]
    {
        if shown.is_err() {
            tracing::warn!("test notification request failed");
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    shown.map_err(|e| e.to_string())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use objc2::runtime::AnyClass;

    #[test]
    fn delegate_class_gains_should_present_notification() {
        // 让 notify.m 被链接进测试二进制（不发送通知，无界面也能跑）。
        let _ = mac_notification_sys::get_bundle_identifier_or_default("Finder");
        assert!(AnyClass::get(c"NotificationCenterDelegate").is_some());
        assert!(super::present_while_frontmost());
        // 幂等：已经存在时仍返回 true
        assert!(super::present_while_frontmost());
    }
}
