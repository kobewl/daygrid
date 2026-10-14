//! 客户端同步引擎：后台线程里的 push → pull 循环，加一条 SSE 通知通道和在线状态上报。
//!
//! 用 Java 的话说：这是一个"定时任务 + 长连接监听器"的组合——
//! - 主循环 ≈ `ScheduledExecutorService`，被本地写入 / 服务端 poke 唤醒，否则每 30 秒兜底拉一次；
//! - SSE 线程 ≈ Nacos 的配置变更长连接：只收到"变了"的通知，具体数据仍由主循环去拉取；
//! - **网络请求一律在锁外**，只在应用结果时短暂持有 `Mutex<Store>`，不会拖住界面和 MCP 的请求。
use super::{
    JoinResponse, MAX_PUSH_OPS, MeResponse, PullResponse, PushRequest, PushResponse,
    runtime::{ConnectRequest, LocalPresence, Runtime},
};
use crate::{
    Error,
    store::{Store, SyncConfig},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use ureq::Agent;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_FALLBACK: Duration = Duration::from_secs(30);
const PRESENCE_HEARTBEAT: Duration = Duration::from_secs(15);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const AUTH_RETRY: Duration = Duration::from_secs(300);
/// 单次同步最多连续推送的批数，防止一直有新写入时永远不去拉取。
const MAX_PUSH_BATCHES: usize = 20;

/// 网络层错误：区分"暂时连不上"（可重试）与"服务器明确拒绝"。
#[derive(Debug, Clone)]
pub enum NetError {
    Offline(String),
    Server {
        status: u16,
        code: String,
        message: String,
    },
    /// 本地存储出错（应用结果时）。
    Local(String),
}

impl NetError {
    pub fn message(&self) -> String {
        match self {
            NetError::Offline(m) | NetError::Local(m) => m.clone(),
            NetError::Server { message, .. } => message.clone(),
        }
    }
    pub fn into_error(self) -> Error {
        match self {
            NetError::Offline(m) => Error::new("SERVICE_UNAVAILABLE", m),
            NetError::Local(m) => Error::new("STORAGE_ERROR", m),
            NetError::Server {
                status, message, ..
            } if status == 401 || status == 403 => Error::new("PERMISSION_DENIED", message),
            NetError::Server { message, .. } => Error::new("VALIDATION_ERROR", message),
        }
    }
}

impl From<Error> for NetError {
    fn from(e: Error) -> Self {
        NetError::Local(e.message)
    }
}

pub fn agent(timeout: Option<Duration>) -> Agent {
    let config = Agent::config_builder()
        .timeout_global(timeout)
        .http_status_as_error(false)
        .user_agent(format!("daygrid/{}", crate::VERSION))
        .build();
    Agent::new_with_config(config)
}

fn offline(e: ureq::Error) -> NetError {
    NetError::Offline(format!("无法连接协同服务器：{e}"))
}

/// 读取响应：2xx 解析为 `T`；其他状态码解析服务端的统一错误体。
fn finish<T: DeserializeOwned>(mut resp: ureq::http::Response<ureq::Body>) -> Result<T, NetError> {
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return resp
            .body_mut()
            .read_json::<T>()
            .map_err(|e| NetError::Offline(format!("服务器响应无法解析：{e}")));
    }
    let body: Value = resp.body_mut().read_json().unwrap_or(Value::Null);
    let code = body["error"]["code"]
        .as_str()
        .unwrap_or("HTTP_ERROR")
        .to_string();
    let message = body["error"]["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| match status {
            401 => "令牌无效或已被撤销。".to_string(),
            403 => "没有权限。".to_string(),
            404 => "服务器上找不到该资源。".to_string(),
            s => format!("服务器返回 {s}。"),
        });
    Err(NetError::Server {
        status,
        code,
        message,
    })
}

fn get<T: DeserializeOwned>(agent: &Agent, url: &str, token: &str) -> Result<T, NetError> {
    let resp = agent
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .call()
        .map_err(offline)?;
    finish(resp)
}

fn post<T: DeserializeOwned>(
    agent: &Agent,
    url: &str,
    token: Option<&str>,
    body: &impl Serialize,
) -> Result<T, NetError> {
    let mut req = agent.post(url);
    if let Some(token) = token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    finish(req.send_json(body).map_err(offline)?)
}

pub fn join(url: &str, invite: &str, display_name: &str) -> Result<JoinResponse, NetError> {
    post(
        &agent(Some(REQUEST_TIMEOUT)),
        &format!("{url}/v1/join"),
        None,
        &json!({"invite":invite,"display_name":display_name}),
    )
}

