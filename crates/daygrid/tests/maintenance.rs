//! 迁移、备份、诊断、契约漂移与性能基线。
use daygrid::{store::Store, tools};
use serde_json::{Value, json};
use std::{fs, time::Instant};

fn call(store: &mut Store, method: &str, args: Value) -> Value {
    store.call("desktop", "secret", method, args)
}

#[test]
fn migrates_v1_database_and_keeps_a_copy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calendar.db");
    {
        // 手工构造 0.1.0 的 schema（user_version=1，没有 activity/proposals）。
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
             CREATE TABLE items(id TEXT PRIMARY KEY,date TEXT NOT NULL,status TEXT NOT NULL,revision INTEGER NOT NULL,created_at TEXT NOT NULL,deleted_at TEXT,body TEXT NOT NULL);
             CREATE INDEX items_date ON items(date,status,created_at,id);
             CREATE TABLE history(sequence INTEGER PRIMARY KEY AUTOINCREMENT,operation_id TEXT NOT NULL,item_id TEXT NOT NULL,action TEXT NOT NULL,before_json TEXT,after_json TEXT,actor TEXT NOT NULL,occurred_at TEXT NOT NULL);
             CREATE TABLE receipts(actor TEXT NOT NULL,key TEXT NOT NULL,digest TEXT NOT NULL,result TEXT NOT NULL,PRIMARY KEY(actor,key));
             CREATE TABLE connections(id TEXT PRIMARY KEY,label TEXT NOT NULL,verifier TEXT NOT NULL,permission TEXT NOT NULL CHECK(permission IN ('read','write')),state TEXT NOT NULL CHECK(state IN ('active','paused','revoked')));
             INSERT INTO connections VALUES('11111111-2222-4333-8444-555555555555','旧连接','x','read','active');
             PRAGMA user_version=1;",
        )
        .unwrap();
        let body = json!({"id":"aaaaaaaa-2222-4333-8444-555555555555","date":"2026-10-15","title":"旧数据","notes":"","color":"blue","status":"planned","revision":1,"created_at":"2026-10-01T00:00:00+00:00","updated_at":"2026-10-01T00:00:00+00:00"});
        db.execute(
            "INSERT INTO items VALUES(?1,'2026-10-15','planned',1,'2026-10-01T00:00:00+00:00',NULL,?2)",
            rusqlite::params!["aaaaaaaa-2222-4333-8444-555555555555", body.to_string()],
        )
        .unwrap();
    }
    let mut store = Store::open(&path, "secret").unwrap();
    let list = call(
        &mut store,
        "daygrid_list_items",
        json!({"from_date":"2026-10-01","to_date":"2026-10-31"}),
    );
    assert_eq!(list["data"]["items"][0]["title"], "旧数据");
    let conns = call(&mut store, "desktop_connections", json!({}));
    assert_eq!(conns["data"]["connections"][0]["label"], "旧连接");
    assert!(conns["data"]["connections"][0]["last_seen_at"].is_null());
    assert_eq!(
        call(&mut store, "desktop_diagnostics", json!({}))["data"]["schema_version"],
        2
    );
    assert!(
        dir.path().join("calendar.db.pre-v2.bak").exists(),
        "迁移前应保留原样拷贝"
    );
    drop(store);
    // 再次打开是无操作，不会重复迁移或报错。
    assert!(Store::open(&path, "secret").is_ok());
}

#[test]
fn refuses_databases_from_the_future() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calendar.db");
    drop(Store::open(&path, "secret").unwrap());
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA user_version=99;")
        .unwrap();
    assert_eq!(
        Store::open(&path, "secret").err().unwrap().code,
        "VERSION_UNSUPPORTED"
    );
}

