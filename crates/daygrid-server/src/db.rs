//! 服务端存储与同步逻辑（同步代码，无 HTTP 依赖，便于直接单元测试）。
//!
//! 用 Java 的话说：这一层是 Service + DAO。`push` 对每条操作开一个 `BEGIN IMMEDIATE` 事务
//! （SQLite 的写锁 ≈ 悲观锁），在事务里读出现状 → 调用纯函数 `merge_update` → 写回，
//! 所以同一事项的并发写入天然串行化；"乐观"的部分只体现在客户端带来的 `base_rev` 上。
use daygrid::{
    domain::{self, Item},
    store::digest,
    sync::{
        Conflict, FIELDS, FieldEdit, ItemFields, JoinResponse, MeResponse, MeWorkspace, Op,
        OpError, OpKind, OpResult, OpStatus, PullResponse, PushResponse, ServerItem, UserInfo,
        VERSIONS_KEEP, WorkspaceInfo,
        hlc::{Hlc, MAX_DRIFT_MS},
        merge::{self, FieldMeta, Lifecycle, LifecycleOutcome, Meta},
    },
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;

pub const ROLES: [&str; 3] = ["owner", "editor", "viewer"];
pub const SCHEMA_VERSION: i64 = 1;

/// 统一错误：HTTP 层直接映射状态码与错误体。
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    pub fn unauthorized() -> Self {
        Self::new(401, "UNAUTHORIZED", "令牌无效或已被撤销。")
    }
    pub fn forbidden(message: &str) -> Self {
        Self::new(403, "FORBIDDEN", message)
    }
    pub fn bad(message: impl Into<String>) -> Self {
        Self::new(400, "VALIDATION_ERROR", message)
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        tracing::error!(error = %e, "sqlite error");
        Self::new(500, "STORAGE_ERROR", "服务端存储出错。")
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug, Clone)]
pub struct User {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub role: String,
    pub joined_at: String,
}

pub struct ServerDb {
    conn: Connection,
}

fn stamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn new_token() -> String {
    format!("dgt_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
fn new_invite_code() -> String {
    format!("dgi_{}", &Uuid::new_v4().simple().to_string()[..24])
}

fn meta_to_json(meta: &Meta) -> String {
    let map: serde_json::Map<String, Value> = meta
        .iter()
        .map(|(k, v)| (k.clone(), json!({"ts":v.ts,"by":v.by})))
        .collect();
    Value::Object(map).to_string()
}
fn meta_from_json(text: &str) -> Meta {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(text) else {
        return Meta::new();
    };
    map.into_iter()
        .map(|(k, v)| {
            (
                k,
                FieldMeta {
                    ts: v["ts"].as_str().unwrap_or("").to_string(),
                    by: v["by"].as_str().unwrap_or("").to_string(),
                },
            )
        })
        .collect()
}

/// `items` 表的一行。
struct Row {
    id: String,
    rev: i64,
    deleted: bool,
    fields: ItemFields,
    created_at: String,
    updated_at: String,
    meta: Meta,
    updated_by: String,
    ts: String,
}

const ROW_COLUMNS: &str =
    "id,rev,deleted,date,title,notes,color,status,created_at,updated_at,meta,updated_by,ts";

fn row_from(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get(0)?,
        rev: r.get(1)?,
        deleted: r.get::<_, i64>(2)? != 0,
        fields: ItemFields {
            date: r.get(3)?,
            title: r.get(4)?,
            notes: r.get(5)?,
            color: r.get(6)?,
            status: r.get(7)?,
        },
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        meta: meta_from_json(&r.get::<_, String>(10)?),
        updated_by: r.get(11)?,
        ts: r.get(12)?,
    })
}

fn get_row(conn: &Connection, ws: &str, id: &str) -> rusqlite::Result<Option<Row>> {
    conn.query_row(
        &format!("SELECT {ROW_COLUMNS} FROM items WHERE workspace_id=?1 AND id=?2"),
        params![ws, id],
        row_from,
    )
    .optional()
}

