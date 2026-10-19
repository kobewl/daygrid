//! 协同相关的存储操作：outbox、服务端快照与本地重建、冲突表、连接配置。
//!
//! 与同步线程的分工（用 Java 的话说：这里是 DAO/Service 层，同步线程是调度器）：
//! - 这里的每个方法都是**一个短事务**，由调用方持有 `Mutex<Store>` 期间执行；
//! - 网络请求一律在锁外，由 `sync::client` 完成。
//!
//! # 核心不变式
//! 对每个**共享**事项：`本地内容 = 服务端快照（sync_items） + 仍在 outbox 里的未确认修改`。
//! 所有"改本地"的路径（收到服务端状态、服务端拒绝了某条修改、角色变化……）都不直接改本地，
//! 而是改快照 / outbox，然后调用 [`rebuild_local`] 重新推导本地内容——
//! 这样被拒绝的修改、LWW 落败的修改、重放的修改都不可能让本地与服务端长期不一致。
//!
//! 反过来，"本地改了、需要告诉服务端"（新建 / 修改 / 删除 / 改可见范围 / 重新连接）统一走
//! [`reconcile_item`]：把本地内容与"快照 + outbox"的结果做差，差出多少就入队多少个操作，
//! 每个字段带上修改前的值（`prev`）作为三方合并的共同祖先。
use super::*;
use crate::sync::{
    self, Conflict, FIELDS, FieldEdit, ItemFields, Op, OpKind, OpStatus, PullResponse,
    PushResponse, ServerItem,
    hlc::{Clock, Hlc},
    runtime::{ConnectRequest, LocalPresence, Runtime},
};
use std::{collections::BTreeMap, sync::Arc};

const TOKEN_FILE: &str = "sync.token";
/// 断开连接时清除的配置。`sync_last_team` 不在其中：重新连接同一个团队时据此保留共享关系与快照。
const META_KEYS: [&str; 10] = [
    "sync_url",
    "sync_workspace",
    "sync_workspace_name",
    "sync_user_id",
    "sync_user_name",
    "sync_role",
    "sync_client_id",
    "sync_cursor",
    "sync_conflict_cursor",
    "sync_hlc",
];
const LAST_TEAM_KEY: &str = "sync_last_team";

/// 已连接的协同配置（令牌不在这里，见 [`Store::sync_token`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConfig {
    pub url: String,
    pub workspace_id: String,
    pub workspace_name: String,
    pub user_id: String,
    pub user_name: String,
    pub role: String,
    pub client_id: String,
}

/// 一批待推送的操作。`max_seq` 之前的 outbox 行属于这一批。
#[derive(Debug, Clone)]
pub struct PushBatch {
    pub client_id: String,
    pub ops: Vec<Op>,
    pub max_seq: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyStats {
    /// 本地可见内容因此发生变化的事项数。
    pub changed: usize,
    pub conflicts: usize,
    pub rejected: usize,
}

fn meta(db: &Connection, key: &str) -> Result<Option<String>> {
    Ok(db
        .query_row("SELECT value FROM metadata WHERE key=?1", [key], |r| {
            r.get(0)
        })
        .optional()?)
}
fn set_meta(db: &Connection, key: &str, value: &str) -> Result<()> {
    db.execute(
        "INSERT OR REPLACE INTO metadata VALUES(?1,?2)",
        params![key, value],
    )?;
    Ok(())
}

fn ensure_column(db: &Connection, table: &str, column: &str, ddl: &str) -> rusqlite::Result<()> {
    let exists = db
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(std::result::Result::ok)
        .any(|name| name == column);
    if !exists {
        db.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ddl};"))?;
    }
    Ok(())
}

