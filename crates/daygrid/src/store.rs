use crate::{
    Error, Result,
    domain::{self, Item},
    success,
};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use uuid::Uuid;

pub fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
pub struct Store {
    db: Connection,
    desktop_verifier: String,
    cursor_secret: String,
}

impl Store {
    pub fn open(path: &Path, desktop_token: &str) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 1 {
            return Err(Error::new(
                "VERSION_UNSUPPORTED",
                "数据库来自更新的应用版本。",
            ));
        }
        db.execute_batch("BEGIN IMMEDIATE;
          CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS items(id TEXT PRIMARY KEY,date TEXT NOT NULL,status TEXT NOT NULL,revision INTEGER NOT NULL,created_at TEXT NOT NULL,deleted_at TEXT,body TEXT NOT NULL);
          CREATE INDEX IF NOT EXISTS items_date ON items(date,status,created_at,id);
          CREATE TABLE IF NOT EXISTS history(sequence INTEGER PRIMARY KEY AUTOINCREMENT,operation_id TEXT NOT NULL,item_id TEXT NOT NULL,action TEXT NOT NULL,before_json TEXT,after_json TEXT,actor TEXT NOT NULL,occurred_at TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS receipts(actor TEXT NOT NULL,key TEXT NOT NULL,digest TEXT NOT NULL,result TEXT NOT NULL,PRIMARY KEY(actor,key));
          CREATE TABLE IF NOT EXISTS connections(id TEXT PRIMARY KEY,label TEXT NOT NULL,verifier TEXT NOT NULL,permission TEXT NOT NULL CHECK(permission IN ('read','write')),state TEXT NOT NULL CHECK(state IN ('active','paused','revoked')));
          PRAGMA user_version=1; COMMIT;")?;
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
            desktop_verifier: digest(desktop_token),
            cursor_secret: desktop_token.to_string(),
        })
    }
    fn metadata(&self, key: &str) -> Result<String> {
        Ok(self
            .db
            .query_row("SELECT value FROM metadata WHERE key=?1", [key], |r| {
                r.get(0)
            })?)
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
        self.dispatch(actor, token, method, args)
            .unwrap_or_else(|e| e.value())
    }
    fn dispatch(&mut self, actor: &str, token: &str, method: &str, args: Value) -> Result<Value> {
        let read = matches!(
            method,
            "daygrid_get_context"
                | "daygrid_list_items"
                | "daygrid_get_item"
                | "desktop_history"
                | "desktop_connections"
        );
        let desktop = self.authorize(actor, token, !read)?;
        if method.starts_with("desktop_") && !desktop {
            return Err(Error::new(
                "PERMISSION_DENIED",
                "该操作只能从日格界面执行。",
            ));
        }
        match method {
            "daygrid_get_context" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Empty {}
                let _: Empty = domain::decode(&args)?;
                let timezone = self.metadata("timezone")?;
                let tz = timezone
                    .parse::<chrono_tz::Tz>()
                    .map_err(|_| Error::new("STORAGE_ERROR", "日历时区无效。"))?;
                Ok(success(
                    json!({"today":Utc::now().with_timezone(&tz).format("%Y-%m-%d").to_string(),"calendar_timezone":timezone,"calendar_generation":self.metadata("generation")?,"supported_date_range":{"from":"1900-01-01","to":"2100-12-31"},"max_query_days":366,"max_page_size":100,"colors":domain::COLORS,"change_sequence":self.sequence()?}),
                ))
            }
            "daygrid_list_items" => self.list(args),
            "daygrid_get_item" => {
                let a: domain::Get = domain::decode(&args)?;
                domain::uuid(&a.item_id)?;
                Ok(success(json!({"item":self.item(&a.item_id,false)?})))
            }
            "daygrid_create_item"
            | "daygrid_update_item"
            | "daygrid_set_item_status"
            | "desktop_delete"
            | "desktop_restore" => self.write(actor, method, args),
            "desktop_history" => {
                let mut stmt=self.db.prepare("SELECT sequence,operation_id,item_id,action,before_json,after_json,actor,occurred_at FROM history ORDER BY sequence DESC LIMIT 100")?;
                let rows = stmt.query_map([], |r| {
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
            "desktop_connections" => {
                let mut stmt = self
                    .db
                    .prepare("SELECT id,label,permission,state FROM connections ORDER BY rowid")?;
                let rows=stmt.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"label":r.get::<_,String>(1)?,"permission":r.get::<_,String>(2)?,"state":r.get::<_,String>(3)?})))?;
                Ok(success(
                    json!({"connections":rows.collect::<std::result::Result<Vec<_>,_>>()?}),
                ))
            }
            "desktop_create_connection" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Args {
                    label: String,
                    permission: String,
                }
                let a: Args = domain::decode(&args)?;
                if a.label.trim().is_empty()
                    || a.label.chars().count() > 100
                    || !matches!(a.permission.as_str(), "read" | "write")
                {
                    return Err(Error::new("VALIDATION_ERROR", "连接名称或权限无效。"));
                }
                let id = Uuid::new_v4().to_string();
                let secret = format!("{}{}", Uuid::new_v4(), Uuid::new_v4());
                self.db.execute(
                    "INSERT INTO connections VALUES(?1,?2,?3,?4,'active')",
                    params![id, a.label.trim(), digest(&secret), a.permission],
                )?;
                // The desktop writes this one-time secret to a protected local file, never to its UI.
                Ok(success(json!({"id":id,"credential":secret})))
            }
            "desktop_set_connection" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Args {
                    id: String,
                    state: String,
                }
                let a: Args = domain::decode(&args)?;
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
                Ok(success(json!({"changed":true})))
            }
            _ => Err(Error::new("VALIDATION_ERROR", "未实现的操作。")),
        }
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
        let mut stmt=self.db.prepare("SELECT body FROM items WHERE deleted_at IS NULL AND date>=?1 AND date<=?2 ORDER BY date,CASE status WHEN 'planned' THEN 0 ELSE 1 END,created_at,id")?;
        let rows = stmt.query_map(params![a.from_date, a.to_date], |r| r.get::<_, String>(0))?;
        let mut items = vec![];
        for row in rows {
            let i: Item = serde_json::from_str(&row?)
                .map_err(|_| Error::new("STORAGE_ERROR", "事项数据无法读取。"))?;
            if a.color.as_ref().is_some_and(|v| v != &i.color)
                || a.status.as_ref().is_some_and(|v| v != &i.status)
            {
                continue;
            }
            if let Some(q) = &a.query {
                let q = q.to_lowercase();
                if !i.title.to_lowercase().contains(&q) && !i.notes.to_lowercase().contains(&q) {
                    continue;
                }
            }
            items.push(i);
        }
        let next = offset.saturating_add(limit);
        let cursor = if next < items.len() {
            let encoded = hex::encode(
                serde_json::to_vec(&Cursor {
                    query: query_digest,
                    sequence,
                    offset: next,
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
            json!({"items":items.into_iter().skip(offset).take(limit).collect::<Vec<_>>(),"next_cursor":cursor,"snapshot_sequence":sequence}),
        ))
    }
    fn write(&mut self, actor: &str, method: &str, args: Value) -> Result<Value> {
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
        let hash = digest(&json!([method, args]).to_string());
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
            return Ok(value);
        }
        let now = Utc::now().to_rfc3339();
        let operation_id = Uuid::new_v4().to_string();
        let (mut item, before, action, expected) = match method {
            "daygrid_create_item" => {
                let a: domain::Create = domain::decode(&args)?;
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
                let (id, revision) = if method == "daygrid_update_item" {
                    let a: domain::Update = domain::decode(&args)?;
                    (a.item_id, a.expected_revision)
                } else if method == "daygrid_set_item_status" {
                    let a: domain::Status = domain::decode(&args)?;
                    (a.item_id, a.expected_revision)
                } else {
                    let a: domain::Lifecycle = domain::decode(&args)?;
                    (a.item_id, a.expected_revision)
                };
                domain::uuid(&id)?;
                let mut i = self.item(&id, method == "desktop_restore")?;
                if revision < 1 || i.revision != revision {
                    let mut e = Error::new(
                        "REVISION_CONFLICT",
                        "事项已被修改，草稿已保留，请读取最新内容。",
                    );
                    e.details = json!({"current_revision":i.revision,"current_item":i});
                    return Err(e);
                }
                let before = i.clone();
                let action = match method {
                    "daygrid_update_item" => {
                        let a: domain::Update = domain::decode(&args)?;
                        a.patch.apply(&mut i)?;
                        "update"
                    }
                    "daygrid_set_item_status" => {
                        let a: domain::Status = domain::decode(&args)?;
                        i.status = a.status;
                        domain::validate(&mut i)?;
                        "status"
                    }
                    "desktop_delete" => "delete",
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
}
