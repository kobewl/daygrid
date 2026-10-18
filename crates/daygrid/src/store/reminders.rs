//! 提醒的派生表、提示文件与到期处理。
//!
//! `remind_at` 的唯一真值在事项 JSON（`items.body`）里；`reminders` 表是派生出来的索引，并记录「已经触发」。
//!
//! # 不变量
//! 对每个事项，`reminders` 里有一行，当且仅当它满足 `status='planned'`、未删除、`remind_at` 不为空，
//! 并且这一行的 `due_ms = epoch_ms(remind_at)`。所有修改 `items` 的地方都在同一个事务里调用 [`sync_row`]。
use super::*;
use crate::reminder::{self, Due, TakeDue};
use chrono::DateTime;

pub(super) fn create_tables(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS reminders(item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,due_ms INTEGER NOT NULL,fired_at TEXT);
         CREATE INDEX IF NOT EXISTS reminders_pending ON reminders(due_ms) WHERE fired_at IS NULL;",
    )
}

/// 迁移到 v5 时执行一次：给 body 里已有 remind_at 的事项补上提醒行。
pub(super) fn backfill(db: &Connection, now_ms: i64) -> Result<()> {
    let rows: Vec<(String, Option<String>)> = {
        let mut stmt = db.prepare(
            "SELECT body,deleted_at FROM items WHERE json_extract(body,'$.remind_at') IS NOT NULL",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?
    };
    for (body, deleted_at) in rows {
        let item: Item = serde_json::from_str(&body)
            .map_err(|_| Error::new("STORAGE_ERROR", "事项数据无法读取。"))?;
        sync_row(db, &item, deleted_at.is_some(), now_ms)?;
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum RowAction {
    Delete,
    Keep,
    Upsert { due_ms: i64, fired: bool },
}

/// `existing`：已有行的 (due_ms, 是否已触发)；`target`：事项有资格触发时的 due_ms。
fn decide(existing: Option<(i64, bool)>, target: Option<i64>, now_ms: i64) -> RowAction {
    match (existing, target) {
        // 已完成、已删除或没有提醒
        (_, None) => RowAction::Delete,
        // 时刻没变：保留原来的触发状态
        (Some((due, _)), Some(t)) if due == t => RowAction::Keep,
        // 新提醒、时间改了、重新打开、恢复：已经过去（超出容差）的直接作废，不补发
        (_, Some(t)) => RowAction::Upsert {
            due_ms: t,
            fired: t < now_ms - reminder::PAST_TOLERANCE_MS,
        },
    }
}

/// 在调用方的事务里，让 `item` 的提醒行符合不变量。now_ms 来自调用方，便于测试。
pub(super) fn sync_row(db: &Connection, item: &Item, deleted: bool, now_ms: i64) -> Result<()> {
    let target = if item.status == "planned" && !deleted {
        item.remind_at.as_deref().and_then(domain::remind_epoch_ms)
    } else {
        None
    };
    let existing: Option<(i64, bool)> = db
        .query_row(
            "SELECT due_ms,fired_at IS NOT NULL FROM reminders WHERE item_id=?1",
            [&item.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match decide(existing, target, now_ms) {
        RowAction::Delete => {
            if existing.is_some() {
                db.execute("DELETE FROM reminders WHERE item_id=?1", [&item.id])?;
            }
        }
        RowAction::Keep => {}
        RowAction::Upsert { due_ms, fired } => {
            db.execute(
                "INSERT INTO reminders(item_id,due_ms,fired_at) VALUES(?1,?2,?3) ON CONFLICT(item_id) DO UPDATE SET due_ms=excluded.due_ms,fired_at=excluded.fired_at",
                params![item.id, due_ms, fired.then(stamp)],
            )?;
        }
    }
    Ok(())
}

fn next_due(db: &Connection) -> rusqlite::Result<Option<i64>> {
    db.query_row(
        "SELECT MIN(due_ms) FROM reminders WHERE fired_at IS NULL",
        [],
        |r| r.get(0),
    )
}

impl Store {
    /// 下一个待触发提醒的 Unix 毫秒，走部分索引 reminders_pending。公开，供性能测试直接计时。
    pub fn next_reminder_due(&self) -> Result<Option<i64>> {
        Ok(next_due(&self.db)?)
    }

    /// 与缓存比较，最早的待触发时间变了才改写提示文件。失败只记 warn，不影响业务结果。
    pub fn refresh_reminder_hint(&mut self) {
        match self.next_reminder_due() {
            Ok(next) if self.reminder_hint != Some(next) => self.write_reminder_hint(next),
            Ok(_) => {}
            Err(e) => tracing::warn!(code = e.code, "reminder hint query failed"),
        }
    }

    /// 同上，但不比较缓存，总是重写（take 之后用：提示文件被删或损坏时借此恢复）。
    fn refresh_reminder_hint_force(&mut self) {
        match self.next_reminder_due() {
            Ok(next) => self.write_reminder_hint(next),
            Err(e) => tracing::warn!(code = e.code, "reminder hint query failed"),
        }
    }

    /// 先写 `reminders.next.tmp`（0600）再改名，桌面进程永远读不到写了一半的文件。
    /// 服务进程持有单实例锁，只有一个写入方，临时文件名可以固定。
    fn write_reminder_hint(&mut self, next: Option<i64>) {
        let dir = self.data_dir().to_path_buf();
        let tmp = dir.join(format!("{}.tmp", reminder::HINT_FILE));
        let written = (|| -> std::io::Result<()> {
            use std::{io::Write, os::unix::fs::OpenOptionsExt};
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(json!({"v":1,"next_due_ms":next}).to_string().as_bytes())?;
            fs::rename(&tmp, dir.join(reminder::HINT_FILE))
        })();
        match written {
            Ok(()) => self.reminder_hint = Some(next),
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                tracing::warn!(kind = ?e.kind(), "reminder hint write failed");
            }
        }
    }

    /// 取出到期的提醒并标记为已触发（最多一次）。`now` 由调用方注入，便于测试。
    /// 24 小时以内的返回给桌面进程通知；更早的、以及不满足资格的只计入 `skipped`。
    pub fn take_due_reminders_at(&mut self, now: DateTime<Utc>) -> Result<Value> {
        let tz = self.calendar_tz()?;
        let now_ms = now.timestamp_millis();
        let tx = self.db.transaction()?;
        let rows: Vec<(String, i64, String, String, Option<String>)> = {
            let mut stmt = tx.prepare_cached(
                "SELECT r.item_id,r.due_ms,i.body,i.status,i.deleted_at FROM reminders r JOIN items i ON i.id=r.item_id WHERE r.fired_at IS NULL AND r.due_ms<=?1 ORDER BY r.due_ms,r.item_id LIMIT ?2",
            )?;
            stmt.query_map(params![now_ms + 999, reminder::TAKE_LIMIT], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<std::result::Result<_, _>>()?
        };
        let fired_at = stamp();
        let mut dues = Vec::new();
        let mut skipped = 0u32;
        for (id, due_ms, body, status, deleted_at) in rows {
            tx.execute(
                "UPDATE reminders SET fired_at=?1 WHERE item_id=?2",
                params![fired_at, id],
            )?;
            let eligible = status == "planned" && deleted_at.is_none();
            match serde_json::from_str::<Item>(&body) {
                Ok(item) if eligible && due_ms >= now_ms - reminder::MISSED_WINDOW_MS => {
                    dues.push(Due {
                        excerpt: reminder::excerpt(&item.notes),
                        item_id: item.id,
                        title: item.title,
                        date: item.date,
                        remind_at: item.remind_at.unwrap_or_default(),
                        late_seconds: ((now_ms - due_ms) / 1000).max(0),
                    });
                }
                Ok(_) if eligible => skipped += 1,
                _ => {
                    // 按不变量不应出现（行只代表有资格的提醒）：同样作废，留一条记录。
                    skipped += 1;
                    tracing::warn!("ineligible reminder row voided");
                }
            }
        }
        // 下一个时间在事务里算好：提交之后不会再因为查询失败丢掉已经标记的提醒。
        let next_due_ms = next_due(&tx)?;
        tx.commit()?;
        self.refresh_reminder_hint_force();
        tracing::info!(fired = dues.len(), skipped, "due reminders taken");
        let today = now.with_timezone(&tz).format("%Y-%m-%d").to_string();
        Ok(success(
            serde_json::to_value(TakeDue {
                today,
                reminders: dues,
                skipped,
                next_due_ms,
            })
            .unwrap(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_760_922_000_000;

    #[test]
    fn decide_covers_every_branch() {
        use RowAction::*;
        // 没有资格：不管有没有行都删除
        assert_eq!(decide(None, None, NOW), Delete);
        assert_eq!(decide(Some((NOW, false)), None, NOW), Delete);
        assert_eq!(decide(Some((NOW, true)), None, NOW), Delete);
        // 时刻没变：保留触发状态（已触发的不会重新生效）
        assert_eq!(decide(Some((NOW + 5, false)), Some(NOW + 5), NOW), Keep);
        assert_eq!(decide(Some((NOW - 5, true)), Some(NOW - 5), NOW), Keep);
        // 新提醒
        assert_eq!(
            decide(None, Some(NOW + 1), NOW),
            Upsert {
                due_ms: NOW + 1,
                fired: false
            }
        );
        // 改了时间：重新生效（即使原来已触发）
        assert_eq!(
            decide(Some((NOW - 10_000, true)), Some(NOW + 60_000), NOW),
            Upsert {
                due_ms: NOW + 60_000,
                fired: false
            }
        );
        // 60 秒容差以内仍待触发；刚好在边界上也算
        assert_eq!(
            decide(None, Some(NOW - 60_000), NOW),
            Upsert {
                due_ms: NOW - 60_000,
                fired: false
            }
        );
        // 超出容差：直接作废（重新打开、恢复一个提醒已过的事项）
        assert_eq!(
            decide(None, Some(NOW - 60_001), NOW),
            Upsert {
                due_ms: NOW - 60_001,
                fired: true
            }
        );
        assert_eq!(
            decide(Some((NOW + 1, false)), Some(NOW - 3_600_000), NOW),
            Upsert {
                due_ms: NOW - 3_600_000,
                fired: true
            }
        );
    }
}
