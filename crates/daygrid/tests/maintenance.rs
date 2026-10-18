//! 迁移、备份、诊断、契约漂移与性能基线。
use daygrid::{
    store::{SCHEMA_VERSION, Store},
    tools,
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, time::Instant};

fn call(store: &mut Store, method: &str, args: Value) -> Value {
    store.call("desktop", "secret", method, args)
}

/// 迁移前备份的文件名（`calendar.db.pre-v{SCHEMA_VERSION}.bak`）。
fn backup_name() -> String {
    format!("calendar.db.pre-v{SCHEMA_VERSION}.bak")
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
        SCHEMA_VERSION
    );
    assert!(
        dir.path().join(backup_name()).exists(),
        "迁移前应保留原样拷贝"
    );
    drop(store);
    // 再次打开是无操作，不会重复迁移或报错。
    assert!(Store::open(&path, "secret").is_ok());
}

/// 建一个 v2（0.3.x）库，并写入一条迁移前标记。返回的连接保持打开，
/// 让标记留在 WAL 里，用来确认备份不依赖 checkpoint。
fn v2_database(path: &std::path::Path) -> rusqlite::Connection {
    drop(Store::open(path, "secret").unwrap());
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch(
        "DROP TABLE day_marks; PRAGMA user_version=2;
         INSERT INTO metadata VALUES('marker','before-v3');",
    )
    .unwrap();
    db
}

fn schema_version(path: &std::path::Path) -> i64 {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

/// 只读打开备份，确认完整、停在 v2 且带着迁移前数据。
fn assert_valid_v2_backup(path: &std::path::Path) {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let ok: String = db
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    let v: i64 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 2, "备份应是迁移前的版本");
    let marker: String = db
        .query_row("SELECT value FROM metadata WHERE key='marker'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(marker, "before-v3");
    let marks: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='day_marks'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(marks, 0, "备份里不应有 v3 的表");
}

fn leftover_temp_files(dir: &std::path::Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(".tmp-"))
        .collect()
}

#[test]
fn pre_migration_backup_is_verified_and_holds_pre_migration_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calendar.db");
    let holder = v2_database(&path);
    let store = Store::open(&path, "secret").unwrap();
    drop(store);
    drop(holder);
    assert_eq!(schema_version(&path), SCHEMA_VERSION);
    let backup = dir.path().join(backup_name());
    assert_valid_v2_backup(&backup);
    assert_eq!(
        fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(leftover_temp_files(dir.path()).is_empty());
}

#[test]
fn invalid_existing_pre_migration_backup_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calendar.db");
    let backup = dir.path().join(backup_name());
    let holder = v2_database(&path);
    fs::write(&backup, b"not a database").unwrap();
    drop(Store::open(&path, "secret").unwrap());
    drop(holder);
    assert_eq!(schema_version(&path), SCHEMA_VERSION);
    assert_valid_v2_backup(&backup);
    assert!(leftover_temp_files(dir.path()).is_empty());
}

#[test]
fn failed_pre_migration_backup_aborts_the_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calendar.db");
    let backup = dir.path().join(backup_name());
    drop(v2_database(&path));
    // 备份路径被一个非空目录占着：快照无法改名到位。
    fs::create_dir(&backup).unwrap();
    fs::write(backup.join("keep"), b"x").unwrap();
    assert_eq!(
        Store::open(&path, "secret").err().unwrap().code,
        "STORAGE_ERROR"
    );
    assert_eq!(schema_version(&path), 2, "备份失败时不应升级");
    let marks: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='day_marks'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(marks, 0);
    assert!(leftover_temp_files(dir.path()).is_empty());

    // 障碍移除后再次打开即可正常备份并升级。
    fs::remove_dir_all(&backup).unwrap();
    drop(Store::open(&path, "secret").unwrap());
    assert_eq!(schema_version(&path), SCHEMA_VERSION);
    assert_valid_v2_backup(&backup);
}

/// 把一个新建的库退回到 `version`（3 = 0.4.0，没有协同表；4 = 开发期协同版）：去掉 v5 的 reminders 表，
/// 写入 `bodies` 里的事项（旧格式没有 remind_at）。
fn older_database(path: &std::path::Path, version: i64, bodies: &[Value]) {
    drop(Store::open(path, "secret").unwrap());
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch("DROP TABLE reminders;").unwrap();
    if version < 4 {
        db.execute_batch(
            "DROP TABLE sync_outbox; DROP TABLE sync_items; DROP TABLE sync_conflicts;",
        )
        .unwrap();
    }
    db.execute_batch(&format!("PRAGMA user_version={version};"))
        .unwrap();
    for (i, body) in bodies.iter().enumerate() {
        db.execute(
            "INSERT INTO items VALUES(?1,'2026-10-15',?2,1,'2026-10-01T00:00:00+00:00',NULL,?3)",
            rusqlite::params![
                format!("aaaaaaaa-2222-4333-8444-{i:012}"),
                body["status"].as_str().unwrap_or("planned"),
                body.to_string()
            ],
        )
        .unwrap();
    }
}

