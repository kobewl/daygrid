use crate::{
    Error, Result, VERSION,
    domain::{self, Item},
    ipc, success,
};
use chrono::{Datelike, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value as Sql};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Instant,
};
use uuid::Uuid;

pub const SCHEMA_VERSION: i64 = 3;
/// 助手发起的待确认请求有效期。
pub const PROPOSAL_TTL_MINUTES: i64 = 10;
/// 助手活动审计最多保留的条数。
const ACTIVITY_KEEP: i64 = 2000;
/// 手动与自动备份各自最多保留的份数。
const BACKUPS_KEEP: usize = 10;
pub const APPROVAL_MODES: [&str; 2] = ["deletes", "all_writes"];

const READ_METHODS: [&str; 5] = [
    "daygrid_get_context",
    "daygrid_list_items",
    "daygrid_get_item",
    "daygrid_month_overview",
    "daygrid_get_proposal",
];
const WRITE_METHODS: [&str; 4] = [
    "daygrid_create_item",
    "daygrid_update_item",
    "daygrid_set_item_status",
    "daygrid_delete_item",
];

pub fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
/// 键排序后的稳定 JSON 序列化：同一请求无论键顺序如何，摘要都相同。
/// 幂等回执的摘要依赖它，因此行为必须与历史版本（按键排序）保持一致。
pub fn canonical(value: &Value) -> String {
    fn sorted(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<_> = m.keys().collect();
                keys.sort();
                Value::Object(
                    keys.into_iter()
                        .map(|k| (k.clone(), sorted(&m[k])))
                        .collect(),
                )
            }
            Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    sorted(value).to_string()
}
/// 固定宽度、可按字典序比较的 UTC 时间戳。
fn stamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
fn short(text: &str) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(40).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// 迁移前备份失败：不迁移，数据库保持原版本。
fn backup_failed(what: &str, e: impl std::fmt::Display) -> Error {
    tracing::error!(error = %e, what, "pre-migration backup failed");
    Error::new(
        "STORAGE_ERROR",
        "升级前无法生成数据库备份，已取消升级，数据保持不变。",
    )
}

/// 备份文件是否完整可用：只读打开、`integrity_check` 通过、schema 版本等于迁移前版本。
fn backup_is_valid(path: &Path, version: i64) -> bool {
    if !path.is_file() {
        return false;
    }
    let check = || -> rusqlite::Result<bool> {
        let db = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let ok: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        let v: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        Ok(ok == "ok" && v == version)
    };
    match check() {
        Ok(valid) => valid,
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "backup check failed");
            false
        }
    }
}

/// 生成 `<db>.pre-v{SCHEMA_VERSION}.bak`：`VACUUM INTO` 临时文件 → 校验 → fsync → 原子改名。
/// 已有同名备份先校验，有效就沿用，无效则用新快照替换。
fn pre_migration_backup(db: &Connection, path: &Path, version: i64) -> Result<()> {
    let backup = PathBuf::from(format!("{}.pre-v{}.bak", path.display(), SCHEMA_VERSION));
    if backup_is_valid(&backup, version) {
        return Ok(());
    }
    let tmp = PathBuf::from(format!("{}.tmp-{}", backup.display(), Uuid::new_v4()));
    let made = (|| -> Result<()> {
        // VACUUM INTO 读取一致性快照（含 WAL 中尚未落盘的内容），不依赖 checkpoint。
        db.execute("VACUUM INTO ?1", [tmp.to_string_lossy().to_string()])
            .map_err(|e| backup_failed("vacuum into", e))?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
            .map_err(|e| backup_failed("chmod", e))?;
        if !backup_is_valid(&tmp, version) {
            return Err(backup_failed("verify", "snapshot failed verification"));
        }
        fs::File::open(&tmp)
            .and_then(|f| f.sync_all())
            .map_err(|e| backup_failed("fsync", e))?;
        fs::rename(&tmp, &backup).map_err(|e| backup_failed("rename", e))?;
        if let Some(dir) = backup.parent() {
            let dir = if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir
            };
            fs::File::open(dir)
                .and_then(|f| f.sync_all())
                .map_err(|e| backup_failed("fsync dir", e))?;
        }
        Ok(())
    })();
    if made.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    made?;
    tracing::info!(
        from = version,
        to = SCHEMA_VERSION,
        "database backed up before migration"
    );
    Ok(())
}

pub struct Store {
    db: Connection,
    path: PathBuf,
    desktop_verifier: String,
    cursor_secret: String,
    started: Instant,
}

/// 写入前置检查的结果：要么这是一次重放，要么可以继续。
enum Pre {
    Replay(Value),
    Go { key: String, hash: String },
}

struct ProposalRow {
    id: String,
    actor: String,
    label: Option<String>,
    digest: String,
    method: String,
    args: String,
    summary: String,
    preview: String,
    state: String,
    created_at: String,
    expires_at: String,
    decided_at: Option<String>,
    outcome: Option<String>,
}

impl ProposalRow {
    /// `admin` 为真时包含连接信息与预览，供桌面界面使用；否则只给助手需要的状态。
    fn to_json(&self, admin: bool) -> Value {
        let outcome: Option<Value> = self
            .outcome
            .as_deref()
            .and_then(|v| serde_json::from_str(v).ok());
        let mut v = json!({
            "id":self.id,"state":self.state,"summary":self.summary,
            "created_at":self.created_at,"expires_at":self.expires_at,"decided_at":self.decided_at,
        });
        match (&outcome, self.state.as_str()) {
            (Some(o), "approved") => v["result"] = o["data"].clone(),
            (Some(o), "failed") => v["error"] = o["error"].clone(),
            _ => {}
        }
        if admin {
            v["actor"] = json!(self.actor);
            v["connection_label"] = json!(self.label);
            v["method"] = json!(self.method);
            v["preview"] = serde_json::from_str(&self.preview).unwrap_or(Value::Null);
        }
        v
    }
}

const PROPOSAL_COLUMNS: &str = "p.id,p.actor,c.label,p.digest,p.method,p.args,p.summary,p.preview,p.state,p.created_at,p.expires_at,p.decided_at,p.outcome";

fn proposal_from_row(r: &rusqlite::Row) -> rusqlite::Result<ProposalRow> {
    Ok(ProposalRow {
        id: r.get(0)?,
        actor: r.get(1)?,
        label: r.get(2)?,
        digest: r.get(3)?,
        method: r.get(4)?,
        args: r.get(5)?,
        summary: r.get(6)?,
        preview: r.get(7)?,
        state: r.get(8)?,
        created_at: r.get(9)?,
        expires_at: r.get(10)?,
        decided_at: r.get(11)?,
        outcome: r.get(12)?,
    })
}

