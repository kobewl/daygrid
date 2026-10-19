//! HTTP 层：路由、认证、SSE 通知与在线状态。
//!
//! 数据库调用都是阻塞的 SQLite，统一放进 `spawn_blocking`，不占用异步运行时的工作线程
//! （Java 类比：Netty 的 EventLoop 里不能做阻塞 JDBC，要丢给业务线程池）。
use crate::db::{ApiError, ApiResult, ServerDb, User};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{delete, get, post},
};
use daygrid::sync::{MAX_PUSH_OPS, OpStatus, Peer, PushRequest};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::broadcast;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

/// 在线状态的有效期：超过这么久没有心跳就视为离线。
pub const PRESENCE_TTL: Duration = Duration::from_secs(45);
const MAX_BODY: usize = 2 * 1024 * 1024;

/// 广播给所有 SSE 连接的通知。
#[derive(Debug, Clone)]
pub enum Notice {
    /// "workspace 有新变更了"——不带数据，客户端收到后自己去拉取（Replicache 的 poke）。
    Poke {
        ws: String,
        head: i64,
    },
    Presence {
        ws: String,
        peers: Vec<Peer>,
    },
}

struct PresenceEntry {
    peer: Peer,
    seen: Instant,
}

pub struct AppState {
    db: Arc<Mutex<ServerDb>>,
    notices: broadcast::Sender<Notice>,
    presence: Mutex<HashMap<(String, String), PresenceEntry>>,
}

impl AppState {
    pub fn new(db: ServerDb) -> Arc<Self> {
        Arc::new(Self {
            db: Arc::new(Mutex::new(db)),
            notices: broadcast::channel(256).0,
            presence: Mutex::new(HashMap::new()),
        })
    }

    fn peers(&self, ws: &str) -> Vec<Peer> {
        let map = self.presence.lock().unwrap_or_else(|e| e.into_inner());
        let mut peers: Vec<Peer> = map
            .iter()
            .filter(|((w, _), e)| w == ws && e.seen.elapsed() < PRESENCE_TTL)
            .map(|(_, e)| e.peer.clone())
            .collect();
        peers.sort_by(|a, b| (&a.user, &a.client_id).cmp(&(&b.user, &b.client_id)));
        peers
    }

    /// 清理过期的在线状态；返回发生变化的 workspace。
    pub fn purge_presence(&self) -> Vec<String> {
        let mut map = self.presence.lock().unwrap_or_else(|e| e.into_inner());
        let mut touched = Vec::new();
        map.retain(|(ws, _), e| {
            let keep = e.seen.elapsed() < PRESENCE_TTL;
            if !keep && !touched.contains(ws) {
                touched.push(ws.clone());
            }
            keep
        });
        drop(map);
        for ws in &touched {
            let _ = self.notices.send(Notice::Presence {
                ws: ws.clone(),
                peers: self.peers(ws),
            });
        }
        touched
    }

    async fn db<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut ServerDb) -> ApiResult<T> + Send + 'static,
    ) -> ApiResult<T> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || f(&mut db.lock().unwrap_or_else(|e| e.into_inner())))
            .await
            .map_err(|_| ApiError::new(500, "INTERNAL", "服务端任务失败。"))?
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (
            status,
            Json(json!({"error":{"code":self.code,"message":self.message}})),
        )
            .into_response()
    }
}

fn bearer(headers: &HeaderMap) -> ApiResult<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(ApiError::unauthorized)
}