fn user_name(conn: &Connection, id: &str) -> String {
    conn.query_row("SELECT name FROM users WHERE id=?1", [id], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn server_item(conn: &Connection, row: &Row, seq: Option<i64>) -> ServerItem {
    ServerItem {
        id: row.id.clone(),
        rev: row.rev,
        deleted: row.deleted,
        fields: row.fields.clone(),
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
        updated_by: user_name(conn, &row.updated_by),
        ts: row.ts.clone(),
        seq,
    }
}

fn body_json(fields: &ItemFields) -> String {
    serde_json::to_string(fields).unwrap()
}

impl ServerDb {
    pub fn open(path: &Path) -> ApiResult<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }
    pub fn open_in_memory() -> ApiResult<Self> {
        Self::init(Connection::open_in_memory()?)
    }
    fn init(conn: Connection) -> ApiResult<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(ApiError::new(
                500,
                "VERSION_UNSUPPORTED",
                "数据库来自更新的服务端版本。",
            ));
        }
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS users(id TEXT PRIMARY KEY,name TEXT NOT NULL,token_hash TEXT NOT NULL UNIQUE,created_at TEXT NOT NULL,revoked INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS workspaces(id TEXT PRIMARY KEY,name TEXT NOT NULL,created_at TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS members(workspace_id TEXT NOT NULL,user_id TEXT NOT NULL,role TEXT NOT NULL CHECK(role IN ('owner','editor','viewer')),joined_at TEXT NOT NULL,PRIMARY KEY(workspace_id,user_id));
             CREATE TABLE IF NOT EXISTS invites(code_hash TEXT PRIMARY KEY,workspace_id TEXT NOT NULL,role TEXT NOT NULL CHECK(role IN ('editor','viewer')),created_by TEXT NOT NULL,expires_at TEXT NOT NULL,uses_left INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS items(workspace_id TEXT NOT NULL,id TEXT NOT NULL,rev INTEGER NOT NULL,deleted INTEGER NOT NULL DEFAULT 0,date TEXT NOT NULL,title TEXT NOT NULL,notes TEXT NOT NULL,color TEXT NOT NULL,status TEXT NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,meta TEXT NOT NULL,updated_by TEXT NOT NULL,ts TEXT NOT NULL,PRIMARY KEY(workspace_id,id));
             CREATE TABLE IF NOT EXISTS item_versions(workspace_id TEXT NOT NULL,item_id TEXT NOT NULL,rev INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(workspace_id,item_id,rev));
             CREATE TABLE IF NOT EXISTS changes(seq INTEGER PRIMARY KEY AUTOINCREMENT,workspace_id TEXT NOT NULL,item_id TEXT NOT NULL,rev INTEGER NOT NULL,user_id TEXT NOT NULL,op_id TEXT NOT NULL,ts TEXT NOT NULL,at TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS changes_ws ON changes(workspace_id,seq);
             CREATE TABLE IF NOT EXISTS applied_ops(workspace_id TEXT NOT NULL,op_id TEXT NOT NULL,user_id TEXT NOT NULL,result TEXT NOT NULL,PRIMARY KEY(workspace_id,op_id));
             CREATE TABLE IF NOT EXISTS conflicts(id INTEGER PRIMARY KEY AUTOINCREMENT,workspace_id TEXT NOT NULL,item_id TEXT NOT NULL,field TEXT NOT NULL,kept_value TEXT NOT NULL,lost_value TEXT NOT NULL,kept_by TEXT NOT NULL,lost_by TEXT NOT NULL,reason TEXT NOT NULL,at TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS conflicts_lost ON conflicts(workspace_id,lost_by,id);",
        )?;
        conn.execute_batch(&format!("PRAGMA user_version={SCHEMA_VERSION}; COMMIT;"))?;
        Ok(Self { conn })
    }

    // ---------- 账号、成员与邀请 ----------

    /// 创建 workspace 与它的 owner；返回 (workspace_id, user_id, 令牌明文)。令牌明文只此一次。
    pub fn init_workspace(
        &mut self,
        workspace: &str,
        owner: &str,
    ) -> ApiResult<(String, String, String)> {
        let name = check_name(owner)?;
        let ws_name = check_name(workspace)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (ws, user, token) = (
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
            new_token(),
        );
        let now = stamp();
        tx.execute(
            "INSERT INTO workspaces VALUES(?1,?2,?3)",
            params![ws, ws_name, now],
        )?;
        tx.execute(
            "INSERT INTO users(id,name,token_hash,created_at) VALUES(?1,?2,?3,?4)",
            params![user, name, digest(&token), now],
        )?;
        tx.execute(
            "INSERT INTO members VALUES(?1,?2,'owner',?3)",
            params![ws, user, now],
        )?;
        tx.commit()?;
        Ok((ws, user, token))
    }

    pub fn workspace_by_name(&self, name: &str) -> ApiResult<Option<WorkspaceInfo>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id,name FROM workspaces WHERE name=?1 OR id=?1 ORDER BY created_at LIMIT 1",
                [name],
                |r| {
                    Ok(WorkspaceInfo {
                        id: r.get(0)?,
                        name: r.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// 令牌 → 用户。已撤销的令牌视为无效。
    pub fn authenticate(&self, token: &str) -> ApiResult<User> {
        self.conn
            .query_row(
                "SELECT id,name FROM users WHERE token_hash=?1 AND revoked=0",
                [digest(token)],
                |r| {
                    Ok(User {
                        id: r.get(0)?,
                        name: r.get(1)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(ApiError::unauthorized)
    }

    /// 用户在 workspace 里的角色；不是成员返回 403（不泄露 workspace 是否存在）。
    pub fn role(&self, ws: &str, user: &str) -> ApiResult<String> {
        self.conn
            .query_row(
                "SELECT role FROM members WHERE workspace_id=?1 AND user_id=?2",
                params![ws, user],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| ApiError::forbidden("你不是这个团队日历的成员。"))
    }

    pub fn me(&self, user: &User) -> ApiResult<MeResponse> {
        let mut stmt = self.conn.prepare(
            "SELECT w.id,w.name,m.role FROM members m JOIN workspaces w ON w.id=m.workspace_id WHERE m.user_id=?1 ORDER BY m.joined_at",
        )?;
        let workspaces = stmt
            .query_map([&user.id], |r| {
                Ok(MeWorkspace {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    role: r.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(MeResponse {
            user: UserInfo {
                id: user.id.clone(),
                name: user.name.clone(),
            },
            workspaces,
        })
    }

    pub fn create_invite(
        &mut self,
        ws: &str,
        by: &str,
        role: &str,
        ttl_hours: i64,
        max_uses: i64,
        now_ms: u64,
    ) -> ApiResult<Value> {
        if !["editor", "viewer"].contains(&role) {
            return Err(ApiError::bad("邀请的角色必须是 editor 或 viewer。"));
        }
        if !(1..=24 * 30).contains(&ttl_hours) || !(1..=100).contains(&max_uses) {
            return Err(ApiError::bad("有效期为 1–720 小时，可用次数为 1–100。"));
        }
        let code = new_invite_code();
        let expires =
            chrono::DateTime::from_timestamp_millis(now_ms as i64 + ttl_hours * 3_600_000)
                .unwrap_or_default()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.conn.execute(
            "INSERT INTO invites VALUES(?1,?2,?3,?4,?5,?6)",
            params![digest(&code), ws, role, by, expires, max_uses],
        )?;
        Ok(json!({"code":code,"role":role,"expires_at":expires,"max_uses":max_uses}))
    }

    /// 凭邀请码加入：返回新用户的令牌（明文只此一次）。
    pub fn join(
        &mut self,
        invite: &str,
        display_name: &str,
        now_ms: u64,
    ) -> ApiResult<JoinResponse> {
        let name = check_name(display_name)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let hash = digest(invite.trim());
        let now_text = chrono::DateTime::from_timestamp_millis(now_ms as i64)
            .unwrap_or_default()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let row: Option<(String, String)> = tx
            .query_row(
                "SELECT workspace_id,role FROM invites WHERE code_hash=?1 AND uses_left>0 AND expires_at>?2",
                params![hash, now_text],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((ws, role)) = row else {
            return Err(ApiError::new(
                403,
                "INVITE_INVALID",
                "邀请码无效、已过期或已用完。",
            ));
        };
        let (user, token) = (Uuid::new_v4().to_string(), new_token());
        tx.execute(
            "INSERT INTO users(id,name,token_hash,created_at) VALUES(?1,?2,?3,?4)",
            params![user, name, digest(&token), stamp()],
        )?;
        tx.execute(
            "INSERT INTO members VALUES(?1,?2,?3,?4)",
            params![ws, user, role, stamp()],
        )?;
        tx.execute(
            "UPDATE invites SET uses_left=uses_left-1 WHERE code_hash=?1",
            [&hash],
        )?;
        let ws_name: String =
            tx.query_row("SELECT name FROM workspaces WHERE id=?1", [&ws], |r| {
                r.get(0)
            })?;
        tx.commit()?;
        Ok(JoinResponse {
            token,
            user: UserInfo { id: user, name },
            workspace: WorkspaceInfo {
                id: ws,
                name: ws_name,
            },
            role,
        })
    }

    pub fn members(&self, ws: &str) -> ApiResult<Vec<Member>> {
        let mut stmt = self.conn.prepare(
            "SELECT u.id,u.name,m.role,m.joined_at FROM members m JOIN users u ON u.id=m.user_id WHERE m.workspace_id=?1 ORDER BY m.joined_at,u.name",
        )?;
        Ok(stmt
            .query_map([ws], |r| {
                Ok(Member {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    role: r.get(2)?,
                    joined_at: r.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }

    pub fn workspace_name(&self, ws: &str) -> ApiResult<String> {
        Ok(self
            .conn
            .query_row("SELECT name FROM workspaces WHERE id=?1", [ws], |r| {
                r.get(0)
            })?)
    }

    /// 移除成员。不能移除自己；被移除后若不再属于任何 workspace，令牌整体作废。
    pub fn remove_member(&mut self, ws: &str, actor: Option<&str>, target: &str) -> ApiResult<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id: Option<String> = tx
            .query_row(
                "SELECT u.id FROM members m JOIN users u ON u.id=m.user_id WHERE m.workspace_id=?1 AND (u.id=?2 OR u.name=?2) LIMIT 1",
                params![ws, target],
                |r| r.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            return Err(ApiError::new(404, "NOT_FOUND", "没有这个成员。"));
        };
        if actor == Some(id.as_str()) {
            return Err(ApiError::bad("不能移除自己。"));
        }
        let role: String = tx.query_row(
            "SELECT role FROM members WHERE workspace_id=?1 AND user_id=?2",
            params![ws, id],
            |r| r.get(0),
        )?;
        if role == "owner" {
            return Err(ApiError::bad("不能移除 owner。"));
        }
        tx.execute(
            "DELETE FROM members WHERE workspace_id=?1 AND user_id=?2",
            params![ws, id],
        )?;
        let left: i64 = tx.query_row(
            "SELECT COUNT(*) FROM members WHERE user_id=?1",
            [&id],
            |r| r.get(0),
        )?;
        if left == 0 {
            tx.execute("UPDATE users SET revoked=1 WHERE id=?1", [&id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 修改成员角色（owner 不可变；只能在 editor / viewer 之间切换）。
    pub fn set_role(&mut self, ws: &str, target: &str, role: &str) -> ApiResult<()> {
        if !["editor", "viewer"].contains(&role) {
            return Err(ApiError::bad("角色必须是 editor 或 viewer。"));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let found: Option<(String, String)> = tx
            .query_row(
                "SELECT u.id,m.role FROM members m JOIN users u ON u.id=m.user_id WHERE m.workspace_id=?1 AND (u.id=?2 OR u.name=?2) LIMIT 1",
                params![ws, target],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((id, old)) = found else {
            return Err(ApiError::new(404, "NOT_FOUND", "没有这个成员。"));
        };
        if old == "owner" {
            return Err(ApiError::bad("不能修改 owner 的角色。"));
        }
        tx.execute(
            "UPDATE members SET role=?1 WHERE workspace_id=?2 AND user_id=?3",
            params![role, ws, id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// workspace 的（第一个）owner，供命令行管理操作署名。
    pub fn owner_id(&self, ws: &str) -> ApiResult<String> {
        self.conn
            .query_row(
                "SELECT user_id FROM members WHERE workspace_id=?1 AND role='owner' ORDER BY joined_at LIMIT 1",
                [ws],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| ApiError::new(404, "NOT_FOUND", "workspace 没有 owner。"))
    }

    pub fn head_seq(&self, ws: &str) -> ApiResult<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(seq),0) FROM changes WHERE workspace_id=?1",
            [ws],
            |r| r.get(0),
        )?)
    }

    /// 一致性备份（`VACUUM INTO`）。
    pub fn backup_to(&self, path: &Path) -> ApiResult<()> {
        self.conn
            .execute("VACUUM INTO ?1", [path.to_string_lossy().to_string()])?;
        Ok(())
    }

    // ---------- 推送：合并的核心 ----------

    /// 处理一批操作。每条操作独立事务；结果与请求一一对应。
    pub fn push(
        &mut self,
        ws: &str,
        user: &User,
        ops: &[Op],
        now_ms: u64,
    ) -> ApiResult<PushResponse> {
        let mut results = Vec::with_capacity(ops.len());
        for op in ops {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let result = apply_op(&tx, ws, user, op, now_ms)?;
            tx.commit()?;
            results.push(result);
        }
        Ok(PushResponse {
            head_seq: self.head_seq(ws)?,
            results,
        })
    }

    // ---------- 拉取 ----------

    pub fn pull(
        &mut self,
        ws: &str,
        user: &User,
        since: i64,
        conflict_since: i64,
        limit: usize,
    ) -> ApiResult<PullResponse> {
        let limit = limit.clamp(1, daygrid::sync::PULL_PAGE);
        let tx = self.conn.transaction()?; // 读事务：保证 head 与明细一致
        // 角色随每次拉取下发：被降级 / 升级的客户端不必等一次失败才发现。
        let role: String = tx
            .query_row(
                "SELECT role FROM members WHERE workspace_id=?1 AND user_id=?2",
                params![ws, user.id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| ApiError::forbidden("你不是这个团队日历的成员。"))?;
        let head: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq),0) FROM changes WHERE workspace_id=?1",
            [ws],
            |r| r.get(0),
        )?;
        let mut changed: Vec<(String, i64)> = {
            let mut stmt = tx.prepare(
                "SELECT item_id,MAX(seq) s FROM changes WHERE workspace_id=?1 AND seq>?2 GROUP BY item_id ORDER BY s LIMIT ?3",
            )?;
            stmt.query_map(params![ws, since, limit as i64 + 1], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<_, _>>()?
        };
        let has_more = changed.len() > limit;
        changed.truncate(limit);
        let next_since = if has_more {
            changed.last().map(|c| c.1).unwrap_or(since)
        } else {
            head.max(since)
        };
        let mut items = Vec::with_capacity(changed.len());
        for (id, seq) in &changed {
            if let Some(row) = get_row(&tx, ws, id)? {
                items.push(server_item(&tx, &row, Some(*seq)));
            }
        }
        let conflicts: Vec<(i64, Conflict)> = {
            let mut stmt = tx.prepare(
                "SELECT c.id,c.item_id,c.field,c.kept_value,c.lost_value,u.name,c.reason,c.at
                 FROM conflicts c LEFT JOIN users u ON u.id=c.kept_by
                 WHERE c.workspace_id=?1 AND c.lost_by=?2 AND c.id>?3 ORDER BY c.id LIMIT 200",
            )?;
            stmt.query_map(params![ws, user.id, conflict_since], |r| {
                let id: i64 = r.get(0)?;
                Ok((
                    id,
                    Conflict {
                        id,
                        message: String::new(),
                        item_id: r.get(1)?,
                        field: r.get(2)?,
                        kept_value: r.get(3)?,
                        lost_value: r.get(4)?,
                        kept_by: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                        reason: r.get(6)?,
                        at: r.get(7)?,
                    },
                ))
            })?
            .collect::<Result<_, _>>()?
        };
        let conflict_head = conflicts
            .last()
            .map(|c| c.0)
            .unwrap_or(conflict_since)
            .max(conflict_since);
        tx.commit()?;
        Ok(PullResponse {
            head_seq: head,
            next_since,
            has_more,
            items,
            conflicts: conflicts.into_iter().map(|c| c.1).collect(),
            conflict_head,
            role: Some(role),
        })
    }

    /// 测试与运维：读取一个事项的当前权威状态。
    pub fn get_item(&self, ws: &str, id: &str) -> ApiResult<Option<ServerItem>> {
        Ok(get_row(&self.conn, ws, id)?.map(|r| server_item(&self.conn, &r, None)))
    }
    pub fn list_items(&self, ws: &str) -> ApiResult<Vec<ServerItem>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ROW_COLUMNS} FROM items WHERE workspace_id=?1 ORDER BY id"
        ))?;
        let rows: Vec<Row> = stmt.query_map([ws], row_from)?.collect::<Result<_, _>>()?;
        Ok(rows
            .iter()
            .map(|r| server_item(&self.conn, r, None))
            .collect())
    }
    pub fn count(&self, sql: &str) -> i64 {
        self.conn.query_row(sql, [], |r| r.get(0)).unwrap_or(-1)
    }
}

/// 单个字段值（含 `prev`）的字符数上限：备注最长 10000 个字符，留一点余量。
const MAX_VALUE_CHARS: usize = 20_000;

fn check_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 40 || name.chars().any(char::is_control) {
        return Err(ApiError::bad("名称需要 1–40 个字符。"));
    }
    Ok(name.to_string())
}

fn rejected(
    op: &Op,
    code: &str,
    message: &str,
    item: Option<ServerItem>,
    conflicts: Vec<Conflict>,
) -> OpResult {
    OpResult {
        op_id: op.op_id.clone(),
        status: OpStatus::Rejected,
        item,
        conflicts,
        error: Some(OpError {
            code: code.into(),
            message: message.into(),
        }),
        clamped: false,
    }
}

/// 校验操作的形状（不碰数据库）。返回夹紧后的操作与是否发生夹紧。
fn validate_op(op: &Op, now_ms: u64) -> Result<(Op, bool), &'static str> {
    if domain::uuid(&op.op_id).is_err() || domain::uuid(&op.item_id).is_err() {
        return Err("操作或事项 ID 必须是 UUID。");
    }
    if op.base_rev < 0 {
        return Err("base_rev 不能为负。");
    }
    let mut clamped = false;
    let mut fix = |ts: &str| -> Result<String, &'static str> {
        let mut h = Hlc::parse(ts).ok_or("时间戳格式无效。")?;
        clamped |= h.clamp_wall(now_ms.saturating_add(MAX_DRIFT_MS));
        Ok(h.to_string())
    };
    let mut out = op.clone();
    out.ts = fix(&op.ts)?;
    for (name, edit) in &op.fields {
        if !FIELDS.contains(&name.as_str()) {
            return Err("包含未知字段。");
        }
        if edit.v.chars().count() > MAX_VALUE_CHARS
            || edit
                .prev
                .as_ref()
                .is_some_and(|p| p.chars().count() > MAX_VALUE_CHARS)
        {
            return Err("字段值过长。");
        }
        let ts = fix(&edit.ts)?;
        out.fields.insert(
            name.clone(),
            FieldEdit {
                v: edit.v.clone(),
                ts,
                prev: edit.prev.clone(),
            },
        );
    }
    match op.kind {
        OpKind::Create if op.fields.len() != FIELDS.len() => Err("create 必须包含全部字段。"),
        OpKind::Update if op.fields.is_empty() => Err("update 至少需要一个字段。"),
        OpKind::Delete | OpKind::Restore if !op.fields.is_empty() => {
            Err("delete/restore 不应带字段。")
        }
        _ => Ok((out, clamped)),
    }
}

/// 用 `domain::validate` 校验（并规范化）合并后的字段。
fn validated(fields: &ItemFields) -> Result<ItemFields, String> {
    let mut item = Item {
        id: Uuid::nil().to_string(),
        date: fields.date.clone(),
        title: fields.title.clone(),
        notes: fields.notes.clone(),
        color: fields.color.clone(),
        status: fields.status.clone(),
        revision: 1,
        created_at: String::new(),
        updated_at: String::new(),
        visibility: "private".into(),
    };
    domain::validate(&mut item).map_err(|e| e.message)?;
    Ok(ItemFields::from(&item))
}

fn record_conflicts(
    tx: &Connection,
    ws: &str,
    item_id: &str,
    user: &User,
    found: &[merge::MergeConflict],
    now: &str,
) -> ApiResult<Vec<Conflict>> {
    let mut mine = Vec::new();
    for c in found {
        // 被覆盖的一方不明（字段从没被人修改过）时没有人需要通知。
        if c.lost_by.is_empty() {
            continue;
        }
        tx.execute(
            "INSERT INTO conflicts(workspace_id,item_id,field,kept_value,lost_value,kept_by,lost_by,reason,at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![ws, item_id, c.field, c.kept_value, c.lost_value, c.kept_by, c.lost_by, c.reason, now],
        )?;
        if c.lost_by == user.id {
            mine.push(Conflict {
                id: tx.last_insert_rowid(),
                item_id: item_id.to_string(),
                field: c.field.clone(),
                kept_value: c.kept_value.clone(),
                lost_value: c.lost_value.clone(),
                kept_by: user_name(tx, &c.kept_by),
                reason: c.reason.to_string(),
                at: now.to_string(),
                message: String::new(),
            });
        }
    }
    Ok(mine)
}

fn save_version(tx: &Connection, ws: &str, row: &Row, user: &User, op: &Op) -> ApiResult<()> {
    tx.execute(
        "INSERT OR REPLACE INTO item_versions VALUES(?1,?2,?3,?4)",
        params![ws, row.id, row.rev, body_json(&row.fields)],
    )?;
    tx.execute(
        "DELETE FROM item_versions WHERE workspace_id=?1 AND item_id=?2 AND rev<=?3",
        params![ws, row.id, row.rev - VERSIONS_KEEP],
    )?;
    tx.execute(
        "INSERT INTO changes(workspace_id,item_id,rev,user_id,op_id,ts,at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![ws, row.id, row.rev, user.id, op.op_id, op.ts, stamp()],
    )?;
    Ok(())
}

fn write_row(tx: &Connection, ws: &str, row: &Row) -> ApiResult<()> {
    tx.execute(
        "INSERT OR REPLACE INTO items(workspace_id,id,rev,deleted,date,title,notes,color,status,created_at,updated_at,meta,updated_by,ts) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![
            ws, row.id, row.rev, row.deleted as i64, row.fields.date, row.fields.title,
            row.fields.notes, row.fields.color, row.fields.status, row.created_at,
            row.updated_at, meta_to_json(&row.meta), row.updated_by, row.ts
        ],
    )?;
    Ok(())
}

fn apply_op(
    tx: &Connection,
    ws: &str,
    user: &User,
    original: &Op,
    now_ms: u64,
) -> ApiResult<OpResult> {
    // 幂等：同一 op_id 重放，返回当初的结果（网络重试 / 响应丢失后的重推）。
    if let Some((saved, owner)) = tx
        .query_row(
            "SELECT result,user_id FROM applied_ops WHERE workspace_id=?1 AND op_id=?2",
            params![ws, original.op_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
    {
        // 只有提交者本人重放才返回当初的结果；别人撞上同一个 op_id 既不能读到结果，也不会被当成已处理。
        if owner != user.id {
            return Ok(rejected(
                original,
                "OP_ID_REUSED",
                "这个操作 ID 已被使用，请换一个新的 UUID。",
                None,
                vec![],
            ));
        }
        if let Ok(result) = serde_json::from_str::<OpResult>(&saved) {
            return Ok(result);
        }
    }
    let result = apply_new_op(tx, ws, user, original, now_ms)?;
    // 只有"有效"的 op_id 才登记；形状非法的不登记（op_id 本身可能不是 UUID，不能进表）。
    if domain::uuid(&original.op_id).is_ok() {
        tx.execute(
            "INSERT OR IGNORE INTO applied_ops VALUES(?1,?2,?3,?4)",
            params![
                ws,
                original.op_id,
                user.id,
                serde_json::to_string(&result).unwrap()
            ],
        )?;
    }
    Ok(result)
}

fn apply_new_op(
    tx: &Connection,
    ws: &str,
    user: &User,
    original: &Op,
    now_ms: u64,
) -> ApiResult<OpResult> {
    let (op, clamped) = match validate_op(original, now_ms) {
        Ok(v) => v,
        Err(message) => {
            return Ok(rejected(
                original,
                "VALIDATION_ERROR",
                message,
                None,
                vec![],
            ));
        }
    };
    let now = stamp();
    let current = get_row(tx, ws, &op.item_id)?;
    let finish = |status: OpStatus, row: &Row, conflicts: Vec<Conflict>| OpResult {
        op_id: op.op_id.clone(),
        status,
        item: Some(server_item(tx, row, None)),
        conflicts,
        error: None,
        clamped,
    };
    match (op.kind, current) {
        (OpKind::Create, Some(row)) => Ok(finish(OpStatus::Noop, &row, vec![])),
        (OpKind::Create, None) => {
            let fields = ItemFields {
                date: op.fields["date"].v.clone(),
                title: op.fields["title"].v.clone(),
                notes: op.fields["notes"].v.clone(),
                color: op.fields["color"].v.clone(),
                status: op.fields["status"].v.clone(),
            };
            let fields = match validated(&fields) {
                Ok(f) => f,
                Err(m) => return Ok(rejected(&op, "VALIDATION_ERROR", &m, None, vec![])),
            };
            let meta: Meta = FIELDS
                .iter()
                .map(|f| {
                    (
                        f.to_string(),
                        FieldMeta {
                            ts: op.fields[*f].ts.clone(),
                            by: user.id.clone(),
                        },
                    )
                })
                .collect();
            let row = Row {
                id: op.item_id.clone(),
                rev: 1,
                deleted: false,
                fields,
                created_at: op
                    .created_at
                    .clone()
                    .filter(|c| !c.is_empty())
                    .unwrap_or_else(|| now.clone()),
                updated_at: now,
                meta,
                updated_by: user.id.clone(),
                ts: op.ts.clone(),
            };
            write_row(tx, ws, &row)?;
            save_version(tx, ws, &row, user, &op)?;
            Ok(finish(OpStatus::Applied, &row, vec![]))
        }
        (_, None) => Ok(rejected(
            &op,
            "NOT_FOUND",
            "服务端没有这个事项。",
            None,
            vec![],
        )),
        (OpKind::Update, Some(mut row)) => {
            if row.deleted {
                // 删除优先：编辑被丢弃，但进冲突表，用户可以恢复。
                let by = row
                    .meta
                    .get("deleted")
                    .map(|m| m.by.clone())
                    .unwrap_or_default();
                let found: Vec<merge::MergeConflict> = op
                    .fields
                    .iter()
                    .map(|(f, e)| merge::MergeConflict {
                        field: f.clone(),
                        kept_value: row.fields.get(f).unwrap_or_default().to_string(),
                        lost_value: e.v.clone(),
                        kept_by: by.clone(),
                        lost_by: user.id.clone(),
                        reason: daygrid::sync::REASON_DELETED,
                    })
                    .collect();
                let mine = record_conflicts(tx, ws, &row.id, user, &found, &now)?;
                let mut r = rejected(
                    &op,
                    "ITEM_DELETED",
                    "事项已被删除，你的修改已保存到冲突列表。",
                    None,
                    mine,
                );
                r.item = Some(server_item(tx, &row, None));
                return Ok(r);
            }
            // 共同祖先：base_rev 等于现版本就是现值；否则取历史版本；取不到则 None（保守按并发处理）。
            let base: Option<ItemFields> = if op.base_rev == row.rev {
                Some(row.fields.clone())
            } else {
                tx.query_row(
                    "SELECT body FROM item_versions WHERE workspace_id=?1 AND item_id=?2 AND rev=?3",
                    params![ws, row.id, op.base_rev],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
                .and_then(|b| serde_json::from_str(&b).ok())
            };
            let merged =
                merge::merge_update(&row.fields, &row.meta, base.as_ref(), &op.fields, &user.id);
            let mut final_fields = merged.fields.clone();
            if merged.changed {
                final_fields = match validated(&merged.fields) {
                    Ok(f) => f,
                    Err(m) => {
                        // 带上服务端当前内容，客户端据此把本地回到权威状态。
                        let item = server_item(tx, &row, None);
                        return Ok(rejected(&op, "VALIDATION_ERROR", &m, Some(item), vec![]));
                    }
                };
            }
            let mine = record_conflicts(tx, ws, &row.id, user, &merged.conflicts, &now)?;
            let status = match (
                merged.changed,
                merged.merged || !merged.conflicts.is_empty(),
            ) {
                (true, false) => OpStatus::Applied,
                (true, true) | (false, true) => OpStatus::Merged,
                (false, false) => OpStatus::Noop,
            };
            if merged.changed {
                row.fields = final_fields;
                row.meta = merged.meta;
                row.rev += 1;
                row.updated_at = now;
                row.updated_by = user.id.clone();
                row.ts = op.ts.clone();
                write_row(tx, ws, &row)?;
                save_version(tx, ws, &row, user, &op)?;
            } else if merged.meta != row.meta {
                row.meta = merged.meta;
                write_row(tx, ws, &row)?; // 只推进时间戳：不递增 rev、不产生变更
            }
            Ok(finish(status, &row, mine))
        }
        (kind @ (OpKind::Delete | OpKind::Restore), Some(mut row)) => {
            let which = if kind == OpKind::Delete {
                Lifecycle::Delete
            } else {
                Lifecycle::Restore
            };
            let last = row.meta.get("deleted").map(|m| m.ts.clone());
            match merge::lifecycle(row.deleted, last.as_deref(), which, &op.ts) {
                LifecycleOutcome::Noop => Ok(finish(OpStatus::Noop, &row, vec![])),
                LifecycleOutcome::LostToNewer => {
                    let mut r = rejected(
                        &op,
                        "LOST_TO_NEWER",
                        "被更晚的相反操作覆盖了。",
                        None,
                        vec![],
                    );
                    r.item = Some(server_item(tx, &row, None));
                    Ok(r)
                }
                LifecycleOutcome::Apply => {
                    row.deleted = kind == OpKind::Delete;
                    row.meta.insert(
                        "deleted".into(),
                        FieldMeta {
                            ts: op.ts.clone(),
                            by: user.id.clone(),
                        },
                    );
                    row.rev += 1;
                    row.updated_at = now;
                    row.updated_by = user.id.clone();
                    row.ts = op.ts.clone();
                    write_row(tx, ws, &row)?;
                    save_version(tx, ws, &row, user, &op)?;
                    Ok(finish(OpStatus::Applied, &row, vec![]))
                }
            }
        }
    }
}