pub fn me(url: &str, token: &str) -> Result<MeResponse, NetError> {
    get(
        &agent(Some(REQUEST_TIMEOUT)),
        &format!("{url}/v1/me"),
        token,
    )
}

pub fn push(cfg: &SyncConfig, token: &str, req: &PushRequest) -> Result<PushResponse, NetError> {
    post(
        &agent(Some(REQUEST_TIMEOUT)),
        &format!("{}/v1/workspaces/{}/push", cfg.url, cfg.workspace_id),
        Some(token),
        req,
    )
}

pub fn pull(
    cfg: &SyncConfig,
    token: &str,
    since: i64,
    conflict_since: i64,
) -> Result<PullResponse, NetError> {
    get(
        &agent(Some(REQUEST_TIMEOUT)),
        &format!(
            "{}/v1/workspaces/{}/pull?since={since}&conflict_since={conflict_since}",
            cfg.url, cfg.workspace_id
        ),
        token,
    )
}

fn send_presence(cfg: &SyncConfig, token: &str, p: &LocalPresence) -> Result<(), NetError> {
    let _: Value = post(
        &agent(Some(Duration::from_secs(8))),
        &format!("{}/v1/workspaces/{}/presence", cfg.url, cfg.workspace_id),
        Some(token),
        &json!({"client_id":cfg.client_id,"date":p.date,"item_id":p.item_id,"editing":p.editing}),
    )?;
    Ok(())
}

/// 生成邀请码（需要 owner 权限）。网络请求发生在锁外，由服务进程调用。
pub fn create_invite(
    cfg: &SyncConfig,
    token: &str,
    role: &str,
    ttl_hours: i64,
    max_uses: i64,
) -> Result<Value, NetError> {
    post(
        &agent(Some(Duration::from_secs(4))),
        &format!("{}/v1/workspaces/{}/invites", cfg.url, cfg.workspace_id),
        Some(token),
        &json!({"role":role,"ttl_hours":ttl_hours,"max_uses":max_uses}),
    )
}

fn lock(store: &Mutex<Store>) -> MutexGuard<'_, Store> {
    // 锁中毒时仍继续：SQLite 事务在 panic 展开时已回滚，数据是一致的。
    store.lock().unwrap_or_else(|e| e.into_inner())
}

/// 做一轮完整同步：先把本地修改推上去，再把服务端的变化拉下来。
pub fn sync_once(
    store: &Mutex<Store>,
    rt: &Runtime,
    cfg: &SyncConfig,
    token: &str,
) -> Result<(), NetError> {
    for _ in 0..MAX_PUSH_BATCHES {
        let batch = lock(store).sync_prepare_push(MAX_PUSH_OPS)?;
        let Some(batch) = batch else { break };
        let resp = match push(
            cfg,
            token,
            &PushRequest {
                client_id: batch.client_id.clone(),
                ops: batch.ops.clone(),
            },
        ) {
            Err(NetError::Server {
                status: 403,
                code,
                message,
            }) if code == "READ_ONLY" => {
                // 角色被改成只读：本地放弃无法上传的修改（记成冲突），然后继续拉取，不当作错误。
                tracing::warn!(%message, "role downgraded to viewer");
                lock(store).sync_change_role("viewer")?;
                continue;
            }
            other => other?,
        };
        let stats = lock(store).sync_apply_push(&batch, &resp)?;
        tracing::debug!(
            ops = batch.ops.len(),
            changed = stats.changed,
            conflicts = stats.conflicts,
            rejected = stats.rejected,
            "sync push applied"
        );
    }
    let mut last = -1;
    loop {
        let (since, conflict_since) = lock(store).sync_cursors()?;
        if since == last {
            break; // 游标没前进：防止服务端异常时死循环
        }
        last = since;
        let resp = pull(cfg, token, since, conflict_since)?;
        let stats = lock(store).sync_apply_pull(&resp)?;
        tracing::debug!(
            items = resp.items.len(),
            changed = stats.changed,
            conflicts = stats.conflicts,
            "sync pull applied"
        );
        if !resp.has_more {
            break;
        }
    }
    // 每轮都会变的时间戳不应该触发界面刷新。
    rt.update_quiet(|s| {
        s.last_sync_at =
            Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    });
    Ok(())
}