fn old_item(i: usize) -> Value {
    json!({"id":format!("aaaaaaaa-2222-4333-8444-{i:012}"),"date":"2026-10-15","title":"旧数据","notes":"","color":"blue","status":"planned","revision":1,"created_at":"2026-10-01T00:00:00+00:00","updated_at":"2026-10-01T00:00:00+00:00"})
}

fn has_table(path: &std::path::Path, name: &str) -> bool {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    db.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        == 1
}

#[test]
fn v3_and_v4_databases_migrate_to_v5_with_a_verified_backup() {
    assert_eq!(SCHEMA_VERSION, 5);
    for version in [3, 4] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("calendar.db");
        older_database(&path, version, &[old_item(0)]);
        let mut store = Store::open(&path, "secret").unwrap();
        assert_eq!(schema_version(&path), 5, "v{version}");
        assert!(has_table(&path, "reminders"), "v{version}");
        let list = call(
            &mut store,
            "daygrid_list_items",
            json!({"from_date":"2026-10-01","to_date":"2026-10-31"}),
        );
        let item = &list["data"]["items"][0];
        assert_eq!(item["title"], "旧数据");
        assert!(item.as_object().unwrap().contains_key("remind_at"));
        assert!(item["remind_at"].is_null(), "旧事项都没有提醒");
        assert_eq!(store.next_reminder_due().unwrap(), None);
        drop(store);
        // 备份完整、停在迁移前的版本，没有 v5 的表。
        let backup = dir.path().join("calendar.db.pre-v5.bak");
        assert_eq!(backup_name(), "calendar.db.pre-v5.bak");
        let db = rusqlite::Connection::open_with_flags(
            &backup,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let ok: String = db
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ok, "ok");
        let v: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, version);
        drop(db);
        assert!(!has_table(&backup, "reminders"));
        assert_eq!(
            fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(leftover_temp_files(dir.path()).is_empty());
    }
}

#[test]
fn migration_backfills_reminder_rows_for_items_that_already_have_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calendar.db");
    let mut planned = old_item(0);
    planned["remind_at"] = json!("2030-10-20T09:00:00+08:00");
    let mut done = old_item(1);
    done["remind_at"] = json!("2030-10-21T09:00:00+08:00");
    done["status"] = json!("done");
    older_database(&path, 4, &[planned, done, old_item(2)]);
    let store = Store::open(&path, "secret").unwrap();
    let due = chrono::DateTime::parse_from_rfc3339("2030-10-20T09:00:00+08:00")
        .unwrap()
        .timestamp_millis();
    assert_eq!(store.next_reminder_due().unwrap(), Some(due));
    let rows: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM reminders", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1, "已完成的事项不建提醒行");
    let hint: Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join("reminders.next")).unwrap())
            .unwrap();
    assert_eq!(hint, json!({"v":1,"next_due_ms":due}));
}

#[test]
fn failed_backfill_keeps_the_old_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calendar.db");
    // body 里有 remind_at，但不是合法的事项：backfill 读不出来。
    older_database(
        &path,
        4,
        &[json!({"remind_at":"2030-10-20T09:00:00+08:00"})],
    );
    assert_eq!(
        Store::open(&path, "secret").err().unwrap().code,
        "STORAGE_ERROR"
    );
    assert_eq!(schema_version(&path), 4, "backfill 失败时版本号不前进");
    assert!(!has_table(&path, "reminders"), "整个迁移事务回滚");
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
    assert_eq!(generated["tools"].as_array().unwrap().len(), 10);
}

#[test]
fn item_schema_is_in_sync_with_the_code() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/item.schema.json");
    let mut generated = json!({
        "$schema":"https://json-schema.org/draft/2020-12/schema",
        "$id":"urn:daygrid:item:2"
    });
    for (k, v) in tools::item_schema().as_object().unwrap() {
        generated[k] = v.clone();
    }
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
        "contracts/item.schema.json 与代码不一致。运行 `UPDATE_CONTRACTS=1 cargo test -p daygrid --test maintenance` 重新生成。"
    );
    let required: Vec<&str> = generated["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for field in ["remind_at", "visibility"] {
        assert!(required.contains(&field), "缺少 {field}");
        assert!(generated["properties"][field].is_object(), "缺少 {field}");
    }
}