impl Store {
    pub fn open(path: &Path, desktop_token: &str) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(Error::new(
                "VERSION_UNSUPPORTED",
                "数据库来自更新的应用版本。",
            ));
        }
        if (1..SCHEMA_VERSION).contains(&version) {
            // 迁移前留一份经过校验的一致性快照：迁移失败或用户想回退到旧版本时可以直接用。
            // 备份做不出来就不迁移，数据库保持原版本不动。
            pre_migration_backup(&db, path, version)?;
        }
        db.execute_batch("BEGIN IMMEDIATE;
          CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS items(id TEXT PRIMARY KEY,date TEXT NOT NULL,status TEXT NOT NULL,revision INTEGER NOT NULL,created_at TEXT NOT NULL,deleted_at TEXT,body TEXT NOT NULL);
          CREATE INDEX IF NOT EXISTS items_date ON items(date,status,created_at,id);
          CREATE TABLE IF NOT EXISTS history(sequence INTEGER PRIMARY KEY AUTOINCREMENT,operation_id TEXT NOT NULL,item_id TEXT NOT NULL,action TEXT NOT NULL,before_json TEXT,after_json TEXT,actor TEXT NOT NULL,occurred_at TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS receipts(actor TEXT NOT NULL,key TEXT NOT NULL,digest TEXT NOT NULL,result TEXT NOT NULL,PRIMARY KEY(actor,key));
          CREATE TABLE IF NOT EXISTS connections(id TEXT PRIMARY KEY,label TEXT NOT NULL,verifier TEXT NOT NULL,permission TEXT NOT NULL CHECK(permission IN ('read','write')),state TEXT NOT NULL CHECK(state IN ('active','paused','revoked')));")?;
        if version < 2 {
            db.execute_batch(
                "ALTER TABLE connections ADD COLUMN created_at TEXT;
                 ALTER TABLE connections ADD COLUMN last_seen_at TEXT;
                 CREATE TABLE IF NOT EXISTS activity(sequence INTEGER PRIMARY KEY AUTOINCREMENT,occurred_at TEXT NOT NULL,connection_id TEXT NOT NULL,tool TEXT NOT NULL,outcome TEXT NOT NULL,error_code TEXT,duration_ms INTEGER NOT NULL,summary TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS proposals(id TEXT PRIMARY KEY,actor TEXT NOT NULL,idempotency_key TEXT NOT NULL,digest TEXT NOT NULL,method TEXT NOT NULL,args TEXT NOT NULL,summary TEXT NOT NULL,preview TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('pending','approved','rejected','expired','failed')),created_at TEXT NOT NULL,expires_at TEXT NOT NULL,decided_at TEXT,outcome TEXT,UNIQUE(actor,idempotency_key));",
            )?;
        }
        if version < 3 {
            // 0.4.0：休息日 / 工作日的手动标记（只记录与默认不同的那几天；调休补班也存这里）。
            db.execute_batch(
                "CREATE TABLE IF NOT EXISTS day_marks(date TEXT PRIMARY KEY,kind TEXT NOT NULL CHECK(kind IN ('rest','work')),updated_at TEXT NOT NULL);",
            )?;
        }
        db.execute_batch(&format!("PRAGMA user_version={SCHEMA_VERSION}; COMMIT;"))?;
        let generation = Uuid::new_v4().to_string();
        db.execute(
            "INSERT OR IGNORE INTO metadata VALUES('generation',?1)",
            [generation],
        )?;
        let timezone = iana_time_zone::get_timezone()
            .map_err(|_| Error::new("VALIDATION_ERROR", "无法识别系统时区。"))?;
        timezone
            .parse::<chrono_tz::Tz>()
            .map_err(|_| Error::new("VALIDATION_ERROR", "系统时区无效。"))?;
        db.execute(
            "INSERT OR IGNORE INTO metadata VALUES('timezone',?1)",
            [timezone],
        )?;
        Ok(Self {
            db,
            path: path.to_path_buf(),
            desktop_verifier: digest(desktop_token),
            cursor_secret: desktop_token.to_string(),
            started: Instant::now(),
        })
    }

    fn data_dir(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }
    fn metadata(&self, key: &str) -> Result<String> {
        Ok(self
            .db
            .query_row("SELECT value FROM metadata WHERE key=?1", [key], |r| {
                r.get(0)
            })?)
    }
    fn metadata_opt(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT value FROM metadata WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }
    fn approval_mode(&self) -> Result<String> {
        Ok(self
            .metadata_opt("approval_mode")?
            .filter(|m| APPROVAL_MODES.contains(&m.as_str()))
            .unwrap_or_else(|| "deletes".into()))
    }
    fn sequence(&self) -> Result<i64> {
        Ok(self
            .db
            .query_row("SELECT COALESCE(MAX(sequence),0) FROM history", [], |r| {
                r.get(0)
            })?)
    }
    fn authorize(&self, actor: &str, token: &str, write: bool) -> Result<bool> {
        if actor == "desktop" && digest(token) == self.desktop_verifier {
            return Ok(true);
        }
        let row: Option<(String, String, String)> = self
            .db
            .query_row(
                "SELECT verifier,permission,state FROM connections WHERE id=?1",
                [actor],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        match row {
            Some((verifier, permission, state))
                if verifier == digest(token)
                    && state == "active"
                    && (!write || permission == "write") =>
            {
                Ok(false)
            }
            _ => Err(Error::new(
                "PERMISSION_DENIED",
                "连接未授权、已暂停、已撤销或没有写入权限。",
            )),
        }
    }

    pub fn call(&mut self, actor: &str, token: &str, method: &str, args: Value) -> Value {
        let started = Instant::now();
        let summary_args = if actor == "desktop" {
            Value::Null
        } else {
            args.clone()
        };
        let value = self
            .dispatch(actor, token, method, args)
            .unwrap_or_else(|e| e.value());
        if actor != "desktop" {
            self.record_activity(actor, method, &summary_args, &value, started.elapsed());
        }
        value
    }

    /// 记录助手调用。只有已存在的连接才会写入，避免任意字符串刷表。
    fn record_activity(
        &self,
        actor: &str,
        method: &str,
        args: &Value,
        value: &Value,
        elapsed: std::time::Duration,
    ) {
        let code = value["error"]["code"].as_str();
        let now = stamp();
        let touched = if code == Some("PERMISSION_DENIED") {
            self.db
                .query_row("SELECT 1 FROM connections WHERE id=?1", [actor], |_| Ok(()))
                .optional()
                .map(|r| r.is_some())
        } else {
            self.db
                .execute(
                    "UPDATE connections SET last_seen_at=?1 WHERE id=?2",
                    params![now, actor],
                )
                .map(|n| n == 1)
        };
        if !matches!(touched, Ok(true)) {
            return;
        }
        let outcome = if value["ok"] == true { "ok" } else { "error" };
        let result = self.db.execute(
            "INSERT INTO activity(occurred_at,connection_id,tool,outcome,error_code,duration_ms,summary) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                now,
                actor,
                method,
                outcome,
                code,
                elapsed.as_millis() as i64,
                activity_summary(method, args, value)
            ],
        );
        match result {
            Ok(_) => {
                let _ = self.db.execute(
                    "DELETE FROM activity WHERE sequence <= (SELECT MAX(sequence) FROM activity) - ?1",
                    [ACTIVITY_KEEP],
                );
            }
            Err(e) => tracing::warn!(error = %e, "activity log write failed"),
        }
    }

    fn dispatch(&mut self, actor: &str, token: &str, method: &str, args: Value) -> Result<Value> {
        let is_desktop_method = method.starts_with("desktop_");
        let needs_write = WRITE_METHODS.contains(&method);
        let desktop = self.authorize(actor, token, needs_write)?;
        if is_desktop_method && !desktop {
            return Err(Error::new(
                "PERMISSION_DENIED",
                "该操作只能从日格界面执行。",
            ));
        }
        match method {
            "daygrid_get_context" => self.context(&args),
            "daygrid_list_items" => self.list(args),
            "daygrid_get_item" => {
                let a: domain::Get = domain::decode(&args)?;
                domain::uuid(&a.item_id)?;
                Ok(success(json!({"item":self.item(&a.item_id,false)?})))
            }
            "daygrid_month_overview" => self.overview(&args),
            "daygrid_get_proposal" => {
                let a: domain::ProposalRef = domain::decode(&args)?;
                domain::uuid(&a.proposal_id)?;
                self.expire_proposals()?;
                let row = self.proposal(&a.proposal_id)?;
                if row.actor != actor && !desktop {
                    return Err(Error::new("NOT_FOUND", "请求不存在。"));
                }
                Ok(success(json!({"proposal":row.to_json(false)})))
            }
            "daygrid_create_item"
            | "daygrid_update_item"
            | "daygrid_set_item_status"
            | "daygrid_delete_item"
            | "desktop_delete"
            | "desktop_restore" => self.write(actor, method, args),
            "desktop_history" => self.history(&args),
            "desktop_connections" => self.connections(),
            "desktop_create_connection" => self.create_connection(&args),
            "desktop_set_connection" => self.set_connection(&args),
            "desktop_activity" => self.activity(&args),
            "desktop_proposals" => self.proposals(),
            "desktop_decide_proposal" => self.decide(&args),
            "desktop_search" => self.search(&args),
            "desktop_pulse" => self.pulse(),
            "desktop_diagnostics" => self.diagnostics(),
            "desktop_backup" => self.backup(),
            "desktop_list_day_marks" => self.list_day_marks(&args),
            "desktop_set_day_mark" => self.set_day_mark(&args),
            "desktop_get_settings" => Ok(success(json!({"approval_mode":self.approval_mode()?}))),
            "desktop_set_settings" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Args {
                    approval_mode: String,
                }
                let a: Args = domain::decode(&args)?;
                if !APPROVAL_MODES.contains(&a.approval_mode.as_str()) {
                    return Err(Error::new("VALIDATION_ERROR", "确认策略无效。"));
                }
                self.db.execute(
                    "INSERT OR REPLACE INTO metadata VALUES('approval_mode',?1)",
                    [&a.approval_mode],
                )?;
                tracing::info!(mode = %a.approval_mode, "approval mode changed");
                Ok(success(json!({"approval_mode":a.approval_mode})))
            }
            _ => Err(Error::new("VALIDATION_ERROR", "未实现的操作。")),
        }
    }

    // ---------- 只读查询 ----------

    fn context(&self, args: &Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Empty {}
        let _: Empty = domain::decode(args)?;
        let timezone = self.metadata("timezone")?;
        let tz = timezone
            .parse::<chrono_tz::Tz>()
            .map_err(|_| Error::new("STORAGE_ERROR", "日历时区无效。"))?;
        let now = Utc::now().with_timezone(&tz);
        Ok(success(json!({
            "today":now.format("%Y-%m-%d").to_string(),
            "weekday":now.weekday().to_string(),
            "week_starts_on":"monday",
            "calendar_timezone":timezone,
            "calendar_generation":self.metadata("generation")?,
            "supported_date_range":{"from":"1900-01-01","to":"2100-12-31"},
            "max_query_days":366,
            "max_page_size":100,
            "colors":domain::COLORS,
            "change_sequence":self.sequence()?,
            "approval_mode":self.approval_mode()?,
            "service_version":VERSION
        })))
    }

    fn item(&self, id: &str, deleted: bool) -> Result<Item> {
        let body: Option<String> = self
            .db
            .query_row(
                "SELECT body FROM items WHERE id=?1 AND ((deleted_at IS NOT NULL)=?2)",
                params![id, deleted],
                |r| r.get(0),
            )
            .optional()?;
        serde_json::from_str(
            &body.ok_or_else(|| Error::new("NOT_FOUND", "事项不存在或生命周期状态不符。"))?,
        )
        .map_err(|_| Error::new("STORAGE_ERROR", "事项数据无法读取。"))
    }

    fn list(&self, args: Value) -> Result<Value> {
        let a: domain::List = domain::decode(&args)?;
        let from = domain::date(&a.from_date)?;
        let to = domain::date(&a.to_date)?;
        let days = (to - from).num_days() + 1;
        let limit = a.limit.unwrap_or(50);
        if !(1..=366).contains(&days)
            || !(1..=100).contains(&limit)
            || a.color
                .as_ref()
                .is_some_and(|c| !domain::COLORS.contains(&c.as_str()))
            || a.status
                .as_ref()
                .is_some_and(|s| !domain::STATUSES.contains(&s.as_str()))
            || a.query
                .as_ref()
                .is_some_and(|q| q.is_empty() || q.chars().count() > 200)
        {
            return Err(Error::new(
                "VALIDATION_ERROR",
                "日期范围、分页或筛选参数无效。",
            ));
        }
        let sequence = self.sequence()?;
        let query_digest = digest(
            &json!([
                a.from_date,
                a.to_date,
                a.query,
                a.color,
                a.status,
                limit,
                self.metadata("generation")?
            ])
            .to_string(),
        );
        #[derive(Serialize, Deserialize)]
        struct Cursor {
            query: String,
            sequence: i64,
            offset: usize,
        }
        let offset = if let Some(cursor) = a.cursor {
            let (encoded, signature) = cursor
                .split_once('.')
                .ok_or_else(|| Error::new("VALIDATION_ERROR", "分页游标无效。"))?;
            if digest(&format!("{}:{encoded}", self.cursor_secret)) != signature {
                return Err(Error::new("VALIDATION_ERROR", "分页游标无效。"));
            }
            let bytes = hex::decode(encoded)
                .map_err(|_| Error::new("VALIDATION_ERROR", "分页游标无效。"))?;
            let c: Cursor = serde_json::from_slice(&bytes)
                .map_err(|_| Error::new("VALIDATION_ERROR", "分页游标无效。"))?;
            if c.query != query_digest || c.sequence != sequence {
                return Err(Error::new(
                    "CURSOR_STALE",
                    "日历或查询条件已变化，请重新查询。",
                ));
            }
            c.offset
        } else {
            0
        };
        // 状态与颜色在 SQL 里过滤；没有文本查询时连分页也交给 SQL，
        // 避免一个月内上千条事项时把整月都读出来再丢弃。
        let mut sql =
            String::from("SELECT body FROM items WHERE deleted_at IS NULL AND date>=? AND date<=?");
        let mut binds: Vec<Sql> = vec![a.from_date.clone().into(), a.to_date.clone().into()];
        if let Some(status) = &a.status {
            sql.push_str(" AND status=?");
            binds.push(status.clone().into());
        }
        if let Some(color) = &a.color {
            sql.push_str(" AND json_extract(body,'$.color')=?");
            binds.push(color.clone().into());
        }
        sql.push_str(" ORDER BY date,CASE status WHEN 'planned' THEN 0 ELSE 1 END,created_at,id");
        let sql_paging = a.query.is_none();
        if sql_paging {
            sql.push_str(" LIMIT ? OFFSET ?");
            binds.push(((limit + 1) as i64).into());
            binds.push((offset as i64).into());
        }
        let mut stmt = self.db.prepare_cached(&sql)?;
        let rows = stmt.query_map(params_from_iter(binds), |r| r.get::<_, String>(0))?;
        let needle = a.query.as_ref().map(|q| q.to_lowercase());
        let mut items = vec![];
        for row in rows {
            let i: Item = serde_json::from_str(&row?)
                .map_err(|_| Error::new("STORAGE_ERROR", "事项数据无法读取。"))?;
            if let Some(q) = &needle
                && !i.title.to_lowercase().contains(q)
                && !i.notes.to_lowercase().contains(q)
            {
                continue;
            }
            items.push(i);
        }
        let page_offset = if sql_paging { 0 } else { offset };
        let has_more = items.len() > page_offset + limit;
        let cursor = if has_more {
            let encoded = hex::encode(
                serde_json::to_vec(&Cursor {
                    query: query_digest,
                    sequence,
                    offset: offset + limit,
                })
                .unwrap(),
            );
            Some(format!(
                "{encoded}.{}",
                digest(&format!("{}:{encoded}", self.cursor_secret))
            ))
        } else {
            None
        };
        Ok(success(
            json!({"items":items.into_iter().skip(page_offset).take(limit).collect::<Vec<_>>(),"next_cursor":cursor,"snapshot_sequence":sequence}),
        ))
    }

    /// 按天汇总数量与颜色，让助手不必拉取整月全部事项就能看出哪天有安排。
    fn overview(&self, args: &Value) -> Result<Value> {
        let a: domain::Overview = domain::decode(args)?;
        let from = domain::date(&a.from_date)?;
        let to = domain::date(&a.to_date)?;
        let span = (to - from).num_days() + 1;
        if !(1..=366).contains(&span) {
            return Err(Error::new("VALIDATION_ERROR", "日期范围必须为 1–366 天。"));
        }
        let mut stmt = self.db.prepare_cached(
            "SELECT date,status,json_extract(body,'$.color'),COUNT(*) FROM items WHERE deleted_at IS NULL AND date>=?1 AND date<=?2 GROUP BY date,status,json_extract(body,'$.color') ORDER BY date",
        )?;
        let rows = stmt.query_map(params![a.from_date, a.to_date], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut days: Vec<(String, i64, i64, Vec<String>)> = vec![];
        for row in rows {
            let (date, status, color, n) = row?;
            if days.last().is_none_or(|d| d.0 != date) {
                days.push((date.clone(), 0, 0, vec![]));
            }
            let d = days.last_mut().unwrap();
            if status == "done" {
                d.2 += n
            } else {
                d.1 += n
            }
            if !d.3.contains(&color) {
                d.3.push(color);
            }
        }
        let total: i64 = days.iter().map(|d| d.1 + d.2).sum();
        let values: Vec<Value> = days
            .into_iter()
            .map(|(date, planned, done, mut colors)| {
                colors.sort_by_key(|c| domain::COLORS.iter().position(|x| x == c));
                json!({"date":date,"planned":planned,"done":done,"colors":colors})
            })
            .collect();
        Ok(success(
            json!({"from_date":a.from_date,"to_date":a.to_date,"total_items":total,"days":values}),
        ))
    }

    fn history(&self, args: &Value) -> Result<Value> {
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(100)
            .clamp(1, 500);
        let mut stmt=self.db.prepare("SELECT sequence,operation_id,item_id,action,before_json,after_json,actor,occurred_at FROM history ORDER BY sequence DESC LIMIT ?1")?;
        let rows = stmt.query_map([limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
            ))
        })?;
        let mut values = vec![];
        for r in rows {
            let (s, op, id, action, before, after, actor, at) = r?;
            values.push(json!({"sequence":s,"operation_id":op,"item_id":id,"action":action,"before":before.and_then(|v|serde_json::from_str::<Value>(&v).ok()),"after":after.and_then(|v|serde_json::from_str::<Value>(&v).ok()),"actor":actor,"occurred_at":at}));
        }
        Ok(success(json!({"history":values})))
    }

    /// 读取一段日期内手动标记的休息日 / 工作日。没有记录的日子由界面按周末与法定假日推算。
    fn list_day_marks(&self, args: &Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            from_date: String,
            to_date: String,
        }
        let a: Args = domain::decode(args)?;
        let from = domain::date(&a.from_date)?;
        let to = domain::date(&a.to_date)?;
        let days = (to - from).num_days() + 1;
        if !(1..=400).contains(&days) {
            return Err(Error::new("VALIDATION_ERROR", "标记查询范围为 1–400 天。"));
        }
        let mut stmt = self.db.prepare_cached(
            "SELECT date,kind FROM day_marks WHERE date BETWEEN ?1 AND ?2 ORDER BY date",
        )?;
        let marks = stmt
            .query_map(params![a.from_date, a.to_date], |r| {
                Ok(json!({"date":r.get::<_, String>(0)?,"kind":r.get::<_, String>(1)?}))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(success(json!({"marks":marks})))
    }

    /// 把某一天标成休息日或工作日；`default` 删除标记，回到按周末与法定假日推算。
    fn set_day_mark(&self, args: &Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            date: String,
            kind: String,
        }
        let a: Args = domain::decode(args)?;
        domain::date(&a.date)?;
        match a.kind.as_str() {
            "rest" | "work" => {
                self.db.execute(
                    "INSERT INTO day_marks(date,kind,updated_at) VALUES(?1,?2,?3) ON CONFLICT(date) DO UPDATE SET kind=excluded.kind,updated_at=excluded.updated_at",
                    params![a.date, a.kind, stamp()],
                )?;
            }
            "default" => {
                self.db
                    .execute("DELETE FROM day_marks WHERE date=?1", [&a.date])?;
            }
            _ => {
                return Err(Error::new(
                    "VALIDATION_ERROR",
                    "日期类型需要是 rest、work 或 default。",
                ));
            }
        }
        Ok(success(json!({"date":a.date,"kind":a.kind})))
    }

    /// 全局搜索：跨全部日期，按日期倒序，标题命中优先标注。
    fn search(&self, args: &Value) -> Result<Value> {
        let a: domain::Search = domain::decode(args)?;
        let query = a.query.trim().to_lowercase();
        let limit = a.limit.unwrap_or(30).clamp(1, 100);
        if query.is_empty() || query.chars().count() > 200 {
            return Err(Error::new("VALIDATION_ERROR", "搜索词需要 1–200 个字符。"));
        }
        let mut stmt = self.db.prepare_cached(
            "SELECT body FROM items WHERE deleted_at IS NULL ORDER BY date DESC,created_at DESC,id",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut hits = vec![];
        let mut has_more = false;
        for row in rows {
            let item: Item = serde_json::from_str(&row?)
                .map_err(|_| Error::new("STORAGE_ERROR", "事项数据无法读取。"))?;
            let field = if item.title.to_lowercase().contains(&query) {
                "title"
            } else if item.notes.to_lowercase().contains(&query) {
                "notes"
            } else {
                continue;
            };
            if hits.len() == limit {
                has_more = true;
                break;
            }
            hits.push(json!({"item":item,"match":field}));
        }
        Ok(success(json!({"results":hits,"has_more":has_more})))
    }

    // ---------- 连接管理与审计 ----------

    fn connections(&self) -> Result<Value> {
        let mut stmt = self.db.prepare(
            "SELECT id,label,permission,state,created_at,last_seen_at,(SELECT COUNT(*) FROM activity a WHERE a.connection_id=connections.id) FROM connections ORDER BY rowid",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "id":r.get::<_,String>(0)?,"label":r.get::<_,String>(1)?,
                "permission":r.get::<_,String>(2)?,"state":r.get::<_,String>(3)?,
                "created_at":r.get::<_,Option<String>>(4)?,
                "last_seen_at":r.get::<_,Option<String>>(5)?,
                "calls":r.get::<_,i64>(6)?
            }))
        })?;
        Ok(success(
            json!({"connections":rows.collect::<std::result::Result<Vec<_>,_>>()?}),
        ))
    }

    fn create_connection(&self, args: &Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            label: String,
            permission: String,
        }
        let a: Args = domain::decode(args)?;
        if a.label.trim().is_empty()
            || a.label.chars().count() > 100
            || !matches!(a.permission.as_str(), "read" | "write")
        {
            return Err(Error::new("VALIDATION_ERROR", "连接名称或权限无效。"));
        }
        let id = Uuid::new_v4().to_string();
        let secret = format!("{}{}", Uuid::new_v4(), Uuid::new_v4());
        self.db.execute(
            "INSERT INTO connections(id,label,verifier,permission,state,created_at) VALUES(?1,?2,?3,?4,'active',?5)",
            params![id, a.label.trim(), digest(&secret), a.permission, stamp()],
        )?;
        tracing::info!(connection = %id, permission = %a.permission, "connection created");
        // 桌面端把这个一次性秘密写入受保护的本地文件，绝不进入界面。
        Ok(success(json!({"id":id,"credential":secret})))
    }

    fn set_connection(&self, args: &Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            id: String,
            state: String,
        }
        let a: Args = domain::decode(args)?;
        domain::uuid(&a.id)?;
        if !matches!(a.state.as_str(), "active" | "paused" | "revoked") {
            return Err(Error::new("VALIDATION_ERROR", "连接状态无效。"));
        }
        let count = self.db.execute(
            "UPDATE connections SET state=?1 WHERE id=?2 AND state!='revoked'",
            params![a.state, a.id],
        )?;
        if count != 1 {
            return Err(Error::new("NOT_FOUND", "连接不存在或已永久撤销。"));
        }
        if a.state != "active" {
            // 撤销或暂停后，该连接尚未处理的请求不应该还能被批准。
            self.db.execute(
                "UPDATE proposals SET state='rejected',decided_at=?1 WHERE actor=?2 AND state='pending'",
                params![stamp(), a.id],
            )?;
        }
        tracing::info!(connection = %a.id, state = %a.state, "connection state changed");
        Ok(success(json!({"changed":true})))
    }

    fn activity(&self, args: &Value) -> Result<Value> {
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(100)
            .clamp(1, 200);
        let mut stmt = self.db.prepare(
            "SELECT a.sequence,a.occurred_at,a.connection_id,c.label,a.tool,a.outcome,a.error_code,a.duration_ms,a.summary FROM activity a LEFT JOIN connections c ON c.id=a.connection_id ORDER BY a.sequence DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |r| {
            Ok(json!({
                "sequence":r.get::<_,i64>(0)?,"occurred_at":r.get::<_,String>(1)?,
                "connection_id":r.get::<_,String>(2)?,"connection_label":r.get::<_,Option<String>>(3)?,
                "tool":r.get::<_,String>(4)?,"outcome":r.get::<_,String>(5)?,
                "error_code":r.get::<_,Option<String>>(6)?,"duration_ms":r.get::<_,i64>(7)?,
                "summary":r.get::<_,String>(8)?
            }))
        })?;
        Ok(success(
            json!({"activity":rows.collect::<std::result::Result<Vec<_>,_>>()?}),
        ))
    }

    /// 界面轮询用的极轻量脉冲：只有这几个数字变了才需要重新拉数据。
    fn pulse(&self) -> Result<Value> {
        self.expire_proposals()?;
        let (pending, newest): (i64, i64) = self.db.query_row(
            "SELECT COALESCE(SUM(state='pending'),0),COALESCE(MAX(rowid),0) FROM proposals",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let activity: i64 =
            self.db
                .query_row("SELECT COALESCE(MAX(sequence),0) FROM activity", [], |r| {
                    r.get(0)
                })?;
        Ok(success(json!({
            "change_sequence":self.sequence()?,
            "pending_proposals":pending,
            "proposal_rev":newest,
            "activity_sequence":activity
        })))
    }

    fn diagnostics(&self) -> Result<Value> {
        let count = |sql: &str| -> Result<i64> { Ok(self.db.query_row(sql, [], |r| r.get(0))?) };
        let size = |p: &Path| fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let wal = PathBuf::from(format!("{}-wal", self.path.display()));
        Ok(success(json!({
            "service_version":VERSION,
            "ipc_version":ipc::IPC_VERSION,
            "schema_version":SCHEMA_VERSION,
            "uptime_seconds":self.started.elapsed().as_secs(),
            "database_bytes":size(&self.path)+size(&wal),
            "items_active":count("SELECT COUNT(*) FROM items WHERE deleted_at IS NULL")?,
            "items_deleted":count("SELECT COUNT(*) FROM items WHERE deleted_at IS NOT NULL")?,
            "history_entries":count("SELECT COUNT(*) FROM history")?,
            "activity_entries":count("SELECT COUNT(*) FROM activity")?,
            "connections_active":count("SELECT COUNT(*) FROM connections WHERE state='active'")?,
            "timezone":self.metadata("timezone")?,
            "backups":self.list_backups(),
            "data_dir":self.data_dir().display().to_string(),
            "log_dir":crate::logging::logs_dir(self.data_dir()).display().to_string(),
        })))
    }

    // ---------- 备份 ----------

    fn backup_dir(&self) -> PathBuf {
        self.data_dir().join("backups")
    }
    fn list_backups(&self) -> Vec<Value> {
        let mut files: Vec<(String, u64, String)> = fs::read_dir(self.backup_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if !(name.starts_with("calendar-") && name.ends_with(".db")) {
                    return None;
                }
                let meta = e.metadata().ok()?;
                let modified: chrono::DateTime<Utc> = meta.modified().ok()?.into();
                Some((
                    name,
                    meta.len(),
                    modified.to_rfc3339_opts(SecondsFormat::Millis, true),
                ))
            })
            .collect();
        files.sort_by(|a, b| b.0.cmp(&a.0));
        files
            .into_iter()
            .map(|(name, bytes, modified_at)| json!({"name":name,"bytes":bytes,"modified_at":modified_at}))
            .collect()
    }
    /// 用 `VACUUM INTO` 生成一致性快照（服务运行中也安全），并只保留最近几份。
    fn backup(&mut self) -> Result<Value> {
        let dir = self.backup_dir();
        ipc::prepare(&dir)?;
        let name = format!("calendar-{}.db", Utc::now().format("%Y%m%d-%H%M%S"));
        let target = dir.join(&name);
        if !target.exists() {
            self.db
                .execute("VACUUM INTO ?1", [target.to_string_lossy().to_string()])?;
            let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o600));
        }
        let all = self.list_backups();
        for old in all.iter().skip(BACKUPS_KEEP) {
            if let Some(n) = old["name"].as_str() {
                let _ = fs::remove_file(dir.join(n));
            }
        }
        let bytes = fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
        tracing::info!(name = %name, bytes, "backup created");
        Ok(success(
            json!({"name":name,"bytes":bytes,"directory":dir.display().to_string()}),
        ))
    }
    /// 服务启动时调用：距离上一份备份超过一天就自动备份一次。
    pub fn auto_backup(&mut self) -> Result<Option<String>> {
        let newest = self.list_backups().into_iter().next();
        let fresh = newest
            .as_ref()
            .and_then(|b| b["modified_at"].as_str())
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .is_some_and(|t| Utc::now().signed_duration_since(t).num_hours() < 24);
        if fresh {
            return Ok(None);
        }
        let v = self.backup()?;
        Ok(v["data"]["name"].as_str().map(str::to_string))
    }

    // ---------- 写入与审批 ----------

    fn needs_approval(&self, method: &str) -> Result<bool> {
        Ok(method == "daygrid_delete_item"
            || (WRITE_METHODS.contains(&method) && self.approval_mode()? == "all_writes"))
    }

    /// 校验幂等键、日历代际，并查找已存在的回执。
    fn preflight(&self, actor: &str, method: &str, args: &Value) -> Result<Pre> {
        let key = args
            .get("idempotency_key")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("VALIDATION_ERROR", "缺少幂等键。"))?;
        domain::uuid(key)?;
        let generation = args
            .get("calendar_generation")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("VALIDATION_ERROR", "缺少日历代际。"))?;
        domain::uuid(generation)?;
        if generation != self.metadata("generation")? {
            return Err(Error::new(
                "CALENDAR_REPLACED",
                "日历已替换，请重新读取上下文。",
            ));
        }
        let hash = digest(&canonical(&json!([method, args])));
        let receipt: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT digest,result FROM receipts WHERE actor=?1 AND key=?2",
                params![actor, key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((old_hash, body)) = receipt {
            if old_hash != hash {
                return Err(Error::new(
                    "IDEMPOTENCY_CONFLICT",
                    "同一幂等键不能用于不同操作。",
                ));
            }
            let mut value: Value = serde_json::from_str(&body)
                .map_err(|_| Error::new("STORAGE_ERROR", "回执无法读取。"))?;
            value["replayed"] = json!(true);
            return Ok(Pre::Replay(value));
        }
        Ok(Pre::Go {
            key: key.to_string(),
            hash,
        })
    }

    fn write(&mut self, actor: &str, method: &str, args: Value) -> Result<Value> {
        let (key, hash) = match self.preflight(actor, method, &args)? {
            Pre::Replay(v) => return Ok(v),
            Pre::Go { key, hash } => (key, hash),
        };
        if actor != "desktop" && self.needs_approval(method)? {
            return self.propose(actor, method, &args, &key, &hash);
        }
        self.apply(actor, method, &args, &key, &hash)
    }

    fn target(method: &str, args: &Value) -> Result<(String, i64)> {
        Ok(match method {
            "daygrid_update_item" => {
                let a: domain::Update = domain::decode(args)?;
                (a.item_id, a.expected_revision)
            }
            "daygrid_set_item_status" => {
                let a: domain::Status = domain::decode(args)?;
                (a.item_id, a.expected_revision)
            }
            "daygrid_delete_item" => {
                let a: domain::Delete = domain::decode(args)?;
                (a.item_id, a.expected_revision)
            }
            _ => {
                let a: domain::Lifecycle = domain::decode(args)?;
                (a.item_id, a.expected_revision)
            }
        })
    }

    fn apply(
        &mut self,
        actor: &str,
        method: &str,
        args: &Value,
        key: &str,
        hash: &str,
    ) -> Result<Value> {
        let now = Utc::now().to_rfc3339();
        let operation_id = Uuid::new_v4().to_string();
        let (mut item, before, action, expected) = match method {
            "daygrid_create_item" => {
                let a: domain::Create = domain::decode(args)?;
                let mut i = Item {
                    id: Uuid::new_v4().to_string(),
                    date: a.date,
                    title: a.title,
                    notes: a.notes,
                    color: a.color,
                    status: "planned".into(),
                    revision: 1,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                };
                domain::validate(&mut i)?;
                (i, None, "create", None)
            }
            _ => {
                let (id, revision) = Self::target(method, args)?;
                domain::uuid(&id)?;
                let mut i = self.item(&id, method == "desktop_restore")?;
                if revision < 1 || i.revision != revision {
                    return Err(Error::new(
                        "REVISION_CONFLICT",
                        "事项已被修改，草稿已保留，请读取最新内容。",
                    )
                    .with_details(json!({"current_revision":i.revision,"current_item":i})));
                }
                let before = i.clone();
                let action = match method {
                    "daygrid_update_item" => {
                        let a: domain::Update = domain::decode(args)?;
                        a.patch.apply(&mut i)?;
                        "update"
                    }
                    "daygrid_set_item_status" => {
                        let a: domain::Status = domain::decode(args)?;
                        i.status = a.status;
                        domain::validate(&mut i)?;
                        "status"
                    }
                    "desktop_delete" | "daygrid_delete_item" => "delete",
                    "desktop_restore" => "restore",
                    _ => unreachable!(),
                };
                (i, Some(before), action, Some(revision))
            }
        };
        let changed = before.as_ref() != Some(&item) || matches!(action, "delete" | "restore");
        let mut sequence = self.sequence()?;
        let tx = self.db.transaction()?;
        if changed {
            if expected.is_some() {
                item.revision += 1;
                item.updated_at = now.clone()
            }
            let body = serde_json::to_string(&item).unwrap();
            if let Some(revision) = expected {
                let deleted = if action == "delete" {
                    Some(now.clone())
                } else {
                    None
                };
                let count=tx.execute("UPDATE items SET date=?1,status=?2,revision=?3,deleted_at=?4,body=?5 WHERE id=?6 AND revision=?7",params![item.date,item.status,item.revision,deleted,body,item.id,revision])?;
                if count != 1 {
                    return Err(Error::new("REVISION_CONFLICT", "事项已被修改。"));
                }
            } else {
                tx.execute(
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
            }
            tx.execute("INSERT INTO history(operation_id,item_id,action,before_json,after_json,actor,occurred_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![operation_id,item.id,action,before.map(|i|serde_json::to_string(&i).unwrap()),body,actor,now])?;
            sequence = tx.last_insert_rowid();
        }
        let result = json!({"schema_version":1,"ok":true,"operation_id":operation_id,"replayed":false,"change_sequence":sequence,"data":{"item":item,"changed":changed}});
        tx.execute(
            "INSERT INTO receipts VALUES(?1,?2,?3,?4)",
            params![actor, key, hash, result.to_string()],
        )?;
        tx.commit()?;
        Ok(result)
    }

    fn expire_proposals(&self) -> Result<()> {
        let now = stamp();
        let n = self.db.execute(
            "UPDATE proposals SET state='expired',decided_at=?1 WHERE state='pending' AND expires_at<=?1",
            [&now],
        )?;
        if n > 0 {
            tracing::info!(count = n, "proposals expired");
        }
        Ok(())
    }

    fn proposal(&self, id: &str) -> Result<ProposalRow> {
        self.db
            .query_row(
                &format!("SELECT {PROPOSAL_COLUMNS} FROM proposals p LEFT JOIN connections c ON c.id=p.actor WHERE p.id=?1"),
                [id],
                proposal_from_row,
            )
            .optional()?
            .ok_or_else(|| Error::new("NOT_FOUND", "请求不存在。"))
    }

    /// 校验这次写入（不落库）并生成给用户看的摘要与前后对比。
    fn preview(&self, method: &str, args: &Value) -> Result<(String, Value)> {
        let now = Utc::now().to_rfc3339();
        Ok(match method {
            "daygrid_create_item" => {
                let a: domain::Create = domain::decode(args)?;
                let mut i = Item {
                    id: Uuid::new_v4().to_string(),
                    date: a.date,
                    title: a.title,
                    notes: a.notes,
                    color: a.color,
                    status: "planned".into(),
                    revision: 1,
                    created_at: now.clone(),
                    updated_at: now,
                };
                domain::validate(&mut i)?;
                (
                    format!("创建「{}」· {}", short(&i.title), i.date),
                    json!({"kind":"create","before":null,"after":i}),
                )
            }
            _ => {
                let (id, revision) = Self::target(method, args)?;
                domain::uuid(&id)?;
                let before = self.item(&id, false)?;
                if before.revision != revision {
                    return Err(
                        Error::new("REVISION_CONFLICT", "事项已被修改，请读取最新内容。")
                            .with_details(
                                json!({"current_revision":before.revision,"current_item":before}),
                            ),
                    );
                }
                let mut after = before.clone();
                let (kind, summary) = match method {
                    "daygrid_update_item" => {
                        let a: domain::Update = domain::decode(args)?;
                        a.patch.apply(&mut after)?;
                        ("update", format!("修改「{}」", short(&before.title)))
                    }
                    "daygrid_set_item_status" => {
                        let a: domain::Status = domain::decode(args)?;
                        after.status = a.status;
                        domain::validate(&mut after)?;
                        (
                            "status",
                            format!(
                                "将「{}」标记为{}",
                                short(&before.title),
                                if after.status == "done" {
                                    "已完成"
                                } else {
                                    "未完成"
                                }
                            ),
                        )
                    }
                    _ => (
                        "delete",
                        format!("删除「{}」· {}", short(&before.title), before.date),
                    ),
                };
                let mut preview = json!({"kind":kind,"before":before,"after":after});
                if kind == "delete" {
                    let a: domain::Delete = domain::decode(args)?;
                    if let Some(reason) = a.reason {
                        if reason.chars().count() > 200 {
                            return Err(Error::new("VALIDATION_ERROR", "理由最多 200 个字符。"));
                        }
                        preview["reason"] = json!(reason);
                    }
                    preview["after"] = Value::Null;
                }
                (summary, preview)
            }
        })
    }

    fn propose(
        &mut self,
        actor: &str,
        method: &str,
        args: &Value,
        key: &str,
        hash: &str,
    ) -> Result<Value> {
        self.expire_proposals()?;
        let existing: Option<String> = self
            .db
            .query_row(
                "SELECT id FROM proposals WHERE actor=?1 AND idempotency_key=?2",
                params![actor, key],
                |r| r.get(0),
            )
            .optional()?;
        let (row, replayed) = if let Some(id) = existing {
            let row = self.proposal(&id)?;
            if row.digest != hash {
                return Err(Error::new(
                    "IDEMPOTENCY_CONFLICT",
                    "同一幂等键不能用于不同操作。",
                ));
            }
            (row, true)
        } else {
            let (summary, preview) = self.preview(method, args)?;
            let id = Uuid::new_v4().to_string();
            let created = stamp();
            let expires = (Utc::now() + chrono::Duration::minutes(PROPOSAL_TTL_MINUTES))
                .to_rfc3339_opts(SecondsFormat::Millis, true);
            self.db.execute(
                "INSERT INTO proposals(id,actor,idempotency_key,digest,method,args,summary,preview,state,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'pending',?9,?10)",
                params![id, actor, key, hash, method, args.to_string(), summary, preview.to_string(), created, expires],
            )?;
            tracing::info!(proposal = %id, connection = %actor, method, "approval requested");
            (self.proposal(&id)?, false)
        };
        let pending = row.state == "pending";
        let message = match row.state.as_str() {
            "pending" => {
                "已提交给用户，等待在日格中确认。请用 daygrid_get_proposal 查询结果；不要重复提交同一请求。"
            }
            "approved" => "用户已批准并已执行，结果见 proposal.result。",
            "rejected" => "用户已拒绝该请求。不要未经询问就重试。",
            "expired" => "请求已过期（用户未及时确认）。如仍需要，请提交新的请求。",
            _ => "请求未能执行，原因见 proposal.error。",
        };
        Ok(json!({
            "schema_version":1,"ok":true,"replayed":replayed,
            "change_sequence":self.sequence()?,
            "data":{"pending_approval":pending,"proposal":row.to_json(false),"message":message}
        }))
    }

    fn proposals(&self) -> Result<Value> {
        self.expire_proposals()?;
        let mut stmt = self.db.prepare(&format!(
            "SELECT {PROPOSAL_COLUMNS} FROM proposals p LEFT JOIN connections c ON c.id=p.actor ORDER BY (p.state='pending') DESC,p.rowid DESC LIMIT 30"
        ))?;
        let rows = stmt.query_map([], proposal_from_row)?;
        let mut list = vec![];
        for row in rows {
            list.push(row?.to_json(true));
        }
        Ok(success(json!({"proposals":list})))
    }

    fn decide(&mut self, args: &Value) -> Result<Value> {
        let a: domain::Decision = domain::decode(args)?;
        domain::uuid(&a.proposal_id)?;
        if !matches!(a.decision.as_str(), "approve" | "reject") {
            return Err(Error::new(
                "VALIDATION_ERROR",
                "决定必须是 approve 或 reject。",
            ));
        }
        self.expire_proposals()?;
        let row = self.proposal(&a.proposal_id)?;
        if row.state != "pending" {
            return Err(Error::new(
                if row.state == "expired" {
                    "PROPOSAL_EXPIRED"
                } else {
                    "PROPOSAL_NOT_PENDING"
                },
                "该请求已处理或已过期。",
            )
            .with_details(json!({"state":row.state})));
        }
        let now = stamp();
        if a.decision == "reject" {
            self.db.execute(
                "UPDATE proposals SET state='rejected',decided_at=?1 WHERE id=?2",
                params![now, row.id],
            )?;
            tracing::info!(proposal = %row.id, "proposal rejected");
            return Ok(success(
                json!({"proposal":self.proposal(&row.id)?.to_json(true)}),
            ));
        }
        // 批准时连接必须仍然启用且具备写权限；暂停或撤销会让它作废。
        let allowed: bool = self
            .db
            .query_row(
                "SELECT state='active' AND permission='write' FROM connections WHERE id=?1",
                [&row.actor],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        let args_value: Value = serde_json::from_str(&row.args)
            .map_err(|_| Error::new("STORAGE_ERROR", "请求内容无法读取。"))?;
        let outcome = if !allowed {
            Err(Error::new(
                "PERMISSION_DENIED",
                "连接已暂停、撤销或没有写入权限。",
            ))
        } else {
            match self.preflight(&row.actor, &row.method, &args_value) {
                Ok(Pre::Replay(v)) => Ok(v),
                Ok(Pre::Go { key, hash }) => {
                    self.apply(&row.actor, &row.method, &args_value, &key, &hash)
                }
                Err(e) => Err(e),
            }
        };
        let (state, stored) = match &outcome {
            Ok(v) => ("approved", v.clone()),
            Err(e) => ("failed", e.value()),
        };
        self.db.execute(
            "UPDATE proposals SET state=?1,decided_at=?2,outcome=?3 WHERE id=?4",
            params![state, now, stored.to_string(), row.id],
        )?;
        tracing::info!(proposal = %row.id, state, "proposal decided");
        Ok(success(
            json!({"proposal":self.proposal(&row.id)?.to_json(true)}),
        ))
    }
}

/// 活动列表里展示的一句话摘要。可以包含事项标题（它只存在本地数据库里，
/// 不会进入日志文件）。
fn activity_summary(method: &str, args: &Value, value: &Value) -> String {
    let ok = value["ok"] == true;
    let data = &value["data"];
    if !ok {
        let message = value["error"]["message"].as_str().unwrap_or("失败");
        return format!("{}失败：{}", tool_label(method), short(message));
    }
    let title = |v: &Value| short(v["item"]["title"].as_str().unwrap_or(""));
    match method {
        "daygrid_get_context" => "读取日历上下文".into(),
        "daygrid_list_items" => format!(
            "查询 {} ~ {}，{} 条",
            args["from_date"].as_str().unwrap_or("?"),
            args["to_date"].as_str().unwrap_or("?"),
            data["items"].as_array().map_or(0, Vec::len)
        ),
        "daygrid_month_overview" => format!(
            "读取 {} ~ {} 概览，{} 条",
            args["from_date"].as_str().unwrap_or("?"),
            args["to_date"].as_str().unwrap_or("?"),
            data["total_items"]
        ),
        "daygrid_get_item" => format!("读取事项「{}」", title(data)),
        "daygrid_get_proposal" => format!(
            "查询确认结果：{}",
            data["proposal"]["summary"].as_str().unwrap_or("")
        ),
        _ if data["pending_approval"] == true => format!(
            "请求{}（等待确认）",
            data["proposal"]["summary"].as_str().unwrap_or("")
        ),
        "daygrid_create_item" => format!(
            "创建「{}」· {}",
            title(data),
            data["item"]["date"].as_str().unwrap_or("")
        ),
        "daygrid_update_item" => format!("修改「{}」", title(data)),
        "daygrid_set_item_status" => format!(
            "「{}」→ {}",
            title(data),
            if data["item"]["status"] == "done" {
                "已完成"
            } else {
                "未完成"
            }
        ),
        "daygrid_delete_item" => format!("删除「{}」", title(data)),
        _ => tool_label(method).into(),
    }
}

fn tool_label(method: &str) -> &'static str {
    match method {
        "daygrid_get_context" => "读取上下文",
        "daygrid_list_items" => "查询事项",
        "daygrid_get_item" => "读取事项",
        "daygrid_month_overview" => "读取概览",
        "daygrid_get_proposal" => "查询确认结果",
        "daygrid_create_item" => "创建事项",
        "daygrid_update_item" => "修改事项",
        "daygrid_set_item_status" => "切换完成状态",
        "daygrid_delete_item" => "删除事项",
        _ => "调用",
    }
}

pub fn is_read_method(method: &str) -> bool {
    READ_METHODS.contains(&method)
}
