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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("calendar.db");
        drop(Store::open(&path, SECRET).unwrap());
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute(
                "UPDATE metadata SET value='Asia/Shanghai' WHERE key='timezone'",
                [],
            )
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
