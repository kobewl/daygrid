//! 事项提醒：存储往返、提醒行的不变量、提示文件、日期平移、确认流、只读成员与到期处理。
use chrono::{DateTime, Duration, NaiveDate, Utc};
use daygrid::store::Store;
use serde_json::{Value, json};
use std::path::PathBuf;
use uuid::Uuid;

const SECRET: &str = "secret";
const PAST: &str = "提醒时间已经过去，请选择将来的时间。";

fn ms(s: &str) -> i64 {
    DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis()
}
fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

struct H {
    dir: tempfile::TempDir,
    store: Store,
    generation: String,
}

impl H {
    /// 日历时区固定为 Asia/Shanghai，结果与运行测试的机器无关。
    fn new() -> Self {
        Self::in_tz("Asia/Shanghai")
    }
    fn in_tz(tz: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("calendar.db");
        drop(Store::open(&path, SECRET).unwrap());
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute("UPDATE metadata SET value=?1 WHERE key='timezone'", [tz])
            .unwrap();
        let mut store = Store::open(&path, SECRET).unwrap();
        let generation = store.call("desktop", SECRET, "daygrid_get_context", json!({}))["data"]
            ["calendar_generation"]
            .as_str()
            .unwrap()
            .to_string();
        Self {
            dir,
            store,
            generation,
        }
    }
    fn path(&self) -> PathBuf {
        self.dir.path().join("calendar.db")
    }
    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.path()).unwrap()
    }
    fn desk(&mut self, method: &str, args: Value) -> Value {
        self.store.call("desktop", SECRET, method, args)
    }
    fn create_args(&self, extra: Value) -> Value {
        let mut a = json!({"calendar_generation":self.generation,"idempotency_key":Uuid::new_v4().to_string(),"date":"2030-10-20","title":"周会","notes":"- [ ] 带上评审清单\n第二行"});
        for (k, v) in extra.as_object().unwrap() {
            a[k] = v.clone();
        }
        a
    }
    fn create(&mut self, extra: Value) -> Value {
        let args = self.create_args(extra);
        let r = self.desk("daygrid_create_item", args);
        assert_eq!(r["ok"], true, "{r}");
        r["data"]["item"].clone()
    }
    fn update_args(&self, item: &Value, patch: Value) -> Value {
        json!({"calendar_generation":self.generation,"idempotency_key":Uuid::new_v4().to_string(),"item_id":item["id"],"expected_revision":item["revision"],"patch":patch})
    }
    fn update(&mut self, item: &Value, patch: Value) -> Value {
        let args = self.update_args(item, patch);
        self.desk("daygrid_update_item", args)
    }
    fn lifecycle(&mut self, method: &str, item: &Value) -> Value {
        let args = json!({"calendar_generation":self.generation,"idempotency_key":Uuid::new_v4().to_string(),"item_id":item["id"],"expected_revision":item["revision"]});
        self.desk(method, args)
    }
    fn status(&mut self, item: &Value, status: &str) -> Value {
        let args = json!({"calendar_generation":self.generation,"idempotency_key":Uuid::new_v4().to_string(),"item_id":item["id"],"expected_revision":item["revision"],"status":status});
        self.desk("daygrid_set_item_status", args)
    }
    fn get(&mut self, id: &Value) -> Value {
        self.desk("daygrid_get_item", json!({"item_id":id}))["data"]["item"].clone()
    }
    /// 提醒行：(due_ms, 是否已触发)。
    fn row(&self, id: &Value) -> Option<(i64, bool)> {
        use rusqlite::OptionalExtension;
        self.db()
            .query_row(
                "SELECT due_ms,fired_at IS NOT NULL FROM reminders WHERE item_id=?1",
                [id.as_str().unwrap()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .unwrap()
    }
    fn hint(&self) -> Value {
        serde_json::from_str(
            &std::fs::read_to_string(self.dir.path().join("reminders.next")).unwrap(),
        )
        .unwrap()
    }
    /// 模拟时间流逝：直接把存储的提醒改成过去的时刻（只在事项没有资格触发、没有提醒行时用）。
    fn set_stored_remind_at(&self, id: &Value, remind_at: &str) {
        self.db()
            .execute(
                "UPDATE items SET body=json_set(body,'$.remind_at',?1) WHERE id=?2",
                rusqlite::params![remind_at, id.as_str().unwrap()],
            )
            .unwrap();
    }
    fn take(&mut self, now: &str) -> Value {
        let v = self.store.take_due_reminders_at(utc(now)).unwrap();
        assert_eq!(v["ok"], true, "{v}");
        v["data"].clone()
    }
    /// 建立助手连接，返回 (id, token)。
    fn connect(&mut self, permission: &str) -> (String, String) {
        let v = self.desk(
            "desktop_create_connection",
            json!({"label":"测试助手","permission":permission}),
        );
        (
            v["data"]["id"].as_str().unwrap().into(),
            v["data"]["credential"].as_str().unwrap().into(),
        )
    }
    fn as_conn(&mut self, c: &(String, String), method: &str, args: Value) -> Value {
        self.store.call(&c.0, &c.1, method, args)
    }
}

#[test]
fn create_reads_back_normalized_and_maintains_row_and_hint() {
    let mut h = H::new();
    assert_eq!(h.hint(), json!({"v":1,"next_due_ms":null}));
    let item = h.create(json!({"remind_at":"2030-10-20T09:00"}));
    assert_eq!(item["remind_at"], "2030-10-20T09:00:00+08:00");
    let due = ms("2030-10-20T09:00:00+08:00");
    assert_eq!(h.row(&item["id"]), Some((due, false)));
    assert_eq!(h.hint(), json!({"v":1,"next_due_ms":due}));
    assert_eq!(h.get(&item["id"])["remind_at"], "2030-10-20T09:00:00+08:00");
    let list = h.desk(
        "daygrid_list_items",
        json!({"from_date":"2030-10-01","to_date":"2030-10-31"}),
    );
    assert_eq!(
        list["data"]["items"][0]["remind_at"],
        "2030-10-20T09:00:00+08:00"
    );
    // 不传、传 null：都没有提醒，字段照样出现
    for extra in [json!({}), json!({"remind_at":null})] {
        let plain = h.create(extra);
        assert!(plain.as_object().unwrap().contains_key("remind_at"));
        assert!(plain["remind_at"].is_null());
        assert_eq!(h.row(&plain["id"]), None);
    }
    // 更早的提醒让提示文件前移；带 Z 和首尾空白的输入同样接受
    let earlier = h.create(json!({"remind_at":" 2030-10-19t09:00Z "}));
    assert_eq!(earlier["remind_at"], "2030-10-19T17:00:00+08:00");
    assert_eq!(
        h.hint()["next_due_ms"],
        json!(ms("2030-10-19T17:00:00+08:00"))
    );
    assert_eq!(
        h.store.next_reminder_due().unwrap(),
        Some(ms("2030-10-19T17:00:00+08:00"))
    );
}

#[test]
fn invalid_reminders_are_rejected_with_details() {
    let mut h = H::new();
    for bad in [
        "tomorrow 9am",
        "2030-10-20T09:00+0800",
        "2020-01-01T09:00",
        "1899-12-31T09:00",
        "2030-10-20T09:00:00.123456789+08:00000000",
        "2030-03-10T02:30:00 bad",
    ] {
        let args = h.create_args(json!({"remind_at":bad}));
        let r = h.desk("daygrid_create_item", args);
        assert_eq!(r["ok"], false, "{bad}");
        assert_eq!(r["error"]["code"], "VALIDATION_ERROR", "{bad}");
        assert_eq!(r["error"]["details"]["field"], "remind_at");
        assert_eq!(r["error"]["details"]["calendar_timezone"], "Asia/Shanghai");
        assert!(r["error"]["details"]["now"].is_string());
    }
    let args = h.create_args(json!({"remind_at":"2020-01-01T09:00"}));
    assert_eq!(
        h.desk("daygrid_create_item", args)["error"]["message"],
        PAST
    );
    // 类型错误
    let args = h.create_args(json!({"remind_at":9}));
    assert_eq!(
        h.desk("daygrid_create_item", args)["error"]["code"],
        "VALIDATION_ERROR"
    );
    assert_eq!(
        h.desk(
            "daygrid_list_items",
            json!({"from_date":"2030-10-01","to_date":"2030-10-31"})
        )["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "校验失败不落库"
    );
}

#[test]
fn null_clears_the_reminder_and_other_nulls_are_still_rejected() {
    let mut h = H::new();
    let item = h.create(json!({"remind_at":"2030-10-20T09:00"}));
    let r = h.update(&item, json!({"title":null}));
    assert_eq!(r["error"]["message"], "字段不能为 null。");
    let mut args = h.update_args(&item, json!({"remind_at":null}));
    args["expected_revision"] = Value::Null;
    assert_eq!(
        h.desk("daygrid_update_item", args)["error"]["message"],
        "字段不能为 null。"
    );
    let args = h.create_args(json!({"notes":null,"remind_at":null}));
    assert_eq!(
        h.desk("daygrid_create_item", args)["error"]["message"],
        "字段不能为 null。"
    );
    assert_eq!(
        h.update(&item, json!({}))["error"]["message"],
        "修改至少需要一个字段。"
    );
    let r = h.update(&item, json!({"remind_at":null}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(r["data"]["item"]["remind_at"].is_null());
    assert_eq!(r["data"]["item"]["revision"], 2);
    assert_eq!(h.row(&item["id"]), None);
    assert_eq!(h.hint()["next_due_ms"], Value::Null);
    // 再设回来
    let item = r["data"]["item"].clone();
    let r = h.update(&item, json!({"remind_at":"2030-10-21T08:30"}));
    assert_eq!(r["data"]["item"]["remind_at"], "2030-10-21T08:30:00+08:00");
    assert_eq!(
        h.row(&item["id"]),
        Some((ms("2030-10-21T08:30:00+08:00"), false))
    );
    // 修改记录里能看到提醒字段的变化
    let history = h.desk("desktop_history", json!({}));
    assert_eq!(
        history["data"]["history"][0]["after"]["remind_at"],
        "2030-10-21T08:30:00+08:00"
    );
    assert!(history["data"]["history"][0]["before"]["remind_at"].is_null());
}

#[test]
fn done_delete_reopen_and_restore_keep_the_row_in_step() {
    let mut h = H::new();
    let item = h.create(json!({"remind_at":"2030-10-20T09:00"}));
    let due = ms("2030-10-20T09:00:00+08:00");
    let done = h.status(&item, "done")["data"]["item"].clone();
    assert_eq!(h.row(&item["id"]), None, "已完成不提醒");
    assert_eq!(h.hint()["next_due_ms"], Value::Null);
    // 到点前重新打开：照常生效
    let open = h.status(&done, "planned")["data"]["item"].clone();
    assert_eq!(h.row(&item["id"]), Some((due, false)));
    let deleted = h.lifecycle("desktop_delete", &open)["data"]["item"].clone();
    assert_eq!(h.row(&item["id"]), None, "已删除不提醒");
    let restored = h.lifecycle("desktop_restore", &deleted)["data"]["item"].clone();
    assert_eq!(h.row(&item["id"]), Some((due, false)));

    // 到点后再重新打开 / 恢复：直接作废，不补发
    let done = h.status(&restored, "done")["data"]["item"].clone();
    h.set_stored_remind_at(&item["id"], "2020-01-01T09:00:00+08:00");
    let open = h.status(&done, "planned");
    assert_eq!(open["ok"], true, "{open}");
    let open = open["data"]["item"].clone();
    assert_eq!(
        h.row(&item["id"]),
        Some((ms("2020-01-01T09:00:00+08:00"), true))
    );
    assert_eq!(h.hint()["next_due_ms"], Value::Null);
    let deleted = h.lifecycle("desktop_delete", &open)["data"]["item"].clone();
    assert_eq!(h.row(&item["id"]), None);
    h.lifecycle("desktop_restore", &deleted);
    assert_eq!(
        h.row(&item["id"]),
        Some((ms("2020-01-01T09:00:00+08:00"), true))
    );
    let data = h.take("2030-10-20T01:00:00Z");
    assert_eq!(data["reminders"], json!([]));
    assert_eq!(data["skipped"], 0, "作废的行不会再被取到");
}

#[test]
fn date_only_changes_shift_the_reminder() {
    let mut h = H::new();
    let item = h.create(json!({"remind_at":"2030-10-19T21:00"}));
    let r = h.update(&item, json!({"date":"2030-10-25"}));
    let item = r["data"]["item"].clone();
    assert_eq!(item["remind_at"], "2030-10-24T21:00:00+08:00");
    assert_eq!(
        h.row(&item["id"]),
        Some((ms("2030-10-24T21:00:00+08:00"), false))
    );
    // 跨月
    let item = h.update(&item, json!({"date":"2030-11-02"}))["data"]["item"].clone();
    assert_eq!(item["remind_at"], "2030-11-01T21:00:00+08:00");
    // 同时给了 remind_at：以给出的为准，不平移
    let item = h.update(
        &item,
        json!({"date":"2030-11-10","remind_at":"2030-11-03T07:00"}),
    )["data"]["item"]
        .clone();
    assert_eq!(item["remind_at"], "2030-11-03T07:00:00+08:00");
    // 只改标题：不动提醒
    let item = h.update(&item, json!({"title":"新标题"}))["data"]["item"].clone();
    assert_eq!(item["remind_at"], "2030-11-03T07:00:00+08:00");
    assert_eq!(
        h.row(&item["id"]),
        Some((ms("2030-11-03T07:00:00+08:00"), false))
    );
}

#[test]
fn assistant_writes_follow_the_same_rules_and_are_audited() {
    let mut h = H::new();
    let conn = h.connect("write");
    let args = h.create_args(json!({"remind_at":"2030-10-20T09:00"}));
    let r = h.as_conn(&conn, "daygrid_create_item", args);
    assert_eq!(r["ok"], true, "{r}");
    let item = r["data"]["item"].clone();
    assert_eq!(item["remind_at"], "2030-10-20T09:00:00+08:00");
    assert_eq!(
        h.row(&item["id"]),
        Some((ms("2030-10-20T09:00:00+08:00"), false))
    );
    let args = h.update_args(&item, json!({"remind_at":"2020-01-01T09:00"}));
    let r = h.as_conn(&conn, "daygrid_update_item", args);
    assert_eq!(r["error"]["message"], PAST);
    let args = h.update_args(&item, json!({"remind_at":null}));
    let r = h.as_conn(&conn, "daygrid_update_item", args);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.row(&item["id"]), None);
    let activity = h.desk("desktop_activity", json!({}));
    let tools: Vec<(&str, &str)> = activity["data"]["activity"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["tool"].as_str().unwrap(), a["outcome"].as_str().unwrap()))
        .collect();
    assert_eq!(
        tools,
        [
            ("daygrid_update_item", "ok"),
            ("daygrid_update_item", "error"),
            ("daygrid_create_item", "ok")
        ]
    );
    // 只读连接不能设置提醒
    let reader = h.connect("read");
    let args = h.create_args(json!({"remind_at":"2030-10-20T09:00"}));
    assert_eq!(
        h.as_conn(&reader, "daygrid_create_item", args)["error"]["code"],
        "PERMISSION_DENIED"
    );
}

#[test]
fn all_writes_mode_is_strict_on_submit_and_lenient_on_approval() {
    let mut h = H::new();
    let r = h.desk(
        "desktop_set_settings",
        json!({"approval_mode":"all_writes"}),
    );
    assert_eq!(r["ok"], true);
    let conn = h.connect("write");
    // 提交时严格检查
    let args = h.create_args(json!({"remind_at":"2020-01-01T09:00"}));
    assert_eq!(
        h.as_conn(&conn, "daygrid_create_item", args)["error"]["message"],
        PAST
    );
    let args = h.create_args(json!({"remind_at":"2030-10-20T09:00"}));
    let r = h.as_conn(&conn, "daygrid_create_item", args);
    assert_eq!(r["data"]["pending_approval"], true, "{r}");
    assert_eq!(
        r["data"]["proposal"]["summary"],
        "创建「周会」· 2030-10-20 · 提醒 10月20日 09:00"
    );
    let pid = r["data"]["proposal"]["id"].clone();
    let d = h.desk(
        "desktop_decide_proposal",
        json!({"proposal_id":pid,"decision":"approve"}),
    );
    let item = d["data"]["proposal"]["result"]["item"].clone();
    assert_eq!(item["remind_at"], "2030-10-20T09:00:00+08:00", "{d}");
    assert_eq!(
        h.row(&item["id"]),
        Some((ms("2030-10-20T09:00:00+08:00"), false))
    );
    // 取消提醒的摘要
    let args = h.update_args(&item, json!({"remind_at":null}));
    let r = h.as_conn(&conn, "daygrid_update_item", args);
    assert_eq!(r["data"]["proposal"]["summary"], "修改「周会」 · 取消提醒");
    let args = h.update_args(&item, json!({"remind_at":"2030-10-21T08:05"}));
    let r = h.as_conn(&conn, "daygrid_update_item", args);
    assert_eq!(
        r["data"]["proposal"]["summary"],
        "修改「周会」 · 提醒 10月21日 08:05"
    );
    let args = h.update_args(&item, json!({"title":"只改标题"}));
    let r = h.as_conn(&conn, "daygrid_update_item", args);
    assert_eq!(r["data"]["proposal"]["summary"], "修改「周会」");
    // 批准时放宽：用户在提醒时间过后才批准，事项照常创建，过期的提醒直接作废。
    let args = h.create_args(json!({"title":"晚批准","remind_at":"2030-10-22T09:00"}));
    let r = h.as_conn(&conn, "daygrid_create_item", args);
    let pid = r["data"]["proposal"]["id"].as_str().unwrap().to_string();
    // 模拟时间流逝：把请求里的提醒改成过去的时刻。
    h.db()
        .execute(
            "UPDATE proposals SET args=json_set(args,'$.remind_at','2020-01-01T09:00') WHERE id=?1",
            [&pid],
        )
        .unwrap();
    let d = h.desk(
        "desktop_decide_proposal",
        json!({"proposal_id":pid,"decision":"approve"}),
    );
    assert_eq!(d["data"]["proposal"]["state"], "approved", "{d}");
    let late = d["data"]["proposal"]["result"]["item"].clone();
    assert_eq!(late["remind_at"], "2020-01-01T09:00:00+08:00");
    assert_eq!(
        h.row(&late["id"]),
        Some((ms("2020-01-01T09:00:00+08:00"), true)),
        "过期的提醒作废，不补发"
    );
}

#[test]
fn viewer_can_only_change_their_own_reminder_on_shared_items() {
    let mut h = H::new();
    let item = h.create(json!({}));
    // 模拟已连接团队、角色是只读成员，事项是团队共享的。
    let db = h.db();
    db.execute_batch(
        "INSERT INTO metadata VALUES('sync_workspace','ws');
         INSERT INTO metadata VALUES('sync_client_id','client');
         INSERT INTO metadata VALUES('sync_role','viewer');",
    )
    .unwrap();
    db.execute(
        "UPDATE items SET body=json_set(body,'$.visibility','shared') WHERE id=?1",
        [item["id"].as_str().unwrap()],
    )
    .unwrap();
    let r = h.update(&item, json!({"remind_at":"2030-10-20T08:00"}));
    assert_eq!(r["ok"], true, "{r}");
    let item = r["data"]["item"].clone();
    assert_eq!(item["remind_at"], "2030-10-20T08:00:00+08:00");
    assert!(h.row(&item["id"]).is_some());
    let r = h.update(
        &item,
        json!({"title":"改标题","remind_at":"2030-10-20T07:00"}),
    );
    assert_eq!(r["error"]["code"], "PERMISSION_DENIED");
    let r = h.update(&item, json!({"date":"2030-10-21"}));
    assert_eq!(r["error"]["code"], "PERMISSION_DENIED");
    assert_eq!(
        h.status(&item, "done")["error"]["code"],
        "PERMISSION_DENIED"
    );
    let r = h.update(&item, json!({"remind_at":null}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.row(&item["id"]), None);
    let outbox: i64 = h
        .db()
        .query_row("SELECT COUNT(*) FROM sync_outbox", [], |r| r.get(0))
        .unwrap();
    assert_eq!(outbox, 0, "提醒不产生同步操作");
}

#[test]
fn take_due_fires_once_catches_up_within_24h_and_rearms() {
    let mut h = H::new();
    let a = h.create(json!({"title":"A","remind_at":"2030-10-20T09:00"}));
    let b = h
        .create(json!({"title":"B","date":"2030-10-21","notes":"","remind_at":"2030-10-21T09:00"}));
    let c = h.create(json!({"title":"C","remind_at":"2030-10-22T09:00"}));
    // 还差 2 秒：不取
    let data = h.take("2030-10-20T00:59:58Z");
    assert_eq!(data["reminders"], json!([]));
    assert_eq!(data["next_due_ms"], json!(ms("2030-10-20T09:00:00+08:00")));
    // 到点：返回一次
    let data = h.take("2030-10-20T01:00:00.300Z");
    assert_eq!(
        data,
        json!({"today":"2030-10-20","skipped":0,"next_due_ms":ms("2030-10-21T09:00:00+08:00"),
               "reminders":[{"item_id":a["id"],"title":"A","date":"2030-10-20",
                             "remind_at":"2030-10-20T09:00:00+08:00","excerpt":"带上评审清单","late_seconds":0}]})
    );
    assert_eq!(
        h.row(&a["id"]),
        Some((ms("2030-10-20T09:00:00+08:00"), true))
    );
    assert_eq!(
        h.hint()["next_due_ms"],
        json!(ms("2030-10-21T09:00:00+08:00"))
    );
    // 只触发一次
    let data = h.take("2030-10-20T01:00:05Z");
    assert_eq!(data["reminders"], json!([]));
    // 应用没运行，2 小时后补发：带迟到秒数
    let data = h.take("2030-10-21T03:00:00Z");
    assert_eq!(data["reminders"][0]["item_id"], b["id"]);
    assert_eq!(data["reminders"][0]["late_seconds"], 7200);
    assert_eq!(data["reminders"][0]["excerpt"], "");
    assert_eq!(data["reminders"].as_array().unwrap().len(), 1);
    // 超过 24 小时：静默作废，只计入 skipped，之后不再出现
    let data = h.take("2030-10-23T02:00:00Z");
    assert_eq!(data["reminders"], json!([]));
    assert_eq!(data["skipped"], 1);
    assert_eq!(data["next_due_ms"], Value::Null);
    assert_eq!(
        h.row(&c["id"]),
        Some((ms("2030-10-22T09:00:00+08:00"), true))
    );
    let data = h.take("2030-10-23T02:00:01Z");
    assert_eq!(
        (data["reminders"].clone(), data["skipped"].clone()),
        (json!([]), json!(0))
    );
    assert_eq!(h.hint()["next_due_ms"], Value::Null);
    // 触发后改到新的未来时间：重新生效
    let a = h.get(&a["id"]);
    let r = h.update(&a, json!({"remind_at":"2030-10-25T09:00"}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(
        h.row(&a["id"]),
        Some((ms("2030-10-25T09:00:00+08:00"), false))
    );
    assert_eq!(
        h.hint()["next_due_ms"],
        json!(ms("2030-10-25T09:00:00+08:00"))
    );
    let data = h.take("2030-10-25T01:00:00Z");
    assert_eq!(data["reminders"][0]["item_id"], a["id"]);
    // 原样再设一次同一个时刻：保持已触发，不会重复
    let a = h.get(&a["id"]);
    h.update(&a, json!({"remind_at":"2030-10-25T09:00"}));
    assert_eq!(
        h.row(&a["id"]),
        Some((ms("2030-10-25T09:00:00+08:00"), true))
    );
}

#[test]
fn take_due_orders_by_time_and_respects_the_batch_limit() {
    let mut h = H::new();
    // 同一分钟 3 个、另一分钟 1 个
    let mut ids = vec![];
    for (title, at) in [
        ("晚", "2030-10-20T09:01"),
        ("早1", "2030-10-20T09:00"),
        ("早2", "2030-10-20T09:00"),
    ] {
        ids.push(h.create(json!({"title":title,"remind_at":at}))["id"].clone());
    }
    let data = h.take("2030-10-20T01:05:00Z");
    let titles: Vec<&str> = data["reminders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles.len(), 3);
    assert_eq!(titles[2], "晚");
    assert_eq!(data["reminders"][2]["late_seconds"], 240);
}

#[test]
fn take_due_today_is_in_the_calendar_timezone() {
    let mut h = H::new();
    let data = h.take("2026-10-19T16:30:00Z");
    assert_eq!(data["today"], "2026-10-20");
    assert_eq!(data["reminders"], json!([]));
}

#[test]
fn take_due_rewrites_a_missing_hint_file() {
    let mut h = H::new();
    h.create(json!({"remind_at":"2030-10-20T09:00"}));
    let file = h.dir.path().join("reminders.next");
    std::fs::remove_file(&file).unwrap();
    // 普通调用不会重写（缓存值没变）
    h.desk("daygrid_get_context", json!({}));
    assert!(!file.exists());
    h.take("2030-01-01T00:00:00Z");
    assert_eq!(
        h.hint(),
        json!({"v":1,"next_due_ms":ms("2030-10-20T09:00:00+08:00")})
    );
    assert!(!h.dir.path().join("reminders.next.tmp").exists());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn desktop_take_due_reminders_needs_empty_args_and_the_desktop_token() {
    let mut h = H::new();
    let r = h.desk("desktop_take_due_reminders", json!({}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(r["data"]["reminders"].is_array());
    let r = h.desk("desktop_take_due_reminders", json!({"x":1}));
    assert_eq!(r["error"]["code"], "VALIDATION_ERROR");
    let conn = h.connect("write");
    let r = h.as_conn(&conn, "desktop_take_due_reminders", json!({}));
    assert_eq!(r["error"]["code"], "PERMISSION_DENIED");
    let r = h.store.call(
        "desktop",
        "wrong-token",
        "desktop_take_due_reminders",
        json!({}),
    );
    assert_eq!(r["error"]["code"], "PERMISSION_DENIED");
}

/// 简单的确定性随机数（不引入 rand）。
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

#[test]
fn reminder_rows_match_items_after_random_operations() {
    let mut h = H::new();
    let mut rng = Lcg(20261020);
    let base = NaiveDate::from_ymd_opt(2030, 1, 1).unwrap();
    let mut ids: Vec<Value> = vec![];
    let day = |rng: &mut Lcg| (base + Duration::days(rng.next(300) as i64)).to_string();
    let read = |h: &H, id: &Value| -> (Value, bool) {
        h.db()
            .query_row(
                "SELECT body,deleted_at IS NOT NULL FROM items WHERE id=?1",
                [id.as_str().unwrap()],
                |r| {
                    Ok((
                        serde_json::from_str(&r.get::<_, String>(0)?).unwrap(),
                        r.get(1)?,
                    ))
                },
            )
            .unwrap()
    };
    for step in 0..200 {
        let op = if ids.is_empty() { 0 } else { rng.next(8) };
        if op == 0 {
            let remind = if rng.next(2) == 0 {
                json!(format!(
                    "{}T{:02}:{:02}",
                    day(&mut rng),
                    rng.next(24),
                    rng.next(60)
                ))
            } else {
                Value::Null
            };
            let date = day(&mut rng);
            ids.push(h.create(json!({"date":date,"remind_at":remind}))["id"].clone());
            continue;
        }
        let id = ids[rng.next(ids.len() as u64) as usize].clone();
        let (item, deleted) = read(&h, &id);
        let r = match (op, deleted) {
            (7, true) => h.lifecycle("desktop_restore", &item),
            (_, true) => continue,
            (1, _) => {
                let date = day(&mut rng);
                h.update(&item, json!({"date":date}))
            }
            (2, _) => {
                let at = format!("{}T{:02}:{:02}", day(&mut rng), rng.next(24), rng.next(60));
                h.update(&item, json!({"remind_at":at}))
            }
            (3, _) => h.update(&item, json!({"remind_at":null})),
            (4, _) => h.status(&item, "done"),
            (5, _) => h.status(&item, "planned"),
            (6, _) => h.lifecycle("desktop_delete", &item),
            _ => continue,
        };
        assert_eq!(r["ok"], true, "step {step} op {op}: {r}");
    }
    // 不变量：有资格触发 ⇔ 有一行，且 due_ms = epoch(remind_at)。
    let mut eligible = 0;
    for id in &ids {
        let (item, deleted) = read(&h, id);
        let want = (item["status"] == "planned" && !deleted && item["remind_at"].is_string())
            .then(|| (ms(item["remind_at"].as_str().unwrap()), false));
        assert_eq!(h.row(id), want, "{item}");
        eligible += want.is_some() as i64;
    }
    let rows: i64 = h
        .db()
        .query_row("SELECT COUNT(*) FROM reminders", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, eligible);
    assert!(eligible > 0, "随机序列应留下一些提醒");
    let next: Option<i64> = h
        .db()
        .query_row(
            "SELECT MIN(due_ms) FROM reminders WHERE fired_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(h.hint()["next_due_ms"], json!(next));
}

// ---------- MCP 契约 0.3：校验、列表、上下文 now、旧回执补字段 ----------

const FORMAT: &str = "提醒时间格式无效，请使用 YYYY-MM-DDTHH:MM（日历时区）或带时区的 RFC 3339 时间，例如 2026-10-20T09:00 或 2026-10-20T09:00+08:00。";
const RANGE: &str = "提醒时间需要在 1900–2100 年之间。";
const GAP: &str = "这个时间在日历时区里不存在（夏令时切换），请换一个时间。";

/// `YYYY-MM-DDTHH:MM:SS±HH:MM`（不用 regex：逐字符比对模板）。
fn is_canonical_now(s: &str) -> bool {
    let template = "dddd-dd-ddTdd:dd:dd+dd:dd";
    s.len() == template.len()
        && s.chars().zip(template.chars()).all(|(c, t)| match t {
            'd' => c.is_ascii_digit(),
            '+' => c == '+' || c == '-',
            _ => c == t,
        })
}

/// 相对真实时钟的提醒时刻（UTC，分钟精度），用于依赖「现在」的列表测试。
fn from_now(minutes: i64) -> String {
    (Utc::now() + Duration::minutes(minutes))
        .format("%Y-%m-%dT%H:%MZ")
        .to_string()
}

#[test]
fn mcp_rejects_invalid_reminders_with_exact_messages_and_details() {
    let mut h = H::new();
    let conn = h.connect("write");
    let too_long = format!("2030-10-20T09:00:00.{}+08:00", "0".repeat(20));
    for (bad, message) in [
        ("tomorrow 9am", FORMAT),
        ("2026-10-20T09:00+0800", FORMAT),
        ("2020-01-01T09:00", PAST),
        (too_long.as_str(), FORMAT),
        ("1899-12-31T09:00", RANGE),
    ] {
        let args = h.create_args(json!({"remind_at":bad}));
        let r = h.as_conn(&conn, "daygrid_create_item", args);
        assert_eq!(r["ok"], false, "{bad}");
        assert_eq!(r["error"]["code"], "VALIDATION_ERROR", "{bad}");
        assert_eq!(r["error"]["message"], message, "{bad}");
        let details = &r["error"]["details"];
        assert_eq!(details["field"], "remind_at");
        assert_eq!(details["calendar_timezone"], "Asia/Shanghai");
        assert!(
            is_canonical_now(details["now"].as_str().unwrap()),
            "{details}"
        );
        assert!(details["now"].as_str().unwrap().ends_with("+08:00"));
    }
    // 首尾空白、小写 t 和 z 都接受，结果规范化到日历时区
    let args = h.create_args(json!({"remind_at":" 2030-10-20t09:00Z "}));
    let r = h.as_conn(&conn, "daygrid_create_item", args);
    assert_eq!(r["ok"], true, "{r}");
    let item = r["data"]["item"].clone();
    assert_eq!(item["remind_at"], "2030-10-20T17:00:00+08:00");
    // patch 里 remind_at 可以是 null（清除），其他字段仍然不行
    let args = h.update_args(&item, json!({"color":null}));
    assert_eq!(
        h.as_conn(&conn, "daygrid_update_item", args)["error"]["message"],
        "字段不能为 null。"
    );
    let args = h.update_args(&item, json!({"remind_at":null}));
    let r = h.as_conn(&conn, "daygrid_update_item", args);
    assert_eq!(r["ok"], true, "{r}");
    assert!(r["data"]["item"]["remind_at"].is_null());
    assert_eq!(h.row(&item["id"]), None);
}

#[test]
fn mcp_rejects_times_skipped_by_daylight_saving() {
    let mut h = H::in_tz("America/New_York");
    let conn = h.connect("write");
    // 2030-03-10 02:00–03:00 在纽约不存在
    let args = h.create_args(json!({"remind_at":"2030-03-10T02:30"}));
    let r = h.as_conn(&conn, "daygrid_create_item", args);
    assert_eq!(r["error"]["code"], "VALIDATION_ERROR", "{r}");
    assert_eq!(r["error"]["message"], GAP);
    assert_eq!(
        r["error"]["details"]["calendar_timezone"],
        "America/New_York"
    );
    let now = r["error"]["details"]["now"].as_str().unwrap();
    assert!(is_canonical_now(now), "{now}");
    // 夏令时边界外照常
    let args = h.create_args(json!({"remind_at":"2030-03-10T03:30"}));
    let r = h.as_conn(&conn, "daygrid_create_item", args);
    assert_eq!(r["data"]["item"]["remind_at"], "2030-03-10T03:30:00-04:00");
}

#[test]
fn get_context_reports_now_with_seconds() {
    let mut h = H::new();
    let ctx = h.desk("daygrid_get_context", json!({}));
    let now = ctx["data"]["now"].as_str().unwrap();
    assert!(is_canonical_now(now), "{now}");
    assert!(now.ends_with("+08:00"));
    assert!(now.starts_with(ctx["data"]["today"].as_str().unwrap()));
    // 与真实时钟一致（允许 5 秒误差）
    let delta = (ms(now) - Utc::now().timestamp_millis()).abs();
    assert!(delta < 5_000, "{now}");
}

#[test]
fn list_reminders_returns_upcoming_unfired_reminders_soonest_first() {
    let mut h = H::new();
    let conn = h.connect("read");
    let list = |h: &mut H, args: Value| -> Value {
        let r = h.as_conn(&conn, "daygrid_list_reminders", args);
        assert_eq!(r["ok"], true, "{r}");
        r["data"].clone()
    };
    let titles = |data: &Value| -> Vec<String> {
        data["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["title"].as_str().unwrap().to_string())
            .collect()
    };
    let empty = list(&mut h, json!({}));
    assert_eq!(empty["items"], json!([]));
    assert_eq!(empty["has_more"], false);
    assert!(is_canonical_now(empty["now"].as_str().unwrap()));

    let day = 24 * 60;
    let in3d = h.create(json!({"title":"三天后","remind_at":from_now(3 * day)}));
    let in1h = h.create(json!({"title":"一小时后","remind_at":from_now(60)}));
    h.create(json!({"title":"十天后","remind_at":from_now(10 * day)}));
    h.create(json!({"title":"没有提醒"}));
    // 默认 7 天，按时间排序
    let data = list(&mut h, json!({}));
    assert_eq!(titles(&data), ["一小时后", "三天后"]);
    assert_eq!(data["has_more"], false);
    assert_eq!(data["items"][0]["id"], in1h["id"]);
    assert_eq!(data["items"][0]["remind_at"], in1h["remind_at"]);
    assert_eq!(
        titles(&list(&mut h, json!({"within_days":30}))),
        ["一小时后", "三天后", "十天后"]
    );
    assert_eq!(
        titles(&list(&mut h, json!({"within_days":1}))),
        ["一小时后"]
    );
    // limit + has_more
    let data = list(&mut h, json!({"within_days":30,"limit":2}));
    assert_eq!(titles(&data), ["一小时后", "三天后"]);
    assert_eq!(data["has_more"], true);
    let data = list(&mut h, json!({"within_days":30,"limit":3}));
    assert_eq!(data["has_more"], false);

    // 已过期但未触发：60 秒以内照样列出（仍会触发），超过 60 秒的不列出（等下次运行时补发）
    let now_ms = Utc::now().timestamp_millis();
    let set_due = |h: &H, id: &Value, due_ms: i64| {
        h.db()
            .execute(
                "UPDATE reminders SET due_ms=?1 WHERE item_id=?2",
                rusqlite::params![due_ms, id.as_str().unwrap()],
            )
            .unwrap();
    };
    set_due(&h, &in1h["id"], now_ms - 30_000);
    assert_eq!(titles(&list(&mut h, json!({}))), ["一小时后", "三天后"]);
    set_due(&h, &in1h["id"], now_ms - 90_000);
    assert_eq!(titles(&list(&mut h, json!({}))), ["三天后"]);

    // 已触发、已完成、已删除的不列出
    let fired = h.create(json!({"title":"已触发","remind_at":from_now(30)}));
    h.db()
        .execute(
            "UPDATE reminders SET fired_at='2026-01-01T00:00:00.000Z' WHERE item_id=?1",
            [fired["id"].as_str().unwrap()],
        )
        .unwrap();
    let done = h.create(json!({"title":"已完成","remind_at":from_now(40)}));
    assert_eq!(h.status(&done, "done")["ok"], true);
    let gone = h.create(json!({"title":"已删除","remind_at":from_now(50)}));
    assert_eq!(h.lifecycle("desktop_delete", &gone)["ok"], true);
    assert_eq!(titles(&list(&mut h, json!({}))), ["三天后"]);
    assert_eq!(list(&mut h, json!({}))["items"][0]["id"], in3d["id"]);

    // 参数越界、未知字段、null
    for bad in [
        json!({"within_days":0}),
        json!({"within_days":367}),
        json!({"limit":0}),
        json!({"limit":101}),
        json!({"within_days":-1}),
    ] {
        let r = h.as_conn(&conn, "daygrid_list_reminders", bad.clone());
        assert_eq!(r["error"]["code"], "VALIDATION_ERROR", "{bad}");
        assert_eq!(r["error"]["message"], "范围或数量无效。", "{bad}");
    }
    for bad in [
        json!({"days":3}),
        json!({"limit":null}),
        json!({"limit":"5"}),
    ] {
        let r = h.as_conn(&conn, "daygrid_list_reminders", bad.clone());
        assert_eq!(r["error"]["code"], "VALIDATION_ERROR", "{bad}");
    }
    // 边界值可以
    list(&mut h, json!({"within_days":366,"limit":100}));
    // 审计摘要
    let activity = h.desk("desktop_activity", json!({"limit":200}));
    let summaries: Vec<&str> = activity["data"]["activity"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["tool"] == "daygrid_list_reminders" && a["outcome"] == "ok")
        .map(|a| a["summary"].as_str().unwrap())
        .collect();
    assert_eq!(summaries[0], "查询提醒，2 条");
    assert!(summaries.contains(&"查询提醒，3 条"));
}

#[test]
fn replayed_pre_upgrade_receipts_carry_remind_at_null() {
    let mut h = H::new();
    let conn = h.connect("write");
    let args = h.create_args(json!({}));
    let r = h.as_conn(&conn, "daygrid_create_item", args.clone());
    assert_eq!(r["ok"], true, "{r}");
    // 模拟升级前写下的回执：事项里没有 remind_at
    let changed = h
        .db()
        .execute(
            "UPDATE receipts SET result=json_remove(result,'$.data.item.remind_at') WHERE key=?1",
            [args["idempotency_key"].as_str().unwrap()],
        )
        .unwrap();
    assert_eq!(changed, 1);
    let replay = h.as_conn(&conn, "daygrid_create_item", args);
    assert_eq!(replay["replayed"], true, "{replay}");
    let item = replay["data"]["item"].as_object().unwrap();
    assert!(item.contains_key("remind_at"));
    assert!(item["remind_at"].is_null());
    assert_eq!(item["id"], r["data"]["item"]["id"]);
    // 带提醒的回执原样重放，不被覆盖
    let args = h.create_args(json!({"remind_at":"2030-10-20T09:00"}));
    h.as_conn(&conn, "daygrid_create_item", args.clone());
    let replay = h.as_conn(&conn, "daygrid_create_item", args);
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        replay["data"]["item"]["remind_at"],
        "2030-10-20T09:00:00+08:00"
    );
}

#[test]
fn approved_pre_upgrade_proposals_carry_remind_at_null() {
    let mut h = H::new();
    h.desk(
        "desktop_set_settings",
        json!({"approval_mode":"all_writes"}),
    );
    let conn = h.connect("write");
    let args = h.create_args(json!({}));
    let r = h.as_conn(&conn, "daygrid_create_item", args);
    let pid = r["data"]["proposal"]["id"].as_str().unwrap().to_string();
    let d = h.desk(
        "desktop_decide_proposal",
        json!({"proposal_id":pid,"decision":"approve"}),
    );
    assert_eq!(d["data"]["proposal"]["state"], "approved", "{d}");
    // 模拟升级前的确认结果：事项里没有 remind_at
    h.db()
        .execute(
            "UPDATE proposals SET outcome=json_remove(outcome,'$.data.item.remind_at') WHERE id=?1",
            [&pid],
        )
        .unwrap();
    let stored: String = h
        .db()
        .query_row("SELECT outcome FROM proposals WHERE id=?1", [&pid], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(!stored.contains("remind_at"));
    let got = h.as_conn(&conn, "daygrid_get_proposal", json!({"proposal_id":pid}));
    let item = got["data"]["proposal"]["result"]["item"]
        .as_object()
        .unwrap();
    assert!(item.contains_key("remind_at"), "{got}");
    assert!(item["remind_at"].is_null());
    let listed = h.desk("desktop_proposals", json!({}));
    let p = listed["data"]["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == pid.as_str())
        .unwrap()
        .clone();
    assert!(p["result"]["item"]["remind_at"].is_null());
    assert!(
        p["result"]["item"]
            .as_object()
            .unwrap()
            .contains_key("remind_at")
    );
}