/// 认证并检查成员身份；返回 (用户, 角色)。
async fn authorize(state: &AppState, headers: &HeaderMap, ws: &str) -> ApiResult<(User, String)> {
    let token = bearer(headers)?;
    let ws = ws.to_string();
    state
        .db(move |db| {
            let user = db.authenticate(&token)?;
            let role = db.role(&ws, &user.id)?;
            Ok((user, role))
        })
        .await
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/join", post(join))
        .route("/v1/me", get(me))
        .route("/v1/workspaces/{ws}", get(workspace))
        .route("/v1/workspaces/{ws}/invites", post(create_invite))
        .route(
            "/v1/workspaces/{ws}/members/{user}",
            delete(remove_member).put(set_member_role),
        )
        .route("/v1/workspaces/{ws}/push", post(push))
        .route("/v1/workspaces/{ws}/pull", get(pull))
        .route("/v1/workspaces/{ws}/events", get(events))
        .route("/v1/workspaces/{ws}/presence", post(presence))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(state)
}

/// 等待终止信号：Ctrl-C（SIGINT）或 `kill` / systemd / docker stop 发来的 SIGTERM。
pub async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).ok();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = async {
            match term.as_mut() {
                Some(t) => { t.recv().await; }
                None => std::future::pending::<()>().await,
            }
        } => {}
    }
}

/// 启动后台清理任务并开始服务；收到终止信号后不再接收新连接，给在途请求几秒钟收尾后退出。
pub async fn serve(listener: tokio::net::TcpListener, state: Arc<AppState>) -> std::io::Result<()> {
    let purger = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        loop {
            tick.tick().await;
            purger.purge_presence();
        }
    });
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = stop_tx.send(true);
    });
    let mut graceful = stop_rx.clone();
    let server = axum::serve(listener, router(state)).with_graceful_shutdown(async move {
        let _ = graceful.wait_for(|stopped| *stopped).await;
        tracing::info!("shutting down: draining in-flight requests");
    });
    // SSE 是长连接，优雅关闭会一直等它们：最多等 5 秒，然后直接结束。
    tokio::select! {
        result = server => result,
        _ = async {
            let _ = stop_rx.wait_for(|stopped| *stopped).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        } => Ok(()),
    }
}

fn now_ms() -> u64 {
    daygrid::sync::now_ms()
}

#[derive(Deserialize)]
struct JoinBody {
    invite: String,
    display_name: String,
}

async fn join(
    State(state): State<Arc<AppState>>,
    Json(body): Json<JoinBody>,
) -> ApiResult<Json<Value>> {
    let resp = state
        .db(move |db| db.join(&body.invite, &body.display_name, now_ms()))
        .await?;
    tracing::info!(user = %resp.user.name, "member joined");
    Ok(Json(serde_json::to_value(resp).unwrap()))
}

async fn me(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let token = bearer(&headers)?;
    let me = state
        .db(move |db| {
            let user = db.authenticate(&token)?;
            db.me(&user)
        })
        .await?;
    Ok(Json(serde_json::to_value(me).unwrap()))
}

async fn workspace(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(ws): Path<String>,
) -> ApiResult<Json<Value>> {
    let (user, role) = authorize(&state, &headers, &ws).await?;
    let ws2 = ws.clone();
    let (name, members, head) = state
        .db(move |db| {
            Ok((
                db.workspace_name(&ws2)?,
                db.members(&ws2)?,
                db.head_seq(&ws2)?,
            ))
        })
        .await?;
    Ok(Json(json!({
        "workspace":{"id":ws,"name":name},
        "you":{"id":user.id,"name":user.name,"role":role},
        "members":members.iter().map(|m| json!({"id":m.id,"name":m.name,"role":m.role,"joined_at":m.joined_at})).collect::<Vec<_>>(),
        "head_seq":head,
    })))
}

#[derive(Deserialize)]
struct InviteBody {
    role: String,
    ttl_hours: Option<i64>,
    max_uses: Option<i64>,
}

async fn create_invite(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(ws): Path<String>,
    Json(body): Json<InviteBody>,
) -> ApiResult<Json<Value>> {
    let (user, role) = authorize(&state, &headers, &ws).await?;
    if role != "owner" {
        return Err(ApiError::forbidden("只有 owner 可以生成邀请码。"));
    }
    let v = state
        .db(move |db| {
            db.create_invite(
                &ws,
                &user.id,
                &body.role,
                body.ttl_hours.unwrap_or(72),
                body.max_uses.unwrap_or(1),
                now_ms(),
            )
        })
        .await?;
    Ok(Json(v))
}