/// 性能基线：1 万条事项。阈值刻意放宽（CI 机器抖动），目的是拦住数量级退化，
/// 精确数字见 docs/verification.md。
#[test]
fn performance_baseline_with_10k_items() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calendar.db");
    drop(Store::open(&path, "secret").unwrap());
    let now = chrono::Utc::now();
    {
        let mut db = rusqlite::Connection::open(&path).unwrap();
        let tx = db.transaction().unwrap();
        let tz: chrono_tz::Tz = tx
            .query_row("SELECT value FROM metadata WHERE key='timezone'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
            .parse()
            .unwrap();
        // 1 千个提醒：在未完成的事项（i % 5 != 0，共 8000 条）里按出现顺序每第 8 条选一条，
        // 时间均匀分布在未来 30 天内。这里绕过 Store 直接造数据：Store::open 已建好 reminders 表，
        // backfill 只在 version < 5 时运行，不会补这些行，所以提醒行也在这里一起写。
        let mut planned = 0u32;
        let mut reminders = 0i64;
        for i in 0..10_000u32 {
            let day = 1 + (i % 28);
            let month = 1 + (i / 28) % 12;
            let date = format!("2026-{month:02}-{day:02}");
            let color = ["blue", "sage", "amber", "rose", "violet", "gray"][(i % 6) as usize];
            let mut body = json!({"id":format!("00000000-0000-4000-8000-{i:012}"),"date":date,"title":format!("事项 {i} 写周报 review"),"notes":"备注内容 ".repeat(5),"color":color,"status":if i % 5 == 0 {"done"} else {"planned"},"revision":1,"created_at":"2026-01-01T00:00:00+00:00","updated_at":"2026-01-01T00:00:00+00:00"});
            let mut due = None;
            if i % 5 != 0 {
                planned += 1;
                if planned.is_multiple_of(8) {
                    let at = now
                        + chrono::Duration::minutes(1)
                        + chrono::Duration::milliseconds(reminders * 30 * 86_400_000 / 1000);
                    let r =
                        daygrid::domain::parse_remind_at(&at.to_rfc3339(), tz, now, false).unwrap();
                    body["remind_at"] = json!(r.text);
                    due = Some(r.epoch_ms);
                    reminders += 1;
                }
            }
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
            if let Some(due) = due {
                tx.execute(
                    "INSERT INTO reminders(item_id,due_ms) VALUES(?1,?2)",
                    rusqlite::params![format!("00000000-0000-4000-8000-{i:012}"), due],
                )
                .unwrap();
            }
        }
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM reminders", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1000);
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
    // 下一个提醒：部分索引上的 MIN。不走 time()（它要求返回信封），单独计时。
    assert!(store.next_reminder_due().unwrap().is_some());
    let runs = 20;
    let start = Instant::now();
    for _ in 0..runs {
        assert!(store.next_reminder_due().unwrap().is_some());
    }
    let next_due = start.elapsed().as_secs_f64() * 1000.0 / runs as f64;
    println!("PERF next_reminder_due: {next_due:.3} ms");
    // 到期处理：time() 的预热会把提醒标记掉，所以每轮先用另一个连接复位（不计时）。
    // 31 天后晚于所有 1000 个提醒，一次正好取满 TAKE_LIMIT = 200 条，大部分超过 24 小时计入 skipped。
    let side = rusqlite::Connection::open(&path).unwrap();
    let rounds = 5;
    let mut total = 0.0;
    for _ in 0..rounds {
        side.execute("UPDATE reminders SET fired_at=NULL", [])
            .unwrap();
        let start = Instant::now();
        let v = store
            .take_due_reminders_at(chrono::Utc::now() + chrono::Duration::days(31))
            .unwrap();
        total += start.elapsed().as_secs_f64() * 1000.0;
        let taken = v["data"]["reminders"].as_array().unwrap().len() as u64
            + v["data"]["skipped"].as_u64().unwrap();
        assert_eq!(taken, 200, "{}", v["data"]["skipped"]);
    }
    let take = total / rounds as f64;
    println!("PERF take_due_reminders_at（200 条）: {take:.2} ms");
    assert!(pulse < 10.0, "轮询脉冲过慢：{pulse} ms");
    assert!(next_due < 1.0, "查询下一个提醒过慢：{next_due} ms");
    assert!(take < 10.0, "到期处理过慢：{take} ms");
}
