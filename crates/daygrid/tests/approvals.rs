//! 审批流、助手活动审计、全局搜索与月概览。
use daygrid::store::Store;
use serde_json::{Value, json};
use uuid::Uuid;

struct Harness {
    store: Store,
    dir: tempfile::TempDir,
    generation: String,
}
impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("calendar.db"), "secret").unwrap();
        let generation = store.call("desktop", "secret", "daygrid_get_context", json!({}))["data"]
            ["calendar_generation"]
            .as_str()
            .unwrap()
            .to_string();
        Self {
            store,
            dir,
            generation,
        }
    }
    fn desktop(&mut self, method: &str, args: Value) -> Value {
        self.store.call("desktop", "secret", method, args)
    }
    /// 建立连接，返回 (id, token)。
    fn connect(&mut self, permission: &str) -> (String, String) {
        let v = self.desktop(
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
    fn create_args(&self, title: &str) -> Value {
        json!({"calendar_generation":self.generation,"idempotency_key":Uuid::new_v4().to_string(),"date":"2026-10-15","title":title,"notes":"备注","color":"rose"})
    }
    fn new_item(&mut self, title: &str) -> Value {
        let args = self.create_args(title);
        self.desktop("daygrid_create_item", args)["data"]["item"].clone()
    }
    fn delete_args(&self, item: &Value, reason: Option<&str>) -> Value {
        let mut a = json!({"calendar_generation":self.generation,"idempotency_key":Uuid::new_v4().to_string(),"item_id":item["id"],"expected_revision":item["revision"]});
        if let Some(r) = reason {
            a["reason"] = json!(r);
        }
        a
    }
}

#[test]
fn delete_needs_approval_then_executes_once() {
    let mut h = Harness::new();
    let item = h.new_item("周报");
    let conn = h.connect("write");
    let args = h.delete_args(&item, Some("重复了"));
    let first = h.as_conn(&conn, "daygrid_delete_item", args.clone());
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["data"]["pending_approval"], true);
    let pid = first["data"]["proposal"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    // 未批准前事项仍然存在。
    assert_eq!(
        h.desktop("daygrid_get_item", json!({"item_id":item["id"]}))["ok"],
        true
    );
    // 重试同一请求得到同一个提案，而不是第二个。
    let again = h.as_conn(&conn, "daygrid_delete_item", args.clone());
    assert_eq!(again["data"]["proposal"]["id"], pid.as_str());
    assert_eq!(again["replayed"], true);
    // 同一幂等键换内容 → 冲突。
    let mut other = args.clone();
    other["reason"] = json!("不同的理由");
    assert_eq!(
        h.as_conn(&conn, "daygrid_delete_item", other)["error"]["code"],
        "IDEMPOTENCY_CONFLICT"
    );
    let listed = h.desktop("desktop_proposals", json!({}));
    let p = &listed["data"]["proposals"][0];
    assert_eq!(p["state"], "pending");
    assert_eq!(p["connection_label"], "测试助手");
    assert_eq!(p["preview"]["kind"], "delete");
    assert_eq!(p["preview"]["reason"], "重复了");
    let pulse = h.desktop("desktop_pulse", json!({}));
    assert_eq!(pulse["data"]["pending_proposals"], 1);
    // 只有创建者和桌面能查看这个提案。
    let stranger = h.connect("write");
    assert_eq!(
        h.as_conn(
            &stranger,
            "daygrid_get_proposal",
            json!({"proposal_id":pid})
        )["error"]["code"],
        "NOT_FOUND"
    );
    // 批准 → 执行，历史里的操作者是那个助手连接。
    let decided = h.desktop(
        "desktop_decide_proposal",
        json!({"proposal_id":pid,"decision":"approve"}),
    );
    assert_eq!(
        decided["data"]["proposal"]["state"], "approved",
        "{decided}"
    );
    assert_eq!(
        h.desktop("daygrid_get_item", json!({"item_id":item["id"]}))["error"]["code"],
        "NOT_FOUND"
    );
    let history = h.desktop("desktop_history", json!({}));
    assert_eq!(history["data"]["history"][0]["action"], "delete");
    assert_eq!(history["data"]["history"][0]["actor"], conn.0.as_str());
    // 助手查询得到最终结果；原请求再次重试返回回执，而不是再删一次。
    let state = h.as_conn(&conn, "daygrid_get_proposal", json!({"proposal_id":pid}));
    assert_eq!(state["data"]["proposal"]["state"], "approved");
    assert_eq!(state["data"]["proposal"]["result"]["item"]["revision"], 2);
    let replay = h.as_conn(&conn, "daygrid_delete_item", args);
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["data"]["item"]["id"], item["id"]);
    // 已处理的提案不能再决定。
    assert_eq!(
        h.desktop(
            "desktop_decide_proposal",
            json!({"proposal_id":pid,"decision":"reject"})
        )["error"]["code"],
        "PROPOSAL_NOT_PENDING"
    );
}

