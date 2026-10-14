//! 休息日 / 工作日标记（调休）：读写、清除、校验与 v2 → v3 迁移。
use daygrid::store::Store;
use serde_json::{Value, json};

fn call(store: &mut Store, actor: &str, method: &str, args: Value) -> Value {
    store.call(actor, "secret", method, args)
}

fn marks(store: &mut Store, from: &str, to: &str) -> Value {
    let r = call(
        store,
        "desktop",
        "desktop_list_day_marks",
        json!({"from_date":from,"to_date":to}),
    );
    assert_eq!(r["ok"], true, "{r}");
    r["data"]["marks"].clone()
}

#[test]
fn marks_can_be_set_overwritten_and_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("calendar.db"), "secret").unwrap();
    assert_eq!(marks(&mut store, "2026-10-01", "2026-10-31"), json!([]));

    // 国庆后的周六补班；周二调为休息。
    for (date, kind) in [("2026-10-10", "work"), ("2026-10-13", "rest")] {
        let r = call(
            &mut store,
            "desktop",
            "desktop_set_day_mark",
            json!({"date":date,"kind":kind}),
        );
        assert_eq!(r["ok"], true, "{r}");
    }
    assert_eq!(
        marks(&mut store, "2026-10-01", "2026-10-31"),
        json!([
            {"date":"2026-10-10","kind":"work"},
            {"date":"2026-10-13","kind":"rest"}
        ])
    );
    // 范围之外的日子不返回。
    assert_eq!(
        marks(&mut store, "2026-10-11", "2026-10-31"),
        json!([{"date":"2026-10-13","kind":"rest"}])
    );

    // 覆盖同一天只保留一条。
    call(
        &mut store,
        "desktop",
        "desktop_set_day_mark",
        json!({"date":"2026-10-13","kind":"work"}),
    );
    assert_eq!(
        marks(&mut store, "2026-10-13", "2026-10-13"),
        json!([{"date":"2026-10-13","kind":"work"}])
    );

    // default 清除标记，重复清除不报错。
    for _ in 0..2 {
        let r = call(
            &mut store,
            "desktop",
            "desktop_set_day_mark",
            json!({"date":"2026-10-13","kind":"default"}),
        );
        assert_eq!(r["ok"], true, "{r}");
    }
    assert_eq!(
        marks(&mut store, "2026-10-01", "2026-10-31"),
        json!([{"date":"2026-10-10","kind":"work"}])
    );
}

#[test]
fn invalid_input_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("calendar.db"), "secret").unwrap();
    for args in [
        json!({"date":"2026-10-13","kind":"holiday"}),
        json!({"date":"2026-13-01","kind":"rest"}),
        json!({"date":"2026-10-13"}),
        json!({"date":"2026-10-13","kind":"rest","extra":1}),
        json!({"date":"1899-12-31","kind":"rest"}),
    ] {
        let r = call(&mut store, "desktop", "desktop_set_day_mark", args.clone());
        assert_eq!(r["error"]["code"], "VALIDATION_ERROR", "{args}");
    }
    let r = call(
        &mut store,
        "desktop",
        "desktop_list_day_marks",
        json!({"from_date":"2026-01-01","to_date":"2027-12-31"}),
    );
    assert_eq!(r["error"]["code"], "VALIDATION_ERROR", "范围过大应被拒绝");
    assert_eq!(marks(&mut store, "2026-10-01", "2026-10-31"), json!([]));
}

#[test]
fn only_the_desktop_can_touch_marks() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("calendar.db"), "secret").unwrap();
    let r = call(
        &mut store,
        "not-the-desktop",
        "desktop_set_day_mark",
        json!({"date":"2026-10-13","kind":"rest"}),
    );
    assert_eq!(r["ok"], false);
    assert_eq!(r["error"]["code"], "PERMISSION_DENIED");
}

#[test]
fn v2_database_migrates_and_keeps_existing_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calendar.db");
    drop(Store::open(&path, "secret").unwrap());
    {
        // 退回成 0.3.0 的样子：v2，没有 day_marks。
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("DROP TABLE day_marks; PRAGMA user_version=2;")
            .unwrap();
    }
    let mut store = Store::open(&path, "secret").unwrap();
    assert!(dir.path().join("calendar.db.pre-v4.bak").exists());
    assert_eq!(
        call(&mut store, "desktop", "desktop_diagnostics", json!({}))["data"]["schema_version"],
        4
    );
    call(
        &mut store,
        "desktop",
        "desktop_set_day_mark",
        json!({"date":"2026-10-10","kind":"work"}),
    );
    drop(store);
    // 再次打开不会清空标记。
    let mut store = Store::open(&path, "secret").unwrap();
    assert_eq!(
        marks(&mut store, "2026-10-01", "2026-10-31"),
        json!([{"date":"2026-10-10","kind":"work"}])
    );
}