#[test]
fn backup_is_consistent_pruned_and_diagnostics_report_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("calendar.db"), "secret").unwrap();
    let generation =
        call(&mut store, "daygrid_get_context", json!({}))["data"]["calendar_generation"].clone();
    call(
        &mut store,
        "daygrid_create_item",
        json!({"calendar_generation":generation,"idempotency_key":uuid::Uuid::new_v4().to_string(),"date":"2026-10-15","title":"要备份的事项"}),
    );
    let made = call(&mut store, "desktop_backup", json!({}));
    assert_eq!(made["ok"], true, "{made}");
    let name = made["data"]["name"].as_str().unwrap();
    let file = dir.path().join("backups").join(name);
    // 备份是一个独立可用的数据库，包含刚才的事项。
    let copy = rusqlite::Connection::open(&file).unwrap();
    let n: i64 = copy
        .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    // 24 小时内已有备份时，自动备份不再重复生成。
    assert_eq!(store.auto_backup().unwrap(), None);
    // 超出保留份数的旧备份被清理：伪造 14 份更早的文件，再备份一次。
    for i in 0..14 {
        fs::write(
            dir.path()
                .join("backups")
                .join(format!("calendar-2000010{}-0000{:02}.db", i % 10, i)),
            b"x",
        )
        .unwrap();
    }
    fs::remove_file(&file).unwrap();
    call(&mut store, "desktop_backup", json!({}));
    let count = fs::read_dir(dir.path().join("backups")).unwrap().count();
    assert_eq!(count, 10);
    let diag = call(&mut store, "desktop_diagnostics", json!({}));
    assert_eq!(diag["data"]["items_active"], 1);
    assert_eq!(diag["data"]["backups"].as_array().unwrap().len(), 10);
    assert!(diag["data"]["database_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn contracts_are_in_sync_with_the_code() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/tools.json");
    let generated = tools::catalog();
    if std::env::var_os("UPDATE_CONTRACTS").is_some() {
        fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&generated).unwrap()),
        )
        .unwrap();
    }
    let on_disk: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        on_disk, generated,
        "contracts/tools.json 与代码不一致。运行 `UPDATE_CONTRACTS=1 cargo test -p daygrid --test maintenance` 重新生成。"
    );
    // 每个工具都有标题、描述、输入输出 schema 与注解。
    for t in generated["tools"].as_array().unwrap() {
        for field in [
            "name",
            "title",
            "description",
            "inputSchema",
            "outputSchema",
            "annotations",
        ] {
            assert!(!t[field].is_null(), "{} 缺少 {field}", t["name"]);
        }
    }
    assert_eq!(generated["tools"].as_array().unwrap().len(), 9);
}

/// 性能基线：1 万条事项。阈值刻意放宽（CI 机器抖动），目的是拦住数量级退化，
/// 精确数字见 docs/verification.md。
#[test]
fn performance_baseline_with_10k_items() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calendar.db");
    drop(Store::open(&path, "secret").unwrap());
    {
        let mut db = rusqlite::Connection::open(&path).unwrap();
        let tx = db.transaction().unwrap();
        for i in 0..10_000u32 {
            let day = 1 + (i % 28);
            let month = 1 + (i / 28) % 12;
            let date = format!("2026-{month:02}-{day:02}");
            let color = ["blue", "sage", "amber", "rose", "violet", "gray"][(i % 6) as usize];
            let body = json!({"id":format!("00000000-0000-4000-8000-{i:012}"),"date":date,"title":format!("事项 {i} 写周报 review"),"notes":"备注内容 ".repeat(5),"color":color,"status":if i % 5 == 0 {"done"} else {"planned"},"revision":1,"created_at":"2026-01-01T00:00:00+00:00","updated_at":"2026-01-01T00:00:00+00:00"});
            tx.execute(
                "INSERT INTO items VALUES(?1,?2,?3,1,'2026-01-01T00:00:00+00:00',NULL,?4)",
                rusqlite::params![
                    format!("00000000-0000-4000-8000-{i:012}"),
                    date,
                    body["status"].as_str().unwrap(),
                    body.to_string()
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let mut store = Store::open(&path, "secret").unwrap();
    let time = |label: &str, f: &mut dyn FnMut() -> Value| -> f64 {
        let _warm = f();
        let runs = 20;
        let start = Instant::now();
        for _ in 0..runs {
            assert_eq!(f()["ok"], true, "{label}");
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        println!("PERF {label}: {ms:.2} ms");
        ms
    };
    let month = time("list 一个月视图范围（约 840 条）", &mut || {
        store.call(
            "desktop",
            "secret",
            "daygrid_list_items",
            json!({"from_date":"2026-06-01","to_date":"2026-07-12","limit":100}),
        )
    });
    let overview = time("month_overview 全年", &mut || {
        store.call(
            "desktop",
            "secret",
            "daygrid_month_overview",
            json!({"from_date":"2026-01-01","to_date":"2026-12-31"}),
        )
    });
    let search = time("全局搜索（命中少）", &mut || {
        store.call(
            "desktop",
            "secret",
            "desktop_search",
            json!({"query":"事项 9999"}),
        )
    });
    let search_miss = time("全局搜索（全表扫描未命中）", &mut || {
        store.call(
            "desktop",
            "secret",
            "desktop_search",
            json!({"query":"绝对不存在的词","limit":5}),
        )
    });
    let pulse = time("pulse（界面轮询）", &mut || {
        store.call("desktop", "secret", "desktop_pulse", json!({}))
    });
    assert!(month < 150.0, "月视图查询过慢：{month} ms");
    assert!(overview < 150.0, "概览过慢：{overview} ms");
    assert!(search < 400.0, "搜索过慢：{search} ms");
    assert!(search_miss < 400.0, "全表扫描搜索过慢：{search_miss} ms");
    assert!(pulse < 10.0, "轮询脉冲过慢：{pulse} ms");
}