#[test]
fn reject_stale_revision_and_permissions() {
    let mut h = Harness::new();
    let item = h.new_item("体检");
    let writer = h.connect("write");
    let reader = h.connect("read");
    // 只读连接不能提出删除。
    let args = h.delete_args(&item, None);
    assert_eq!(
        h.as_conn(&reader, "daygrid_delete_item", args)["error"]["code"],
        "PERMISSION_DENIED"
    );
    // 拒绝：事项保持不变，助手看到 rejected。
    let args = h.delete_args(&item, None);
    let pid = h.as_conn(&writer, "daygrid_delete_item", args)["data"]["proposal"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    h.desktop(
        "desktop_decide_proposal",
        json!({"proposal_id":pid,"decision":"reject"}),
    );
    assert_eq!(
        h.as_conn(&writer, "daygrid_get_proposal", json!({"proposal_id":pid}))["data"]["proposal"]
            ["state"],
        "rejected"
    );
    assert_eq!(
        h.desktop("daygrid_get_item", json!({"item_id":item["id"]}))["ok"],
        true
    );
    // 提交时版本就不对 → 立即冲突，且带最新内容。
    let mut stale = h.delete_args(&item, None);
    stale["expected_revision"] = json!(99);
    let err = h.as_conn(&writer, "daygrid_delete_item", stale);
    assert_eq!(err["error"]["code"], "REVISION_CONFLICT");
    assert_eq!(err["error"]["details"]["current_item"]["title"], "体检");
    assert!(err["error"]["hint"].is_string());
    // 等待期间事项被改动 → 批准时失败而不是误删，提案标记 failed。
    let args = h.delete_args(&item, None);
    let pid = h.as_conn(&writer, "daygrid_delete_item", args)["data"]["proposal"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let edit = json!({"calendar_generation":h.generation,"idempotency_key":Uuid::new_v4().to_string(),"item_id":item["id"],"expected_revision":1,"patch":{"title":"体检（改期）"}});
    assert_eq!(h.desktop("daygrid_update_item", edit)["ok"], true);
    let decided = h.desktop(
        "desktop_decide_proposal",
        json!({"proposal_id":pid,"decision":"approve"}),
    );
    assert_eq!(decided["data"]["proposal"]["state"], "failed");
    assert_eq!(
        decided["data"]["proposal"]["error"]["code"],
        "REVISION_CONFLICT"
    );
    assert_eq!(
        h.desktop("daygrid_get_item", json!({"item_id":item["id"]}))["ok"],
        true
    );
}

#[test]
fn pausing_a_connection_voids_its_pending_requests_and_expiry_works() {
    let mut h = Harness::new();
    let item = h.new_item("续费");
    let conn = h.connect("write");
    let args = h.delete_args(&item, None);
    let pid = h.as_conn(&conn, "daygrid_delete_item", args)["data"]["proposal"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    h.desktop(
        "desktop_set_connection",
        json!({"id":conn.0,"state":"paused"}),
    );
    assert_eq!(
        h.desktop(
            "desktop_decide_proposal",
            json!({"proposal_id":pid,"decision":"approve"})
        )["error"]["code"],
        "PROPOSAL_NOT_PENDING"
    );
    assert_eq!(
        h.desktop("daygrid_get_item", json!({"item_id":item["id"]}))["ok"],
        true
    );
    // 过期：把有效期改到过去（通过另一个 SQLite 连接，WAL 下可并发）。
    h.desktop(
        "desktop_set_connection",
        json!({"id":conn.0,"state":"active"}),
    );
    let args = h.delete_args(&item, None);
    let pid = h.as_conn(&conn, "daygrid_delete_item", args)["data"]["proposal"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let raw = rusqlite::Connection::open(h.dir.path().join("calendar.db")).unwrap();
    raw.execute(
        "UPDATE proposals SET expires_at='2000-01-01T00:00:00.000Z' WHERE id=?1",
        [&pid],
    )
    .unwrap();
    assert_eq!(
        h.as_conn(&conn, "daygrid_get_proposal", json!({"proposal_id":pid}))["data"]["proposal"]["state"],
        "expired"
    );
    assert_eq!(
        h.desktop(
            "desktop_decide_proposal",
            json!({"proposal_id":pid,"decision":"approve"})
        )["error"]["code"],
        "PROPOSAL_EXPIRED"
    );
}

#[test]
fn strict_mode_routes_every_assistant_write_through_approval() {
    let mut h = Harness::new();
    let conn = h.connect("write");
    // 默认：创建直接生效。
    let direct = h.as_conn(&conn, "daygrid_create_item", h.create_args("直接创建"));
    assert_eq!(direct["data"]["item"]["title"], "直接创建");
    assert_eq!(
        h.desktop("desktop_set_settings", json!({"approval_mode":"bogus"}))["error"]["code"],
        "VALIDATION_ERROR"
    );
    h.desktop(
        "desktop_set_settings",
        json!({"approval_mode":"all_writes"}),
    );
    assert_eq!(
        h.desktop("daygrid_get_context", json!({}))["data"]["approval_mode"],
        "all_writes"
    );
    let args = h.create_args("需确认的创建");
    let pending = h.as_conn(&conn, "daygrid_create_item", args.clone());
    assert_eq!(pending["data"]["pending_approval"], true);
    assert_eq!(
        pending["data"]["proposal"]["summary"],
        "创建「需确认的创建」· 2026-10-15"
    );
    let list = json!({"from_date":"2026-10-01","to_date":"2026-10-31"});
    assert_eq!(
        h.desktop("daygrid_list_items", list.clone())["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "未批准前不应出现"
    );
    let pid = pending["data"]["proposal"]["id"].as_str().unwrap();
    h.desktop(
        "desktop_decide_proposal",
        json!({"proposal_id":pid,"decision":"approve"}),
    );
    assert_eq!(
        h.desktop("daygrid_list_items", list)["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // 桌面自己的写入永远不需要确认。
    assert_eq!(
        h.desktop("daygrid_create_item", h.create_args("桌面创建"))["ok"],
        true
    );
    // 批准后重放原请求 → 回执，不会重复创建。
    assert_eq!(
        h.as_conn(&conn, "daygrid_create_item", args)["replayed"],
        true
    );
}

#[test]
fn activity_is_audited_for_known_connections_only() {
    let mut h = Harness::new();
    let conn = h.connect("write");
    h.as_conn(&conn, "daygrid_get_context", json!({}));
    h.as_conn(&conn, "daygrid_create_item", h.create_args("写代码"));
    h.as_conn(
        &conn,
        "daygrid_list_items",
        json!({"from_date":"2026-10-01","to_date":"2026-10-31"}),
    );
    h.as_conn(&conn, "daygrid_create_item", json!({"bad":"args"}));
    // 未知调用方与桌面自己的调用不入账。
    h.store.call("ghost", "x", "daygrid_get_context", json!({}));
    h.desktop("daygrid_get_context", json!({}));
    // 令牌错误的已知连接：记一条拒绝，但不更新最近活动时间。
    let before =
        h.desktop("desktop_connections", json!({}))["data"]["connections"][0]["last_seen_at"]
            .clone();
    h.store
        .call(&conn.0, "wrong", "daygrid_get_context", json!({}));
    let activity = h.desktop("desktop_activity", json!({}));
    let rows = activity["data"]["activity"].as_array().unwrap();
    assert_eq!(rows.len(), 5, "{activity}");
    assert_eq!(rows[0]["error_code"], "PERMISSION_DENIED");
    assert_eq!(rows[1]["outcome"], "error");
    assert_eq!(rows[2]["summary"], "查询 2026-10-01 ~ 2026-10-31，1 条");
    assert_eq!(rows[3]["summary"], "创建「写代码」· 2026-10-15");
    assert_eq!(rows[3]["connection_label"], "测试助手");
    assert_eq!(rows[4]["tool"], "daygrid_get_context");
    let conns = h.desktop("desktop_connections", json!({}));
    assert_eq!(conns["data"]["connections"][0]["calls"], 5);
    assert_eq!(conns["data"]["connections"][0]["last_seen_at"], before);
    assert!(before.is_string());
}

#[test]
fn search_overview_and_filters() {
    let mut h = Harness::new();
    for (date, title, color) in [
        ("2026-10-01", "国庆假期", "amber"),
        ("2026-10-15", "提交材料", "amber"),
        ("2026-10-15", "给家人打电话", "rose"),
        ("2026-12-24", "平安夜聚餐", "sage"),
        ("2025-03-02", "旧的材料归档", "gray"),
    ] {
        let mut a = h.create_args(title);
        a["date"] = json!(date);
        a["color"] = json!(color);
        assert_eq!(h.desktop("daygrid_create_item", a)["ok"], true);
    }
    // 全局搜索跨月份，按日期倒序；Unicode 大小写不敏感。
    let found = h.desktop("desktop_search", json!({"query":"材料"}));
    let r = found["data"]["results"].as_array().unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(r[0]["item"]["title"], "提交材料");
    assert_eq!(r[1]["item"]["title"], "旧的材料归档");
    assert_eq!(
        h.desktop("desktop_search", json!({"query":"  "}))["error"]["code"],
        "VALIDATION_ERROR"
    );
    let limited = h.desktop("desktop_search", json!({"query":"材料","limit":1}));
    assert_eq!(limited["data"]["has_more"], true);
    // 月概览。
    let o = h.desktop(
        "daygrid_month_overview",
        json!({"from_date":"2026-10-01","to_date":"2026-10-31"}),
    );
    assert_eq!(o["data"]["total_items"], 3);
    let days = o["data"]["days"].as_array().unwrap();
    assert_eq!(days.len(), 2);
    assert_eq!(days[1]["date"], "2026-10-15");
    assert_eq!(days[1]["planned"], 2);
    assert_eq!(days[1]["colors"], json!(["amber", "rose"]));
    assert_eq!(
        h.desktop(
            "daygrid_month_overview",
            json!({"from_date":"2026-01-01","to_date":"2027-12-31"})
        )["error"]["code"],
        "VALIDATION_ERROR"
    );
    // SQL 下推的颜色过滤与分页：翻页不丢不重。
    let colored = h.desktop(
        "daygrid_list_items",
        json!({"from_date":"2026-10-01","to_date":"2026-10-31","color":"amber"}),
    );
    assert_eq!(colored["data"]["items"].as_array().unwrap().len(), 2);
    let mut titles = vec![];
    let mut cursor = Value::Null;
    loop {
        let mut q = json!({"from_date":"2026-10-01","to_date":"2026-12-31","limit":1});
        if !cursor.is_null() {
            q["cursor"] = cursor.clone();
        }
        let page = h.desktop("daygrid_list_items", q);
        for i in page["data"]["items"].as_array().unwrap() {
            titles.push(i["title"].as_str().unwrap().to_string());
        }
        cursor = page["data"]["next_cursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert_eq!(
        titles,
        ["国庆假期", "提交材料", "给家人打电话", "平安夜聚餐"]
    );
    // 带文本查询的分页同样正确。
    let q = h.desktop(
        "daygrid_list_items",
        json!({"from_date":"2026-10-01","to_date":"2026-12-31","query":"家","limit":1}),
    );
    assert_eq!(q["data"]["items"][0]["title"], "给家人打电话");
    assert!(q["data"]["next_cursor"].is_null());
}

#[test]
fn idempotency_digest_ignores_key_order() {
    let mut h = Harness::new();
    let args = h.create_args("顺序无关");
    let first = h.desktop("daygrid_create_item", args.clone());
    // 同一个请求，键顺序反过来。
    let mut reversed = serde_json::Map::new();
    for (k, v) in args.as_object().unwrap().iter().rev() {
        reversed.insert(k.clone(), v.clone());
    }
    let second = h.desktop("daygrid_create_item", Value::Object(reversed));
    assert_eq!(second["replayed"], true, "{second}");
    assert_eq!(second["data"]["item"]["id"], first["data"]["item"]["id"]);
}