/// 令牌加入了多个团队时必须明确指定，不能悄悄选第一个。
fn pick_workspace(
    list: &[super::MeWorkspace],
    wanted: Option<&str>,
) -> Result<super::MeWorkspace, NetError> {
    let fail = |message: String| NetError::Server {
        status: 404,
        code: "NO_WORKSPACE".into(),
        message,
    };
    let names = || {
        list.iter()
            .map(|w| format!("「{}」", w.name))
            .collect::<Vec<_>>()
            .join("、")
    };
    match (list.len(), wanted) {
        (0, _) => Err(fail("这个令牌没有加入任何团队日历。".into())),
        (1, None) => Ok(list[0].clone()),
        (_, None) => Err(fail(format!(
            "这个令牌加入了多个团队日历：{}。请填写要连接的团队名称。",
            names()
        ))),
        (_, Some(w)) => list
            .iter()
            .find(|x| x.id == w || x.name == w)
            .cloned()
            .ok_or_else(|| fail(format!("没有名为「{w}」的团队日历，可选：{}。", names()))),
    }
}

/// 处理界面发起的连接请求：换令牌 → 确定 workspace → 写入配置并上传本机事项。
fn handle_connect(store: &Mutex<Store>, rt: &Runtime, req: ConnectRequest) {
    let result = (|| -> Result<(SyncConfig, String), NetError> {
        let (token, user, workspace, role) = match (&req.invite, &req.token) {
            (Some(invite), _) => {
                let j = join(&req.url, invite, &req.display_name)?;
                (j.token, j.user, j.workspace, j.role)
            }
            (None, Some(token)) => {
                let info = me(&req.url, token)?;
                let first = pick_workspace(&info.workspaces, req.workspace.as_deref())?;
                (
                    token.clone(),
                    info.user,
                    super::WorkspaceInfo {
                        id: first.id,
                        name: first.name,
                    },
                    first.role,
                )
            }
            _ => unreachable!("validated by desktop_sync_connect"),
        };
        let cfg = SyncConfig {
            url: req.url.clone(),
            workspace_id: workspace.id,
            workspace_name: workspace.name,
            user_id: user.id,
            user_name: user.name,
            role,
            client_id: uuid::Uuid::new_v4().simple().to_string(),
        };
        Ok((cfg, token))
    })();
    match result.and_then(|(cfg, token)| {
        let shared = lock(store).sync_link(&cfg, &token, req.share_existing)?;
        Ok((cfg, shared))
    }) {
        Ok((cfg, shared)) => {
            tracing::info!(workspace = %cfg.workspace_name, shared, "sync connected");
            rt.update(|s| {
                s.state = "syncing";
                s.last_error = None;
            });
        }
        Err(e) => {
            tracing::warn!(error = %e.message(), "sync connect failed");
            rt.update(|s| {
                s.state = "error";
                s.last_error = Some(e.message());
            });
        }
    }
}

/// SSE 通知线程的句柄。`stop` 置位后线程在下一次读到数据或超时时退出。
struct Events {
    client_id: String,
    stop: Arc<AtomicBool>,
}

impl Drop for Events {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn start_events(cfg: &SyncConfig, token: &str, rt: Arc<Runtime>) -> Events {
    let stop = Arc::new(AtomicBool::new(false));
    let (cfg2, token2, stop2) = (cfg.clone(), token.to_string(), stop.clone());
    let _ = std::thread::Builder::new()
        .name("daygrid-sync-events".into())
        .spawn(move || events_loop(&cfg2, &token2, &rt, &stop2));
    Events {
        client_id: cfg.client_id.clone(),
        stop,
    }
}

fn events_loop(cfg: &SyncConfig, token: &str, rt: &Runtime, stop: &AtomicBool) {
    let mut delay = Duration::from_secs(1);
    while !stop.load(Ordering::Relaxed) {
        let connected_at = Instant::now();
        let outcome = events_once(cfg, token, rt, stop);
        rt.update(|s| s.events_connected = false);
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if connected_at.elapsed() > Duration::from_secs(30) {
            delay = Duration::from_secs(1);
        }
        if let Err(e) = outcome {
            tracing::debug!(error = %e, "events stream ended");
        }
        std::thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

fn events_once(
    cfg: &SyncConfig,
    token: &str,
    rt: &Runtime,
    stop: &AtomicBool,
) -> Result<(), String> {
    // 流式响应：不设总超时，只限制连接阶段；断线靠服务端 15 秒一次的 keepalive 与读错误发现。
    let config = Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(20)))
        .timeout_recv_body(Some(Duration::from_secs(300)))
        .http_status_as_error(false)
        .build();
    let agent = Agent::new_with_config(config);
    let resp = agent
        .get(&format!(
            "{}/v1/workspaces/{}/events",
            cfg.url, cfg.workspace_id
        ))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "text/event-stream")
        .call()
        .map_err(|e| e.to_string())?;
    if resp.status().as_u16() != 200 {
        return Err(format!("status {}", resp.status()));
    }
    rt.update(|s| s.events_connected = true);
    rt.wake(); // 刚（重新）连上：补拉一次，避免断线期间漏掉变更
    let mut reader = BufReader::new(resp.into_body().into_reader());
    let (mut event, mut data) = (String::new(), String::new());
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let text = line.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            if !event.is_empty() {
                handle_event(cfg, rt, &event, &data);
            }
            event.clear();
            data.clear();
        } else if let Some(v) = text.strip_prefix("event:") {
            event = v.trim().to_string();
        } else if let Some(v) = text.strip_prefix("data:") {
            data.push_str(v.trim_start());
        }
        // 以 ':' 开头的是 keepalive 注释，忽略。
    }
}