/// 建表（幂等）。对不连接协同的用户全是空表。
pub(super) fn create_tables(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS sync_outbox(seq INTEGER PRIMARY KEY AUTOINCREMENT,op_id TEXT NOT NULL UNIQUE,item_id TEXT NOT NULL,kind TEXT NOT NULL CHECK(kind IN ('create','update','delete','restore')),base_rev INTEGER NOT NULL,fields TEXT NOT NULL,ts TEXT NOT NULL,created_at TEXT NOT NULL);
         CREATE INDEX IF NOT EXISTS sync_outbox_item ON sync_outbox(item_id,seq);
         CREATE TABLE IF NOT EXISTS sync_items(item_id TEXT PRIMARY KEY,server_rev INTEGER NOT NULL,updated_by TEXT,updated_at TEXT);
         CREATE TABLE IF NOT EXISTS sync_conflicts(id INTEGER PRIMARY KEY AUTOINCREMENT,server_id INTEGER NOT NULL UNIQUE,item_id TEXT NOT NULL,field TEXT NOT NULL,kept_value TEXT,lost_value TEXT,kept_by TEXT,reason TEXT NOT NULL,occurred_at TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open','dismissed','restored')));",
    )?;
    // sync_items 同时是"服务端快照"：最近一次确认的服务端内容，用来重建本地与计算差异。
    ensure_column(db, "sync_items", "fields", "TEXT")?;
    ensure_column(db, "sync_items", "deleted", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(db, "sync_items", "created_at", "TEXT")?;
    ensure_column(db, "sync_conflicts", "message", "TEXT NOT NULL DEFAULT ''")
}

/// 当前是否已连接协同（有 workspace 与客户端 ID）。
pub(super) fn linked(db: &Connection) -> Result<bool> {
    Ok(meta(db, "sync_workspace")?.is_some() && meta(db, "sync_client_id")?.is_some())
}

fn is_viewer(db: &Connection) -> Result<bool> {
    Ok(linked(db)? && meta(db, "sync_role")?.as_deref() == Some("viewer"))
}

/// 产生下一个本地 HLC 并持久化（与调用方同一事务，重启后不回退）。
fn next_ts(db: &Connection, node: &str) -> Result<Hlc> {
    let mut clock = Clock::restore(node, meta(db, "sync_hlc")?.as_deref());
    let ts = clock.send(sync::now_ms());
    set_meta(db, "sync_hlc", &ts.to_string())?;
    Ok(ts)
}

/// 吸收服务端时间戳，保证此后本机产生的时间戳因果上更晚。
fn observe_ts(db: &Connection, node: &str, remote: &str) -> Result<()> {
    let Some(remote) = Hlc::parse(remote) else {
        return Ok(());
    };
    let mut clock = Clock::restore(node, meta(db, "sync_hlc")?.as_deref());
    if clock.recv(&remote, sync::now_ms()) {
        set_meta(db, "sync_hlc", &clock.last().to_string())?;
    }
    Ok(())
}

fn server_rev(db: &Connection, item_id: &str) -> Result<i64> {
    Ok(db
        .query_row(
            "SELECT server_rev FROM sync_items WHERE item_id=?1",
            [item_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

fn insert_outbox(
    db: &Connection,
    item_id: &str,
    kind: OpKind,
    base_rev: i64,
    fields: &BTreeMap<String, FieldEdit>,
    ts: &Hlc,
) -> Result<()> {
    db.execute(
        "INSERT INTO sync_outbox(op_id,item_id,kind,base_rev,fields,ts,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            Uuid::new_v4().to_string(),
            item_id,
            kind.as_str(),
            base_rev,
            serde_json::to_string(fields).unwrap(),
            ts.to_string(),
            stamp()
        ],
    )?;
    Ok(())
}

// ---------- 服务端快照与"有效状态"（快照 + outbox） ----------

#[derive(Debug, Clone)]
struct Snapshot {
    rev: i64,
    fields: ItemFields,
    deleted: bool,
    created_at: String,
    updated_at: String,
}

type SnapshotRow = (i64, Option<String>, bool, Option<String>, Option<String>);

fn snapshot(db: &Connection, id: &str) -> Result<Option<Snapshot>> {
    let row: Option<SnapshotRow> = db
        .query_row(
            "SELECT server_rev,fields,deleted,created_at,updated_at FROM sync_items WHERE item_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((rev, Some(fields), deleted, created_at, updated_at)) = row else {
        return Ok(None); // 没有记录，或是开发期旧版本留下的无内容记录：当作服务端不认识它
    };
    let Ok(fields) = serde_json::from_str(&fields) else {
        return Ok(None);
    };
    Ok(Some(Snapshot {
        rev,
        fields,
        deleted,
        created_at: created_at.unwrap_or_default(),
        updated_at: updated_at.unwrap_or_default(),
    }))
}

fn store_snapshot(db: &Connection, s: &ServerItem) -> Result<()> {
    db.execute(
        "INSERT OR REPLACE INTO sync_items(item_id,server_rev,updated_by,updated_at,fields,deleted,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            s.id,
            s.rev,
            s.updated_by,
            s.updated_at,
            serde_json::to_string(&s.fields).unwrap(),
            s.deleted,
            s.created_at
        ],
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Effective {
    fields: ItemFields,
    deleted: bool,
}

struct OutboxRow {
    op_id: String,
    kind: OpKind,
    fields: BTreeMap<String, FieldEdit>,
}

fn outbox_rows(db: &Connection, item_id: &str) -> Result<Vec<OutboxRow>> {
    let mut stmt =
        db.prepare("SELECT op_id,kind,fields FROM sync_outbox WHERE item_id=?1 ORDER BY seq")?;
    let rows = stmt.query_map([item_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (op_id, kind, fields) = row?;
        if let Some(kind) = OpKind::parse(&kind) {
            out.push(OutboxRow {
                op_id,
                kind,
                fields: serde_json::from_str(&fields).unwrap_or_default(),
            });
        }
    }
    Ok(out)
}

/// 服务端状态叠加上尚未确认的修改，即"这个事项在服务端处理完我所有排队修改之后应该的样子"。
/// `None` 表示服务端不认识它，也没有排队中的创建。
fn effective(db: &Connection, id: &str) -> Result<Option<Effective>> {
    let mut state = snapshot(db, id)?.map(|s| Effective {
        fields: s.fields,
        deleted: s.deleted,
    });
    for op in outbox_rows(db, id)? {
        let cur = state.get_or_insert_with(|| Effective {
            fields: ItemFields::default(),
            deleted: false,
        });
        match op.kind {
            OpKind::Create | OpKind::Update => {
                for (name, edit) in op.fields {
                    cur.fields.set(&name, edit.v);
                }
                if op.kind == OpKind::Create {
                    cur.deleted = false;
                }
            }
            OpKind::Delete => cur.deleted = true,
            OpKind::Restore => cur.deleted = false,
        }
    }
    Ok(state)
}

fn read_local(db: &Connection, id: &str) -> Result<Option<(Item, bool)>> {
    let row: Option<(String, Option<String>)> = db
        .query_row("SELECT body,deleted_at FROM items WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let Some((body, deleted_at)) = row else {
        return Ok(None);
    };
    let item = serde_json::from_str(&body)
        .map_err(|_| Error::new("STORAGE_ERROR", "本地事项无法读取。"))?;
    Ok(Some((item, deleted_at.is_some())))
}

fn history(
    db: &Connection,
    item_id: &str,
    action: &str,
    before: Option<&Item>,
    after: &str,
    actor: &str,
) -> Result<()> {
    db.execute(
        "INSERT INTO history(operation_id,item_id,action,before_json,after_json,actor,occurred_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            Uuid::new_v4().to_string(),
            item_id,
            action,
            before.map(|i| serde_json::to_string(i).unwrap()),
            after,
            actor,
            stamp()
        ],
    )?;
    Ok(())
}

/// 只改事项的可见范围标记（不写历史、不动版本号）。用于"切换团队后全部变私有"这类系统整理。
fn set_visibility_raw(db: &Connection, id: &str, visibility: &str) -> Result<()> {
    let Some((mut item, _)) = read_local(db, id)? else {
        return Ok(());
    };
    if item.visibility != visibility {
        item.visibility = visibility.into();
        db.execute(
            "UPDATE items SET body=?1 WHERE id=?2",
            params![serde_json::to_string(&item).unwrap(), id],
        )?;
    }
    Ok(())
}

/// 用户级的可见范围变化：像一次普通修改那样升版本并留下历史。
fn change_visibility(db: &Connection, id: &str, visibility: &str, actor: &str) -> Result<()> {
    let Some((mut item, _)) = read_local(db, id)? else {
        return Ok(());
    };
    if item.visibility == visibility {
        return Ok(());
    }
    let before = item.clone();
    item.visibility = visibility.into();
    item.revision += 1;
    item.updated_at = Utc::now().to_rfc3339();
    let body = serde_json::to_string(&item).unwrap();
    db.execute(
        "UPDATE items SET revision=?1,body=?2 WHERE id=?3",
        params![item.revision, body, id],
    )?;
    history(db, id, "visibility", Some(&before), &body, actor)
}

fn all_item_ids(db: &Connection) -> Result<Vec<String>> {
    let mut stmt = db.prepare("SELECT id FROM items ORDER BY created_at,id")?;
    Ok(stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<_, _>>()?)
}

/// 按"快照 + outbox"重新推导共享事项的本地内容（见模块文档的不变式）。返回本地可见内容是否变化。
///
/// - 本地还没有这个事项（同事新建的）：创建，标记为共享；
/// - 本地是私有事项：绝不改动；若服务端仍认为它存在（例如同事恢复了我已撤回的事项），
///   立刻排一个删除，保证私有内容不会留在团队里。
fn rebuild_local(db: &Connection, id: &str, actor: &str) -> Result<bool> {
    let Some(eff) = effective(db, id)? else {
        return Ok(false);
    };
    let snap = snapshot(db, id)?;
    let local = read_local(db, id)?;
    let Some((mut item, was_deleted)) = local else {
        // 同事新建的事项。服务端上已经是删除状态的就不必在本地凭空出现了。
        let Some(snap) = snap else { return Ok(false) };
        if eff.deleted {
            return Ok(false);
        }
        let item = Item {
            id: id.to_string(),
            date: eff.fields.date.clone(),
            title: eff.fields.title.clone(),
            notes: eff.fields.notes.clone(),
            color: eff.fields.color.clone(),
            status: eff.fields.status.clone(),
            revision: 1,
            created_at: snap.created_at.clone(),
            updated_at: snap.updated_at.clone(),
            visibility: "shared".into(),
            remind_at: None,
        };
        let body = serde_json::to_string(&item).unwrap();
        db.execute(
            "INSERT INTO items VALUES(?1,?2,?3,?4,?5,NULL,?6)",
            params![
                item.id,
                item.date,
                item.status,
                item.revision,
                item.created_at,
                body
            ],
        )?;
        history(db, id, "create", None, &body, actor)?;
        return Ok(true);
    };
    if item.visibility != "shared" {
        if !eff.deleted {
            reconcile_item(db, id)?;
        }
        return Ok(false);
    }
    if ItemFields::from(&item) == eff.fields && was_deleted == eff.deleted {
        return Ok(false);
    }
    let before = item.clone();
    item.date = eff.fields.date.clone();
    item.title = eff.fields.title.clone();
    item.notes = eff.fields.notes.clone();
    item.color = eff.fields.color.clone();
    item.status = eff.fields.status.clone();
    // remind_at 是本机字段（item 里原样保留）：同步只改日期时按天数平移，时刻不变。
    if before.date != item.date
        && let Some(r) = item.remind_at.as_deref()
    {
        let tz = meta(db, "timezone")?
            .and_then(|t| t.parse::<chrono_tz::Tz>().ok())
            .ok_or_else(|| Error::new("STORAGE_ERROR", "日历时区无效。"))?;
        match domain::shift_remind_at(r, &before.date, &item.date, tz) {
            Some(s) => item.remind_at = Some(s),
            // validate 保证存储值可解析，正常不会发生；保持原值，不让整次同步失败。
            None => tracing::warn!("remind_at shift failed; kept as is"),
        }
    }
    item.revision += 1;
    item.updated_at = snap
        .map(|s| s.updated_at)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    let body = serde_json::to_string(&item).unwrap();
    db.execute(
        "UPDATE items SET date=?1,status=?2,revision=?3,deleted_at=?4,body=?5 WHERE id=?6",
        params![
            item.date,
            item.status,
            item.revision,
            eff.deleted.then(|| item.updated_at.clone()),
            body,
            id
        ],
    )?;
    let action = match (was_deleted, eff.deleted) {
        (false, true) => "delete",
        (true, false) => "restore",
        _ => "update",
    };
    history(db, id, action, Some(&before), &body, actor)?;
    // 同事改了日期、状态或删除 / 恢复：提醒行随之维护（同一事务）。
    super::reminders::sync_row(db, &item, eff.deleted, Utc::now().timestamp_millis())?;
    Ok(true)
}

/// 把一个服务端事项状态记入快照，并据此重建本地。返回本地可见内容是否变化。
fn apply_server_item(db: &Connection, s: &ServerItem) -> Result<bool> {
    let known = snapshot(db, &s.id)?.map(|k| k.rev);
    if known.is_none_or(|k| s.rev > k) {
        store_snapshot(db, s)?;
    }
    // 即使这个版本已经应用过（rev 没前进）也要重建：被拒绝 / 落败的修改需要靠它让本地回到正确状态。
    rebuild_local(db, &s.id, &format!("sync:{}", s.updated_by))
}

/// 一个字段的修改：(字段名, 修改前的值)。
type Change = (&'static str, Option<String>);

/// 把本地内容与"快照 + outbox"做差，差出多少入队多少个操作。返回入队的操作数。
///
/// 所有写入口（手工修改、助手写入、改可见范围、重新连接、角色变化）都走这里，
/// 所以"本地改了但没告诉服务端"不可能发生。只读成员不会产生操作。
fn reconcile_item(db: &Connection, id: &str) -> Result<usize> {
    if !linked(db)? || is_viewer(db)? {
        return Ok(0);
    }
    let Some(node) = meta(db, "sync_client_id")? else {
        return Ok(0);
    };
    let Some((item, locally_deleted)) = read_local(db, id)? else {
        return Ok(0);
    };
    let eff = effective(db, id)?;
    let want_alive = item.visibility == "shared" && !locally_deleted;
    let local_fields = ItemFields::from(&item);
    // 要排的操作：(种类, 字段名, 修改前的值)。
    let mut plan: Vec<(OpKind, Vec<Change>)> = Vec::new();
    let diff = |base: &ItemFields| -> Vec<Change> {
        FIELDS
            .iter()
            .copied()
            .filter(|f| local_fields.get(f) != base.get(f))
            .map(|f| (f, base.get(f).map(str::to_string)))
            .collect()
    };
    match (want_alive, &eff) {
        (true, None) => plan.push((OpKind::Create, Vec::new())),
        (true, Some(e)) if e.deleted => {
            plan.push((OpKind::Restore, Vec::new()));
            let d = diff(&e.fields);
            if !d.is_empty() {
                plan.push((OpKind::Update, d));
            }
        }
        (true, Some(e)) => {
            let d = diff(&e.fields);
            if !d.is_empty() {
                plan.push((OpKind::Update, d));
            }
        }
        (false, Some(e)) if !e.deleted => plan.push((OpKind::Delete, Vec::new())),
        _ => {}
    }
    if plan.is_empty() {
        return Ok(0);
    }
    let ts = next_ts(db, &node)?;
    let base = server_rev(db, id)?;
    let count = plan.len();
    for (kind, changed) in plan {
        let fields: BTreeMap<String, FieldEdit> = match kind {
            OpKind::Create => FIELDS
                .iter()
                .map(|f| {
                    (
                        f.to_string(),
                        FieldEdit {
                            v: local_fields.get(f).unwrap().to_string(),
                            ts: ts.to_string(),
                            prev: None,
                        },
                    )
                })
                .collect(),
            _ => changed
                .into_iter()
                .map(|(f, prev)| {
                    (
                        f.to_string(),
                        FieldEdit {
                            v: local_fields.get(f).unwrap().to_string(),
                            ts: ts.to_string(),
                            prev,
                        },
                    )
                })
                .collect(),
        };
        let base_rev = if kind == OpKind::Create { 0 } else { base };
        insert_outbox(db, id, kind, base_rev, &fields, &ts)?;
    }
    Ok(count)
}

fn reconcile_all(db: &Connection) -> Result<usize> {
    let mut n = 0;
    for id in all_item_ids(db)? {
        n += reconcile_item(db, &id)?;
    }
    Ok(n)
}

/// 本地写入成功后，在**同一事务**里把它记入 outbox（Transactional Outbox）。
/// 未连接协同时什么都不做；来自服务端的变更不走这里（否则会回声循环）。
pub(super) fn enqueue_local(db: &Connection, item_id: &str) -> Result<()> {
    reconcile_item(db, item_id)?;
    Ok(())
}

/// 本机的待推送操作数。
pub(super) fn pending(db: &Connection) -> Result<i64> {
    Ok(db.query_row("SELECT COUNT(*) FROM sync_outbox", [], |r| r.get(0))?)
}

fn insert_conflict(db: &Connection, c: &Conflict, server_id: i64) -> Result<bool> {
    let at = if c.at.is_empty() {
        stamp()
    } else {
        c.at.clone()
    };
    Ok(db.execute(
        "INSERT OR IGNORE INTO sync_conflicts(server_id,item_id,field,kept_value,lost_value,kept_by,reason,occurred_at,message) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![server_id, c.item_id, c.field, c.kept_value, c.lost_value, c.kept_by, c.reason, at, c.message],
    )? == 1)
}

/// 本地生成的冲突记录（如服务端拒绝了我的修改）使用负数 server_id，避免与服务端 ID 冲突。
fn local_conflict_id(db: &Connection) -> Result<i64> {
    let min: i64 = db.query_row(
        "SELECT COALESCE(MIN(server_id),0) FROM sync_conflicts",
        [],
        |r| r.get(0),
    )?;
    Ok(min.min(0) - 1)
}

/// 一条排队中的修改没能生效（服务端拒绝 / 角色变成只读），把它的内容记成冲突，用户可以找回。
/// 更新类操作按字段各记一条（可以一键恢复）；其他操作记一条整体说明。
fn record_discarded(
    db: &Connection,
    item_id: &str,
    op: &OutboxRow,
    reason: &str,
    message: &str,
) -> Result<usize> {
    let snap = snapshot(db, item_id)?;
    let mut rows: Vec<Conflict> = Vec::new();
    if op.kind == OpKind::Update {
        for (field, edit) in &op.fields {
            rows.push(Conflict {
                id: 0,
                item_id: item_id.to_string(),
                field: field.clone(),
                kept_value: snap
                    .as_ref()
                    .and_then(|s| s.fields.get(field))
                    .unwrap_or_default()
                    .to_string(),
                lost_value: edit.v.clone(),
                kept_by: String::new(),
                reason: reason.to_string(),
                at: String::new(),
                message: message.to_string(),
            });
        }
    } else {
        rows.push(Conflict {
            id: 0,
            item_id: item_id.to_string(),
            field: "*".into(),
            kept_value: String::new(),
            lost_value: String::new(),
            kept_by: String::new(),
            reason: reason.to_string(),
            at: String::new(),
            message: message.to_string(),
        });
    }
    let mut n = 0;
    for c in rows {
        let id = local_conflict_id(db)?;
        if insert_conflict(db, &c, id)? {
            n += 1;
        }
    }
    Ok(n)
}

/// 角色变化。降级成只读时，没法上传的排队修改被放弃（记成冲突，可在升级后找回），本地回到服务端状态。
/// 返回角色是否真的变了。
fn apply_role(db: &Connection, role: &str) -> Result<bool> {
    if !matches!(role, "owner" | "editor" | "viewer") {
        return Ok(false);
    }
    let old = meta(db, "sync_role")?.unwrap_or_default();
    if old == role {
        return Ok(false);
    }
    set_meta(db, "sync_role", role)?;
    if role == "viewer" {
        let ids: Vec<String> = {
            let mut stmt = db.prepare("SELECT DISTINCT item_id FROM sync_outbox")?;
            stmt.query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<_, _>>()?
        };
        for id in ids {
            let had_snapshot = snapshot(db, &id)?.is_some();
            for op in outbox_rows(db, &id)? {
                record_discarded(
                    db,
                    &id,
                    &op,
                    sync::REASON_READ_ONLY,
                    "你在团队日历里的角色变成了只读，这次修改没有上传。",
                )?;
            }
            db.execute("DELETE FROM sync_outbox WHERE item_id=?1", [&id])?;
            if had_snapshot {
                rebuild_local(db, &id, "sync:server")?;
            } else {
                // 还没来得及上传的新事项：留在本机，改为私有。
                set_visibility_raw(db, &id, "private")?;
            }
        }
    } else if old == "viewer" {
        reconcile_all(db)?;
    }
    Ok(true)
}

impl Store {
    pub fn runtime(&self) -> Arc<Runtime> {
        self.runtime.clone()
    }

    fn token_path(&self) -> PathBuf {
        self.data_dir().join(TOKEN_FILE)
    }

    /// 读取协同令牌（权限 0600 的独立文件，不进数据库、备份与日志）。
    pub fn sync_token(&self) -> Result<Option<String>> {
        match fs::read_to_string(self.token_path()) {
            Ok(t) if !t.trim().is_empty() => Ok(Some(t.trim().to_string())),
            _ => Ok(None),
        }
    }

    pub fn sync_config(&self) -> Result<Option<SyncConfig>> {
        if !linked(&self.db)? {
            return Ok(None);
        }
        let get = |k: &str| -> Result<String> { Ok(meta(&self.db, k)?.unwrap_or_default()) };
        Ok(Some(SyncConfig {
            url: get("sync_url")?,
            workspace_id: get("sync_workspace")?,
            workspace_name: get("sync_workspace_name")?,
            user_id: get("sync_user_id")?,
            user_name: get("sync_user_name")?,
            role: get("sync_role")?,
            client_id: get("sync_client_id")?,
        }))
    }

    pub fn sync_pending(&self) -> Result<i64> {
        pending(&self.db)
    }

    pub fn sync_cursors(&self) -> Result<(i64, i64)> {
        let get = |k: &str| -> Result<i64> {
            Ok(meta(&self.db, k)?.and_then(|v| v.parse().ok()).unwrap_or(0))
        };
        Ok((get("sync_cursor")?, get("sync_conflict_cursor")?))
    }

    /// 只读成员？（未连接协同时恒为 false。）
    pub(super) fn sync_is_viewer(&self) -> Result<bool> {
        is_viewer(&self.db)
    }

    /// 新建事项时确定可见范围：界面可以指定；助手一律是私有；只读成员不能新建共享事项。
    pub(super) fn create_visibility(&self, actor: &str, requested: Option<&str>) -> Result<String> {
        let Some(v) = requested else {
            return Ok("private".into());
        };
        if actor != "desktop" {
            return Err(Error::new(
                "VALIDATION_ERROR",
                "助手不能指定可见范围，它创建的事项默认只有你自己可见。",
            ));
        }
        if !domain::VISIBILITIES.contains(&v) {
            return Err(Error::new("VALIDATION_ERROR", "可见范围无效。"));
        }
        if v == "shared" {
            self.ensure_linked_for_sharing()?;
        }
        Ok(v.into())
    }

    pub(super) fn ensure_linked_for_sharing(&self) -> Result<()> {
        if !linked(&self.db)? {
            return Err(Error::new(
                "VALIDATION_ERROR",
                "还没有连接团队日历，事项只保存在本机。",
            ));
        }
        Ok(())
    }

    /// 连接协同：写配置与令牌。
    ///
    /// - 连接的是**另一个团队**（或第一次连接）：所有本地事项一律变成私有、清掉上一个团队的快照与冲突，
    ///   上一个团队的内容绝不会被上传到新团队；
    /// - 重新连接**同一个团队**：保留共享关系与快照，断开期间的本地修改 / 删除会被逐字段对账后上传，
    ///   不会被服务器版本静默覆盖；
    /// - `share_existing`：把本机现有的私有事项也共享出去（默认不共享，由用户明确选择）。
    ///
    /// 返回本次新共享的事项数。
    pub fn sync_link(
        &mut self,
        cfg: &SyncConfig,
        token: &str,
        share_existing: bool,
    ) -> Result<usize> {
        if linked(&self.db)? {
            return Err(Error::new("VALIDATION_ERROR", "已连接协同，请先断开。"));
        }
        let path = self.token_path();
        let mut file = fs::OpenOptions::new();
        file.write(true).create(true).truncate(true);
        {
            use std::os::unix::fs::OpenOptionsExt;
            file.mode(0o600);
        }
        {
            use std::io::Write;
            file.open(&path)
                .and_then(|mut f| f.write_all(token.as_bytes()))
                .map_err(|_| Error::new("STORAGE_ERROR", "无法保存协同令牌。"))?;
        }
        let team = format!("{}|{}", cfg.url, cfg.workspace_id);
        let result = (|| -> Result<usize> {
            let tx = self.db.transaction()?;
            let same_team = meta(&tx, LAST_TEAM_KEY)?.as_deref() == Some(team.as_str());
            tx.execute("DELETE FROM sync_outbox", [])?;
            // 来自上一个团队的事项（曾经共享过的）：即使选了"共享现有事项"也不会带进新团队。
            let mut foreign: std::collections::HashSet<String> = Default::default();
            if !same_team {
                tx.execute_batch("DELETE FROM sync_items; DELETE FROM sync_conflicts;")?;
                for id in all_item_ids(&tx)? {
                    if read_local(&tx, &id)?.is_some_and(|(i, _)| i.visibility == "shared") {
                        foreign.insert(id.clone());
                    }
                    set_visibility_raw(&tx, &id, "private")?;
                }
            }
            for (k, v) in [
                ("sync_url", cfg.url.as_str()),
                ("sync_workspace", cfg.workspace_id.as_str()),
                ("sync_workspace_name", cfg.workspace_name.as_str()),
                ("sync_user_id", cfg.user_id.as_str()),
                ("sync_user_name", cfg.user_name.as_str()),
                ("sync_role", cfg.role.as_str()),
                ("sync_client_id", cfg.client_id.as_str()),
                ("sync_cursor", "0"),
                ("sync_conflict_cursor", "0"),
                (LAST_TEAM_KEY, team.as_str()),
            ] {
                set_meta(&tx, k, v)?;
            }
            let mut shared = 0;
            if cfg.role == "viewer" {
                // 只读成员：断开期间对共享事项做的修改上传不了，先记下来，随后的拉取会让本地回到服务端状态。
                for id in all_item_ids(&tx)? {
                    let (Some((item, deleted)), Some(snap)) =
                        (read_local(&tx, &id)?, snapshot(&tx, &id)?)
                    else {
                        continue;
                    };
                    if item.visibility != "shared" || deleted {
                        continue;
                    }
                    let local = ItemFields::from(&item);
                    let fields = FIELDS
                        .iter()
                        .filter(|f| local.get(f) != snap.fields.get(f))
                        .map(|f| {
                            (
                                f.to_string(),
                                FieldEdit {
                                    v: local.get(f).unwrap().to_string(),
                                    ts: String::new(),
                                    prev: None,
                                },
                            )
                        })
                        .collect::<BTreeMap<_, _>>();
                    if !fields.is_empty() {
                        let op = OutboxRow {
                            op_id: String::new(),
                            kind: OpKind::Update,
                            fields,
                        };
                        record_discarded(
                            &tx,
                            &id,
                            &op,
                            sync::REASON_READ_ONLY,
                            "你在团队日历里是只读成员，断开期间的这次修改没有上传。",
                        )?;
                    }
                }
            } else {
                if share_existing {
                    for id in all_item_ids(&tx)? {
                        if let Some((item, false)) = read_local(&tx, &id)?
                            && item.visibility != "shared"
                            && !foreign.contains(&id)
                        {
                            change_visibility(&tx, &id, "shared", "desktop")?;
                            shared += 1;
                        }
                    }
                }
                reconcile_all(&tx)?;
            }
            tx.commit()?;
            Ok(shared)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&path);
        }
        let count = result?;
        tracing::info!(shared = count, "sync linked");
        self.refresh_reminder_hint();
        self.runtime.wake();
        Ok(count)
    }

    /// 断开协同：清除配置、令牌与待推送队列。本地事项原样保留；
    /// 服务端快照保留着，万一重新连接的是同一个团队，断开期间的修改据此对账。
    pub fn sync_unlink(&mut self) -> Result<()> {
        let tx = self.db.transaction()?;
        for key in META_KEYS {
            tx.execute("DELETE FROM metadata WHERE key=?1", [key])?;
        }
        tx.execute("DELETE FROM sync_outbox", [])?;
        tx.commit()?;
        let _ = fs::remove_file(self.token_path());
        self.runtime.update(|s| *s = Default::default());
        tracing::info!("sync unlinked");
        Ok(())
    }

    /// 取出待推送的操作（不删除；推送成功后由 [`Store::sync_apply_push`] 清理）。
    pub fn sync_prepare_push(&self, limit: usize) -> Result<Option<PushBatch>> {
        let Some(cfg) = self.sync_config()? else {
            return Ok(None);
        };
        if cfg.role == "viewer" {
            return Ok(None);
        }
        let mut stmt = self.db.prepare(
            "SELECT seq,op_id,item_id,kind,base_rev,fields,ts FROM sync_outbox ORDER BY seq LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        let mut ops = Vec::new();
        let mut max_seq = 0;
        for row in rows {
            let (seq, op_id, item_id, kind, base_rev, fields, ts) = row?;
            let Some(kind) = OpKind::parse(&kind) else {
                continue;
            };
            let created_at = if kind == OpKind::Create {
                self.db
                    .query_row(
                        "SELECT created_at FROM items WHERE id=?1",
                        [&item_id],
                        |r| r.get(0),
                    )
                    .optional()?
            } else {
                None
            };
            max_seq = seq;
            ops.push(Op {
                op_id,
                item_id,
                kind,
                base_rev,
                ts,
                fields: serde_json::from_str(&fields).unwrap_or_default(),
                created_at,
            });
        }
        if ops.is_empty() {
            return Ok(None);
        }
        Ok(Some(PushBatch {
            client_id: cfg.client_id,
            ops,
            max_seq,
        }))
    }

    /// 应用推送结果：清理已处理的 outbox 行，把服务端返回的权威状态记入快照并重建本地，记录冲突。
    ///
    /// 注意**不会**改动仍在排队的其他修改：它们各自带着修改前的值（`prev`），
    /// 服务端合并时自己能分清"我改之前是什么、别人改了什么"。
    pub fn sync_apply_push(
        &mut self,
        batch: &PushBatch,
        resp: &PushResponse,
    ) -> Result<ApplyStats> {
        let Some(cfg) = self.sync_config()? else {
            return Ok(ApplyStats::default());
        };
        if cfg.client_id != batch.client_id {
            return Ok(ApplyStats::default()); // 推送期间断开又重连了：结果作废
        }
        let by_id: BTreeMap<&str, &Op> = batch.ops.iter().map(|o| (o.op_id.as_str(), o)).collect();
        let mut stats = ApplyStats::default();
        let tx = self.db.transaction()?;
        let mut max_ts: Option<String> = None;
        for result in &resp.results {
            let Some(op) = by_id.get(result.op_id.as_str()) else {
                continue;
            };
            let sent = outbox_rows(&tx, &op.item_id)?
                .into_iter()
                .find(|r| r.op_id == result.op_id);
            tx.execute("DELETE FROM sync_outbox WHERE op_id=?1", [&result.op_id])?;
            for c in &result.conflicts {
                if insert_conflict(&tx, c, c.id)? {
                    stats.conflicts += 1;
                }
            }
            if let Some(item) = &result.item {
                if apply_server_item(&tx, item)? {
                    stats.changed += 1;
                }
                if max_ts.as_deref() < Some(item.ts.as_str()) {
                    max_ts = Some(item.ts.clone());
                }
            }
            if result.status == OpStatus::Rejected {
                stats.rejected += 1;
                let code = result.error.as_ref().map(|e| e.code.as_str()).unwrap_or("");
                let message = result
                    .error
                    .as_ref()
                    .map(|e| e.message.clone())
                    .unwrap_or_default();
                tracing::warn!(code, kind = op.kind.as_str(), "sync op rejected");
                if code == "NOT_FOUND" && op.kind != OpKind::Create {
                    // 服务端不认识这个事项（例如服务器数据曾被回滚）：丢掉排队的旧修改，用本地当前内容重新上传。
                    reseed(&tx, &op.item_id)?;
                } else {
                    if let (Some(sent), true) = (&sent, result.conflicts.is_empty()) {
                        let note = if op.kind == OpKind::Create {
                            format!("{message}（这个事项没能共享，已改为只有你自己可见）")
                        } else {
                            message.clone()
                        };
                        stats.conflicts +=
                            record_discarded(&tx, &op.item_id, sent, sync::REASON_INVALID, &note)?;
                    }
                    if op.kind == OpKind::Create && snapshot(&tx, &op.item_id)?.is_none() {
                        // 服务端不接受这个新事项：它不在团队里，就不该继续标成"共享"。
                        tx.execute("DELETE FROM sync_outbox WHERE item_id=?1", [&op.item_id])?;
                        set_visibility_raw(&tx, &op.item_id, "private")?;
                    }
                    // 被拒绝的修改不能留在本地：按"快照 + 剩余排队修改"重新推导。
                    if rebuild_local(&tx, &op.item_id, "sync:server")? {
                        stats.changed += 1;
                    }
                }
            }
        }
        if let Some(ts) = max_ts {
            observe_ts(&tx, &cfg.client_id, &ts)?;
        }
        tx.commit()?;
        self.refresh_reminder_hint();
        Ok(stats)
    }

    /// 应用拉取结果并推进游标；同时按服务端告知的角色校正本地角色。
    pub fn sync_apply_pull(&mut self, resp: &PullResponse) -> Result<ApplyStats> {
        let Some(cfg) = self.sync_config()? else {
            return Ok(ApplyStats::default());
        };
        let mut stats = ApplyStats::default();
        let tx = self.db.transaction()?;
        let mut max_ts: Option<String> = None;
        for item in &resp.items {
            if apply_server_item(&tx, item)? {
                stats.changed += 1;
            }
            if max_ts.as_deref() < Some(item.ts.as_str()) {
                max_ts = Some(item.ts.clone());
            }
        }
        for c in &resp.conflicts {
            if insert_conflict(&tx, c, c.id)? {
                stats.conflicts += 1;
            }
        }
        if let Some(ts) = max_ts {
            observe_ts(&tx, &cfg.client_id, &ts)?;
        }
        let (cursor, conflict_cursor) = {
            let get = |k: &str| -> i64 {
                meta(&tx, k)
                    .ok()
                    .flatten()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
            };
            (get("sync_cursor"), get("sync_conflict_cursor"))
        };
        set_meta(&tx, "sync_cursor", &cursor.max(resp.next_since).to_string())?;
        set_meta(
            &tx,
            "sync_conflict_cursor",
            &conflict_cursor.max(resp.conflict_head).to_string(),
        )?;
        if let Some(role) = &resp.role
            && apply_role(&tx, role)?
        {
            stats.changed += 1;
        }
        tx.commit()?;
        self.refresh_reminder_hint();
        if resp.role.as_deref().is_some_and(|r| r != cfg.role) {
            self.runtime.wake();
        }
        Ok(stats)
    }

    /// 角色变化（例如推送时被服务端告知只读）。
    pub fn sync_change_role(&mut self, role: &str) -> Result<()> {
        let tx = self.db.transaction()?;
        apply_role(&tx, role)?;
        tx.commit()?;
        self.refresh_reminder_hint();
        Ok(())
    }

    // ---------- 界面使用的 desktop_sync_* ----------

    pub(super) fn sync_status(&self) -> Result<Value> {
        let status = self.runtime.status();
        let open: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM sync_conflicts WHERE state='open'",
            [],
            |r| r.get(0),
        )?;
        let cfg = self.sync_config()?;
        let (cursor, _) = self.sync_cursors()?;
        let peers: Vec<Value> = status
            .peers
            .iter()
            .map(|p| json!({"client_id":p.client_id,"user":p.user,"date":p.date,"item_id":p.item_id,"editing":p.editing}))
            .collect();
        Ok(success(json!({
            "linked": cfg.is_some(),
            "config": cfg.as_ref().map(|c| json!({
                "url":c.url,"workspace_id":c.workspace_id,"workspace_name":c.workspace_name,
                "user_id":c.user_id,"user_name":c.user_name,"role":c.role,"client_id":c.client_id,
                "insecure": insecure_url(&c.url),
            })),
            "state": match (cfg.is_some(), status.state) {
                (true, "off") => "connecting",
                (true, s) => s,
                (false, "connecting" | "error") => status.state,
                (false, _) => "off",
            },
            "pending": self.sync_pending()?,
            "local_items": self.db.query_row("SELECT COUNT(*) FROM items WHERE deleted_at IS NULL", [], |r| r.get::<_, i64>(0))?,
            "open_conflicts": open,
            "last_sync_at": status.last_sync_at,
            "last_error": status.last_error,
            "events_connected": status.events_connected,
            "cursor": cursor,
            "peers": peers,
        })))
    }

    pub(super) fn sync_connect(&self, args: &Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            url: String,
            invite: Option<String>,
            token: Option<String>,
            display_name: Option<String>,
            /// 一个令牌加入了多个团队时，用名称或 ID 指定连哪一个。
            workspace: Option<String>,
            /// 是否把本机现有的事项共享给团队（默认不共享）。
            #[serde(default)]
            share_existing: bool,
        }
        let a: Args = domain::decode(args)?;
        if linked(&self.db)? {
            return Err(Error::new("VALIDATION_ERROR", "已连接协同，请先断开。"));
        }
        let url = normalize_url(&a.url)?;
        let invite = a
            .invite
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let token = a
            .token
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let name = a.display_name.unwrap_or_default().trim().to_string();
        if invite.is_none() == token.is_none() {
            return Err(Error::new(
                "VALIDATION_ERROR",
                "请填写邀请码或访问令牌（二选一）。",
            ));
        }
        if invite.is_some() && (name.is_empty() || name.chars().count() > 40) {
            return Err(Error::new(
                "VALIDATION_ERROR",
                "请填写 1–40 个字符的显示名。",
            ));
        }
        self.runtime.request_connect(ConnectRequest {
            url,
            invite,
            token,
            display_name: name,
            workspace: a
                .workspace
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            share_existing: a.share_existing,
        });
        Ok(success(json!({"accepted":true})))
    }

    pub(super) fn sync_presence(&self, args: &Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            date: Option<String>,
            item_id: Option<String>,
            #[serde(default)]
            editing: bool,
        }
        let a: Args = domain::decode(args)?;
        if let Some(d) = &a.date {
            domain::date(d)?;
        }
        if let Some(i) = &a.item_id {
            domain::uuid(i)?;
        }
        // 私有事项的存在本身也不对外：在线状态里只带共享事项的 ID。
        let item_id = match a.item_id {
            Some(id) => match read_local(&self.db, &id)? {
                Some((item, _)) if item.visibility == "shared" => Some(id),
                _ => None,
            },
            None => None,
        };
        self.runtime.set_presence(LocalPresence {
            date: a.date,
            item_id,
            editing: a.editing,
        });
        Ok(success(json!({})))
    }

    pub(super) fn sync_conflicts(&self, args: &Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            state: Option<String>,
        }
        let a: Args = domain::decode(args)?;
        let state = a.state.unwrap_or_else(|| "open".into());
        let mut stmt = self.db.prepare(
            "SELECT c.id,c.item_id,c.field,c.kept_value,c.lost_value,c.kept_by,c.reason,c.occurred_at,c.state,i.body,c.message
             FROM sync_conflicts c LEFT JOIN items i ON i.id=c.item_id
             WHERE (?1='all' OR c.state=?1) ORDER BY c.id DESC LIMIT 100",
        )?;
        let rows = stmt.query_map([&state], |r| {
            let body: Option<String> = r.get(9)?;
            let title = body
                .and_then(|b| serde_json::from_str::<Item>(&b).ok())
                .map(|i| i.title);
            Ok(json!({
                "id":r.get::<_,i64>(0)?,"item_id":r.get::<_,String>(1)?,"field":r.get::<_,String>(2)?,
                "kept_value":r.get::<_,Option<String>>(3)?,"lost_value":r.get::<_,Option<String>>(4)?,
                "kept_by":r.get::<_,Option<String>>(5)?,"reason":r.get::<_,String>(6)?,
                "occurred_at":r.get::<_,String>(7)?,"state":r.get::<_,String>(8)?,"item_title":title,
                "message":r.get::<_,String>(10)?,
            }))
        })?;
        let list: Vec<Value> = rows.collect::<std::result::Result<_, _>>()?;
        Ok(success(json!({"conflicts":list})))
    }

    /// 处理一条冲突：`dismiss` 忽略；`restore` 把我被覆盖的值作为一次新的普通修改写回（会再次参与合并）。
    pub(super) fn sync_resolve_conflict(&mut self, args: &Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            id: i64,
            action: String,
        }
        let a: Args = domain::decode(args)?;
        let row: Option<(String, String, Option<String>, String)> = self
            .db
            .query_row(
                "SELECT item_id,field,lost_value,state FROM sync_conflicts WHERE id=?1",
                [a.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((item_id, field, lost, state)) = row else {
            return Err(Error::new("NOT_FOUND", "冲突记录不存在。"));
        };
        if state != "open" {
            return Err(Error::new("VALIDATION_ERROR", "这条冲突已处理。"));
        }
        match a.action.as_str() {
            "dismiss" => {
                self.db.execute(
                    "UPDATE sync_conflicts SET state='dismissed' WHERE id=?1",
                    [a.id],
                )?;
            }
            "restore" => {
                if !FIELDS.contains(&field.as_str()) {
                    return Err(Error::new(
                        "VALIDATION_ERROR",
                        "这条冲突无法恢复，请手动修改。",
                    ));
                }
                let lost = lost.unwrap_or_default();
                let is_deleted: bool = self
                    .db
                    .query_row(
                        "SELECT deleted_at IS NOT NULL FROM items WHERE id=?1",
                        [&item_id],
                        |r| r.get(0),
                    )
                    .optional()?
                    .ok_or_else(|| Error::new("NOT_FOUND", "事项不存在。"))?;
                let current = self.item(&item_id, is_deleted)?;
                // 先在副本上校验：值不合法 / 没有权限就在动手之前报错，不会做到一半。
                let mut probe = current.clone();
                match field.as_str() {
                    "status" => probe.status = lost.clone(),
                    "date" => probe.date = lost.clone(),
                    "title" => probe.title = lost.clone(),
                    "notes" => probe.notes = lost.clone(),
                    _ => probe.color = lost.clone(),
                }
                domain::validate(&mut probe)?;
                if self.sync_is_viewer()? && current.visibility == "shared" {
                    return Err(Error::new(
                        "PERMISSION_DENIED",
                        "你在团队日历里是只读成员，现在不能恢复这条修改。",
                    ));
                }
                let generation = self.metadata("generation")?;
                let args = |rev: i64| {
                    json!({
                        "idempotency_key":Uuid::new_v4().to_string(),
                        "calendar_generation":generation,
                        "item_id":item_id,"expected_revision":rev,
                    })
                };
                let mut rev = current.revision;
                if is_deleted {
                    let r = self.write("desktop", "desktop_restore", args(rev))?;
                    rev = r["data"]["item"]["revision"].as_i64().unwrap_or(rev + 1);
                }
                // 状态有专门的写入口，其余字段走普通修改。
                let (method, mut body) = if field == "status" {
                    let mut v = args(rev);
                    v["status"] = json!(lost);
                    ("daygrid_set_item_status", v)
                } else {
                    let mut v = args(rev);
                    v["patch"] = json!({ field.clone(): lost });
                    ("daygrid_update_item", v)
                };
                body["idempotency_key"] = json!(Uuid::new_v4().to_string());
                if let Err(e) = self.write("desktop", method, body) {
                    if is_deleted {
                        // 恢复事项后写回失败：把事项还原成已删除，保证"要么全成功要么没动过"。
                        let restored = self.item(&item_id, false)?;
                        let _ = self.write("desktop", "desktop_delete", args(restored.revision));
                    }
                    return Err(e);
                }
                self.db.execute(
                    "UPDATE sync_conflicts SET state='restored' WHERE id=?1",
                    [a.id],
                )?;
            }
            _ => {
                return Err(Error::new(
                    "VALIDATION_ERROR",
                    "操作必须是 dismiss 或 restore。",
                ));
            }
        }
        Ok(success(json!({"id":a.id,"action":a.action})))
    }
}

/// 服务端不认识这个事项时：丢掉它排队的旧修改与过期的快照，用本地当前内容重新入队为 create。
fn reseed(db: &Connection, item_id: &str) -> Result<()> {
    db.execute("DELETE FROM sync_outbox WHERE item_id=?1", [item_id])?;
    db.execute("DELETE FROM sync_items WHERE item_id=?1", [item_id])?;
    reconcile_item(db, item_id)?;
    Ok(())
}

/// 规范化服务器地址：只允许 http / https，去掉末尾斜杠与查询。
pub fn normalize_url(url: &str) -> Result<String> {
    let url = url.trim().trim_end_matches('/');
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://"))
        || url.len() > 300
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
        || url.contains('?')
        || url.contains('#')
    {
        return Err(Error::new(
            "VALIDATION_ERROR",
            "服务器地址必须以 http:// 或 https:// 开头。",
        ));
    }
    Ok(url.to_string())
}

/// 明文 http 且不是本机回环地址：传输未加密，界面据此提示。
pub fn insecure_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', ':']).next().unwrap_or("");
    !(host == "localhost" || host == "127.0.0.1" || host == "[::1]")
}
