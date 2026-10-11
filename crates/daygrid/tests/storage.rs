use daygrid::{domain, store::Store};
use serde_json::{Value, json};
use uuid::Uuid;

fn call(store: &mut Store, method: &str, args: Value) -> Value {
    store.call("desktop", "test-secret", method, args)
}
fn context(store: &mut Store) -> String {
    call(store, "daygrid_get_context", json!({}))["data"]["calendar_generation"]
        .as_str()
        .unwrap()
        .into()
}
fn create(generation: &str) -> Value {
    json!({"calendar_generation":generation,"idempotency_key":Uuid::new_v4().to_string(),"date":"2026-10-15","title":"提交材料","notes":"附件\n检查","color":"amber"})
}

#[test]
fn durable_writes_retries_conflicts_and_noops() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("calendar.db");
    let mut store = Store::open(&db, "test-secret").unwrap();
    let generation = context(&mut store);
    let request = create(&generation);
    let first = call(&mut store, "daygrid_create_item", request.clone());
    assert_eq!(first["ok"], true);
    let id = first["data"]["item"]["id"].clone();
    drop(store);
    let mut store = Store::open(&db, "test-secret").unwrap();
    let replay = call(&mut store, "daygrid_create_item", request.clone());
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["data"]["item"]["id"], id);
    let mut changed = request.clone();
    changed["title"] = json!("different");
    assert_eq!(
        call(&mut store, "daygrid_create_item", changed)["error"]["code"],
        "IDEMPOTENCY_CONFLICT"
    );
    let update = json!({"calendar_generation":generation,"idempotency_key":Uuid::new_v4().to_string(),"item_id":id,"expected_revision":1,"patch":{"date":"2026-10-16","color":"violet"}});
    let result = call(&mut store, "daygrid_update_item", update.clone());
    assert_eq!(result["data"]["item"]["revision"], 2);
    assert_eq!(result["data"]["item"]["notes"], "附件\n检查");
    let mut stale = update.clone();
    stale["idempotency_key"] = json!(Uuid::new_v4().to_string());
    assert_eq!(
        call(&mut store, "daygrid_update_item", stale)["error"]["code"],
        "REVISION_CONFLICT"
    );
    assert_eq!(
        call(&mut store, "daygrid_update_item", update)["replayed"],
        true
    );
    let no_op = json!({"calendar_generation":generation,"idempotency_key":Uuid::new_v4().to_string(),"item_id":id,"expected_revision":2,"status":"planned"});
    let result = call(&mut store, "daygrid_set_item_status", no_op);
    assert_eq!(result["data"]["changed"], false);
    assert_eq!(result["data"]["item"]["revision"], 2);
    assert_eq!(result["change_sequence"], 2);
    assert_eq!(
        call(&mut store, "desktop_history", json!({}))["data"]["history"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn validation_unicode_and_generation() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("db"), "test-secret").unwrap();
    let generation = context(&mut store);
    for (field, value) in [
        ("date", json!("2026-02-30")),
        ("title", json!("  ")),
        ("title", json!("😀".repeat(201))),
        ("notes", json!("字".repeat(10001))),
        ("color", json!("red")),
        ("notes", Value::Null),
        ("unknown", json!("x")),
    ] {
        let mut r = create(&generation);
        r[field] = value;
        assert_eq!(
            call(&mut store, "daygrid_create_item", r)["error"]["code"],
            "VALIDATION_ERROR",
            "{field}"
        )
    }
    let mut r = create(&generation);
    r["title"] = json!(format!(" {} ", "😀".repeat(200)));
    assert_eq!(call(&mut store, "daygrid_create_item", r)["ok"], true);
    let mut r = create(&generation);
    r["calendar_generation"] = json!(Uuid::new_v4().to_string());
    assert_eq!(
        call(&mut store, "daygrid_create_item", r)["error"]["code"],
        "CALENDAR_REPLACED"
    );
    assert!(domain::date("2024-02-29").is_ok());
    assert!(domain::date("2026-2-03").is_err());
}
#[test]
fn soft_delete_restore_paging_and_live_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("db"), "test-secret").unwrap();
    let generation = context(&mut store);
    let first = call(&mut store, "daygrid_create_item", create(&generation));
    let id = first["data"]["item"]["id"].clone();
    call(&mut store, "daygrid_create_item", create(&generation));
    let list = json!({"from_date":"2026-10-01","to_date":"2026-10-31","limit":1});
    let page = call(&mut store, "daygrid_list_items", list.clone());
    assert_eq!(page["data"]["items"].as_array().unwrap().len(), 1);
    let lifecycle = json!({"calendar_generation":generation,"idempotency_key":Uuid::new_v4().to_string(),"item_id":id,"expected_revision":1});
    let deleted = call(&mut store, "desktop_delete", lifecycle);
    assert_eq!(deleted["data"]["item"]["revision"], 2);
    assert_eq!(
        call(&mut store, "daygrid_get_item", json!({"item_id":id}))["error"]["code"],
        "NOT_FOUND"
    );
    let mut paged = list;
    paged["cursor"] = page["data"]["next_cursor"].clone();
    assert_eq!(
        call(&mut store, "daygrid_list_items", paged)["error"]["code"],
        "CURSOR_STALE"
    );
    let restore = json!({"calendar_generation":generation,"idempotency_key":Uuid::new_v4().to_string(),"item_id":id,"expected_revision":2});
    assert_eq!(
        call(&mut store, "desktop_restore", restore)["data"]["item"]["revision"],
        3
    );
    let connection = call(
        &mut store,
        "desktop_create_connection",
        json!({"label":"test","permission":"read"}),
    );
    let c = connection["data"]["id"].as_str().unwrap();
    let token = connection["data"]["credential"].as_str().unwrap();
    assert_eq!(
        store.call(c, token, "daygrid_get_context", json!({}))["ok"],
        true
    );
    assert_eq!(
        store.call(c, token, "daygrid_create_item", create(&generation))["error"]["code"],
        "PERMISSION_DENIED"
    );
    call(
        &mut store,
        "desktop_set_connection",
        json!({"id":c,"state":"paused"}),
    );
    assert_eq!(
        store.call(c, token, "daygrid_get_context", json!({}))["error"]["code"],
        "PERMISSION_DENIED"
    );
    call(
        &mut store,
        "desktop_set_connection",
        json!({"id":c,"state":"revoked"}),
    );
    assert_eq!(
        call(
            &mut store,
            "desktop_set_connection",
            json!({"id":c,"state":"active"})
        )["error"]["code"],
        "NOT_FOUND"
    );
}