fn handle_event(cfg: &SyncConfig, rt: &Runtime, event: &str, data: &str) {
    match event {
        "poke" => rt.wake(),
        "presence" => {
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                return;
            };
            let peers: Vec<super::Peer> =
                serde_json::from_value(v["peers"].clone()).unwrap_or_default();
            rt.update(|s| {
                s.peers = peers
                    .into_iter()
                    .filter(|p| p.client_id != cfg.client_id)
                    .collect();
            });
        }
        _ => {}
    }
}

/// 启动同步线程。线程随服务进程存在；未连接协同时几乎不占资源（每分钟醒一次）。
pub fn spawn(store: Arc<Mutex<Store>>) {
    let rt = lock(&store).runtime();
    let _ = std::thread::Builder::new()
        .name("daygrid-sync".into())
        .spawn(move || run(&store, rt));
}

fn run(store: &Mutex<Store>, rt: Arc<Runtime>) {
    let mut events: Option<Events> = None;
    let mut backoff = Duration::from_secs(1);
    let mut last_presence = Instant::now() - PRESENCE_HEARTBEAT;
    loop {
        if let Some(req) = rt.take_connect() {
            handle_connect(store, &rt, req);
        }
        let (cfg, token) = {
            let s = lock(store);
            (
                s.sync_config().ok().flatten(),
                s.sync_token().ok().flatten(),
            )
        };
        let (Some(cfg), Some(token)) = (cfg, token) else {
            events = None;
            rt.wait(Duration::from_secs(60));
            continue;
        };
        if events.as_ref().is_none_or(|e| e.client_id != cfg.client_id) {
            events = Some(start_events(&cfg, &token, rt.clone()));
        }
        rt.update(|s| {
            if matches!(s.state, "off" | "connecting" | "offline") {
                s.state = "syncing";
            }
        });
        match sync_once(store, &rt, &cfg, &token) {
            Ok(()) => {
                backoff = Duration::from_secs(1);
                rt.update(|s| {
                    s.state = "online";
                    s.last_error = None;
                });
            }
            Err(e) => {
                let auth =
                    matches!(e, NetError::Server { status, .. } if status == 401 || status == 403);
                tracing::warn!(error = %e.message(), "sync failed");
                rt.update(|s| {
                    s.state = if auth { "error" } else { "offline" };
                    s.last_error = Some(e.message());
                });
                if auth {
                    // 令牌被撤销 / 被移出团队：重试也不会好，只低频确认（本地写入仍会立即唤醒重试一次）。
                    rt.wait(AUTH_RETRY);
                } else {
                    rt.wait(backoff);
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
                continue;
            }
        }
        if rt.take_presence_dirty() || last_presence.elapsed() >= PRESENCE_HEARTBEAT {
            let _ = send_presence(&cfg, &token, &rt.presence());
            last_presence = Instant::now();
        }
        // 没有 SSE 时缩短兜底间隔，保证协作者的变化仍能较快看到。
        let wait = if rt.status().events_connected {
            POLL_FALLBACK
        } else {
            Duration::from_secs(5)
        };
        rt.wait(wait.min(PRESENCE_HEARTBEAT));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::MeWorkspace;

    fn ws(id: &str, name: &str) -> MeWorkspace {
        MeWorkspace {
            id: id.into(),
            name: name.into(),
            role: "editor".into(),
        }
    }

    #[test]
    fn a_token_with_several_teams_must_say_which_one() {
        let one = [ws("1", "产品组")];
        assert_eq!(pick_workspace(&one, None).unwrap().id, "1");
        let two = [ws("1", "产品组"), ws("2", "研发组")];
        let err = pick_workspace(&two, None).unwrap_err().message();
        assert!(err.contains("产品组") && err.contains("研发组"), "{err}");
        assert_eq!(pick_workspace(&two, Some("研发组")).unwrap().id, "2");
        assert_eq!(pick_workspace(&two, Some("1")).unwrap().id, "1");
        assert!(pick_workspace(&two, Some("不存在")).is_err());
        assert!(pick_workspace(&[], None).is_err());
    }
}
