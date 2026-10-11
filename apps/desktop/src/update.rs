//! 手动检查，安装用户看到的那个版本；更新包由 Tauri 验签后才安装。
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::{Update, UpdaterExt};

#[derive(Default)]
pub struct UpdateState {
    pending: Mutex<Option<Update>>,
    installing: AtomicBool,
    installed: AtomicBool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    current_version: String,
    version: String,
    notes: String,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    phase: &'static str,
    downloaded: u64,
    total: Option<u64>,
}

fn check_error(raw: &str) -> String {
    if raw.contains("Could not fetch a valid release JSON") {
        "暂时无法读取更新信息，请稍后重试，或前往发布页手动下载。".into()
    } else if raw.contains("platform") || raw.contains("target") {
        "新版本没有适用于这台电脑的更新包，请查看发布页。".into()
    } else {
        format!("检查更新失败：{raw}")
    }
}

#[tauri::command]
pub async fn check_update(
    app: AppHandle,
    state: State<'_, UpdateState>,
) -> Result<Option<UpdateInfo>, String> {
    if state.installing.load(Ordering::SeqCst) {
        return Err("正在安装更新，请稍候。".into());
    }
    let mut update = app
        .updater_builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| check_error(&e.to_string()))?
        .check()
        .await
        .map_err(|e| check_error(&e.to_string()))?;
    if let Some(update) = &mut update {
        // 检查用较短超时；下载留出更长时间。
        update.timeout = Some(Duration::from_secs(180));
    }
    let info = update.as_ref().map(|u| UpdateInfo {
        current_version: u.current_version.clone(),
        version: u.version.clone(),
        notes: u.body.clone().unwrap_or_default(),
    });
    *state
        .pending
        .lock()
        .map_err(|_| "更新状态不可用，请重新打开应用。")? = update;
    Ok(info)
}

struct InstallGuard<'a>(&'a AtomicBool);
impl Drop for InstallGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    state: State<'_, UpdateState>,
    version: String,
) -> Result<(), String> {
    if state
        .installing
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("正在安装更新，请勿重复操作。".into());
    }
    let _guard = InstallGuard(&state.installing);
    let update = state
        .pending
        .lock()
        .map_err(|_| "更新状态不可用。")?
        .as_ref()
        .filter(|u| u.version == version)
        .cloned()
        .ok_or("更新信息已变化，请重新检查更新。")?;

    // 先保存一份一致的数据库备份；不触碰正在运行的服务与 MCP 凭据。
    let backup = tauri::async_runtime::spawn_blocking(|| {
        super::dispatch("desktop_backup", serde_json::json!({}))
    })
    .await
    .map_err(|_| "更新前备份失败，请稍后重试。")?;
    if backup["ok"] != true {
        return Err("更新前备份失败，请检查数据目录后重试。".into());
    }

    let mut downloaded = 0u64;
    let progress_app = app.clone();
    let finish_app = app.clone();
    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = progress_app.emit(
                    "update:progress",
                    Progress {
                        phase: "downloading",
                        downloaded,
                        total: total.filter(|n| *n > 0),
                    },
                );
            },
            move || {
                let _ = finish_app.emit(
                    "update:progress",
                    Progress {
                        phase: "installing",
                        downloaded: 0,
                        total: None,
                    },
                );
            },
        )
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "update installation failed");
            format!("更新未完成：{e}。可以重试或到发布页手动下载。")
        })?;
    state.installed.store(true, Ordering::SeqCst);
    tracing::info!(version, "update installed, restarting");
    app.restart();
}

#[tauri::command]
pub fn restart_after_update(app: AppHandle, state: State<'_, UpdateState>) -> Result<(), String> {
    if !state.installed.load(Ordering::SeqCst) {
        return Err("尚未安装更新。".into());
    }
    app.restart();
}

#[tauri::command]
pub fn open_release_page() -> Result<(), String> {
    // 地址固定为本项目，不接受前端提供的 URL。
    #[cfg(target_os = "macos")]
    let command = "open";
    #[cfg(not(target_os = "macos"))]
    let command = "xdg-open";
    std::process::Command::new(command)
        .arg("https://github.com/kobewl/daygrid/releases/latest")
        .spawn()
        .map_err(|e| format!("无法打开发布页：{e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_install_releases_the_guard() {
        let state = UpdateState::default();
        state.installing.store(true, Ordering::SeqCst);
        {
            let _guard = InstallGuard(&state.installing);
        }
        assert!(!state.installing.load(Ordering::SeqCst));
        assert!(!state.installed.load(Ordering::SeqCst));
    }
}