async fn remove_member(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((ws, target)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let (user, role) = authorize(&state, &headers, &ws).await?;
    if role != "owner" {
        return Err(ApiError::forbidden("只有 owner 可以移除成员。"));
    }
    state
        .db(move |db| db.remove_member(&ws, Some(&user.id), &target))
        .await?;
    Ok(Json(json!({"removed":true})))
}

#[derive(Deserialize)]
struct RoleBody {
    role: String,
}

async fn set_member_role(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((ws, target)): Path<(String, String)>,
    Json(body): Json<RoleBody>,
) -> ApiResult<Json<Value>> {
    let (_, role) = authorize(&state, &headers, &ws).await?;
    if role != "owner" {
        return Err(ApiError::forbidden("只有 owner 可以修改成员角色。"));
    }
    state
        .db(move |db| db.set_role(&ws, &target, &body.role))
        .await?;
    Ok(Json(json!({"updated":true})))
}

async fn push(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(ws): Path<String>,
    Json(body): Json<PushRequest>,
) -> ApiResult<Json<Value>> {
    let (user, role) = authorize(&state, &headers, &ws).await?;
    if role == "viewer" {
        return Err(ApiError::new(
            403,
            "READ_ONLY",
            "你是只读成员，不能提交修改。",
        ));
    }
    if body.client_id.is_empty() || body.client_id.len() > 64 {
        return Err(ApiError::bad("client_id 无效。"));
    }
    if body.ops.len() > MAX_PUSH_OPS {
        return Err(ApiError::bad(format!(
            "一次最多推送 {MAX_PUSH_OPS} 条操作。"
        )));
    }
    let ws2 = ws.clone();
    let resp = state
        .db(move |db| db.push(&ws2, &user, &body.ops, now_ms()))
        .await?;
    if resp
        .results
        .iter()
        .any(|r| matches!(r.status, OpStatus::Applied | OpStatus::Merged))
    {
        let _ = state.notices.send(Notice::Poke {
            ws,
            head: resp.head_seq,
        });
    }
    Ok(Json(serde_json::to_value(resp).unwrap()))
}

#[derive(Deserialize)]
struct PullQuery {
    since: Option<i64>,
    conflict_since: Option<i64>,
    limit: Option<usize>,
}

async fn pull(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(ws): Path<String>,
    Query(q): Query<PullQuery>,
) -> ApiResult<Json<Value>> {
    let (user, _) = authorize(&state, &headers, &ws).await?;
    let resp = state
        .db(move |db| {
            db.pull(
                &ws,
                &user,
                q.since.unwrap_or(0).max(0),
                q.conflict_since.unwrap_or(0).max(0),
                q.limit.unwrap_or(daygrid::sync::PULL_PAGE),
            )
        })
        .await?;
    Ok(Json(serde_json::to_value(resp).unwrap()))
}

#[derive(Deserialize)]
struct PresenceBody {
    client_id: String,
    date: Option<String>,
    item_id: Option<String>,
    #[serde(default)]
    editing: bool,
}

async fn presence(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(ws): Path<String>,
    Json(body): Json<PresenceBody>,
) -> ApiResult<Json<Value>> {
    let (user, _) = authorize(&state, &headers, &ws).await?;
    if body.client_id.is_empty() || body.client_id.len() > 64 {
        return Err(ApiError::bad("client_id 无效。"));
    }
    let peer = Peer {
        client_id: body.client_id.clone(),
        user: user.name.clone(),
        date: body.date.filter(|d| daygrid::domain::date(d).is_ok()),
        item_id: body.item_id.filter(|i| daygrid::domain::uuid(i).is_ok()),
        editing: body.editing,
    };
    let changed = {
        let mut map = state.presence.lock().unwrap_or_else(|e| e.into_inner());
        // 键里带上用户：别人即使用了同一个 client_id，也只会占用自己的那一条，不能顶替他人的在线状态。
        let key = (ws.clone(), format!("{}/{}", user.id, body.client_id));
        let changed = map
            .get(&key)
            .is_none_or(|e| e.peer != peer || e.seen.elapsed() >= PRESENCE_TTL);
        map.insert(
            key,
            PresenceEntry {
                peer,
                seen: Instant::now(),
            },
        );
        changed
    };
    if changed {
        let _ = state.notices.send(Notice::Presence {
            peers: state.peers(&ws),
            ws,
        });
    }
    Ok(Json(json!({})))
}

/// SSE 连接建立后，每隔这么久重新核对一次令牌与成员身份；被撤销 / 被移出团队的连接随即断开。
const EVENTS_RECHECK: Duration = Duration::from_secs(30);

async fn events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(ws): Path<String>,
) -> ApiResult<Sse<impl tokio_stream::Stream<Item = Result<SseEvent, Infallible>>>> {
    let (user, _) = authorize(&state, &headers, &ws).await?;
    let token = bearer(&headers)?;
    // 先订阅再取初始状态：两者之间发生的变化不会丢（至多重复一次，客户端处理是幂等的）。
    let rx = state.notices.subscribe();
    let ws2 = ws.clone();
    let head = state.db(move |db| db.head_seq(&ws2)).await?;
    let (out, out_rx) = tokio::sync::mpsc::channel::<Result<SseEvent, Infallible>>(64);
    let _ = out
        .send(Ok(SseEvent::default()
            .event("presence")
            .data(json!({"peers":state.peers(&ws)}).to_string())))
        .await;
    let _ = out
        .send(Ok(SseEvent::default()
            .event("poke")
            .data(json!({"head_seq":head}).to_string())))
        .await;
    // 转发任务（Java 类比：一个带定时心跳校验的 WebSocket 会话线程）：
    // 既转发广播通知，也定期复核身份；对端断开或身份失效就结束，不会泄露连接。
    tokio::spawn(async move {
        let mut live = BroadcastStream::new(rx);
        let mut check = tokio::time::interval(EVENTS_RECHECK);
        check.tick().await;
        loop {
            tokio::select! {
                notice = live.next() => {
                    let event = match notice {
                        None => break,
                        Some(Ok(Notice::Poke { ws: w, head })) if w == ws => SseEvent::default()
                            .event("poke")
                            .data(json!({"head_seq":head}).to_string()),
                        Some(Ok(Notice::Presence { ws: w, peers })) if w == ws => SseEvent::default()
                            .event("presence")
                            .data(json!({"peers":peers}).to_string()),
                        // 订阅者落后太多、通知被丢弃：发一个 poke 让客户端重新拉取，保证最终一致。
                        Some(Err(_)) => SseEvent::default()
                            .event("poke")
                            .data(json!({"head_seq":-1}).to_string()),
                        Some(Ok(_)) => continue,
                    };
                    if out.send(Ok(event)).await.is_err() {
                        break;
                    }
                }
                _ = check.tick() => {
                    if out.is_closed() {
                        break;
                    }
                    let (token, ws, uid) = (token.clone(), ws.clone(), user.id.clone());
                    let still = state
                        .db(move |db| {
                            let u = db.authenticate(&token)?;
                            if u.id != uid {
                                return Err(ApiError::unauthorized());
                            }
                            db.role(&ws, &u.id).map(|_| ())
                        })
                        .await;
                    if still.is_err() {
                        tracing::info!(user = %user.name, "events stream closed: access revoked");
                        break;
                    }
                }
            }
        }
    });
    Ok(
        Sse::new(tokio_stream::wrappers::ReceiverStream::new(out_rx)).keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        ),
    )
}
