//! 端到端协同测试：真实的 HTTP 服务端 + 多个真实的本地 `Store` + 真实的客户端同步引擎。
//!
//! 每个"客户端"就是一台电脑：自己的 SQLite、自己的 outbox。测试里手动驱动 `sync_once`
//! 来精确控制"谁先同步"，模拟离线 / 并发 / 响应丢失等场景。
use daygrid::{
    store::{Store, SyncConfig},
    sync::{
        PushRequest,
        client::{self, NetError},
        runtime::Runtime,
    },
};
use daygrid_server::{
    app::{self, AppState},
    db::ServerDb,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;

const SECRET: &str = "test-secret";

struct Server {
    url: String,
    ws: String,
    owner_token: String,
    _dir: tempfile::TempDir,
}

fn start_server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut db = ServerDb::open(&dir.path().join("server.db")).unwrap();
    let (ws, _, owner_token) = db.init_workspace("团队", "Alice").unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            app::serve(listener, AppState::new(db)).await.unwrap();
        });
    });
    let addr = rx.recv().unwrap();
    Server {
        url: format!("http://{addr}"),
        ws,
        owner_token,
        _dir: dir,
    }
}

/// 一台"电脑"。
struct Pc {
    store: Arc<Mutex<Store>>,
    rt: Arc<Runtime>,
    cfg: SyncConfig,
    token: String,
    dir: tempfile::TempDir,
}

type Snapshot = BTreeMap<String, (String, String, String, String, String)>;

impl Pc {
    fn open_store() -> (Arc<Mutex<Store>>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("calendar.db"), SECRET).unwrap();
        (Arc::new(Mutex::new(store)), dir)
    }

    /// 用邀请码加入（role = editor / viewer）。
    fn join(server: &Server, name: &str, role: &str) -> Pc {
        let owner_cfg = SyncConfig {
            url: server.url.clone(),
            workspace_id: server.ws.clone(),
            workspace_name: "团队".into(),
            user_id: String::new(),
            user_name: String::new(),
            role: "owner".into(),
            client_id: "owner".into(),
        };
        let invite = client::create_invite(&owner_cfg, &server.owner_token, role, 24, 10).unwrap();
        let joined = client::join(&server.url, invite["code"].as_str().unwrap(), name).unwrap();
        Self::link(server, joined.token, joined.user.id, name, &joined.role)
    }

    /// 用已有令牌连接（owner）。
    fn owner(server: &Server) -> Pc {
        let me = client::me(&server.url, &server.owner_token).unwrap();
        Self::link(
            server,
            server.owner_token.clone(),
            me.user.id,
            "Alice",
            "owner",
        )
    }

    fn link(server: &Server, token: String, user_id: String, name: &str, role: &str) -> Pc {
        let (store, dir) = Self::open_store();
        let rt = store.lock().unwrap().runtime();
        let cfg = SyncConfig {
            url: server.url.clone(),
            workspace_id: server.ws.clone(),
            workspace_name: "团队".into(),
            user_id,
            user_name: name.into(),
            role: role.into(),
            client_id: Uuid::new_v4().simple().to_string(),
        };
        store
            .lock()
            .unwrap()
            .sync_link(&cfg, &token, false)
            .unwrap();
        Pc {
            store,
            rt,
            cfg,
            token,
            dir,
        }
    }

    fn call(&self, method: &str, args: Value) -> Value {
        self.store
            .lock()
            .unwrap()
            .call("desktop", SECRET, method, args)
    }
    fn generation(&self) -> String {
        self.call("daygrid_get_context", json!({}))["data"]["calendar_generation"]
            .as_str()
            .unwrap()
            .into()
    }
    fn create(&self, date: &str, title: &str, notes: &str) -> String {
        let r = self.call("daygrid_create_item", json!({
            "calendar_generation":self.generation(),"idempotency_key":Uuid::new_v4().to_string(),
            "date":date,"title":title,"notes":notes,"visibility":"shared"
        }));
        assert_eq!(r["ok"], true, "{r}");
        r["data"]["item"]["id"].as_str().unwrap().into()
    }
    fn get(&self, id: &str) -> Option<Value> {
        let r = self.call("daygrid_get_item", json!({"item_id":id}));
        (r["ok"] == true).then(|| r["data"]["item"].clone())
    }
    fn update(&self, id: &str, patch: Value) -> Value {
        let item = self.get(id).expect("item exists");
        self.call("daygrid_update_item", json!({
            "calendar_generation":self.generation(),"idempotency_key":Uuid::new_v4().to_string(),
            "item_id":id,"expected_revision":item["revision"],"patch":patch
        }))
    }
    fn set_visibility(&self, id: &str, visibility: &str) -> Value {
        let item = self.get(id).unwrap();
        self.call("desktop_set_visibility", json!({
            "calendar_generation":self.generation(),"idempotency_key":Uuid::new_v4().to_string(),
            "item_id":id,"expected_revision":item["revision"],"visibility":visibility
        }))
    }
    fn set_status(&self, id: &str, status: &str) -> Value {
        let item = self.get(id).unwrap();
        self.call("daygrid_set_item_status", json!({
            "calendar_generation":self.generation(),"idempotency_key":Uuid::new_v4().to_string(),
            "item_id":id,"expected_revision":item["revision"],"status":status
        }))
    }
    fn delete(&self, id: &str) -> Value {
        let item = self.get(id).unwrap();
        self.call("desktop_delete", json!({
            "calendar_generation":self.generation(),"idempotency_key":Uuid::new_v4().to_string(),
            "item_id":id,"expected_revision":item["revision"]
        }))
    }
    fn sync(&self) {
        client::sync_once(&self.store, &self.rt, &self.cfg, &self.token).unwrap();
    }
    fn try_sync(&self) -> Result<(), NetError> {
        client::sync_once(&self.store, &self.rt, &self.cfg, &self.token)
    }
    fn conflicts(&self) -> Vec<Value> {
        self.call("desktop_sync_conflicts", json!({}))["data"]["conflicts"]
            .as_array()
            .unwrap()
            .clone()
    }
    fn pending(&self) -> i64 {
        self.store.lock().unwrap().sync_pending().unwrap()
    }
    /// 本机所有存活事项的内容快照。
    fn snapshot(&self) -> Snapshot {
        let r = self.call(
            "daygrid_list_items",
            json!({"from_date":"2026-01-01","to_date":"2026-12-31","limit":100}),
        );
        r["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                let s = |k: &str| i[k].as_str().unwrap().to_string();
                (
                    s("id"),
                    (s("date"), s("title"), s("notes"), s("color"), s("status")),
                )
            })
            .collect()
    }
}

/// 服务端权威状态的快照（只含未删除的事项），用来与客户端副本对比。
fn server_snapshot(server: &Server, pc: &Pc) -> Snapshot {
    let mut since = 0;
    let mut out = BTreeMap::new();
    loop {
        let resp = client::pull(&pc.cfg, &server.owner_token, since, 0).unwrap();
        for i in resp.items {
            if i.deleted {
                out.remove(&i.id);
            } else {
                out.insert(
                    i.id.clone(),
                    (
                        i.fields.date,
                        i.fields.title,
                        i.fields.notes,
                        i.fields.color,
                        i.fields.status,
                    ),
                );
            }
        }
        if !resp.has_more {
            return out;
        }
        since = resp.next_since;
    }
}

fn settle(pcs: &[&Pc]) {
    // 两轮：第一轮各自推送并拉取，第二轮拉到别人在这一轮的结果。
    for _ in 0..3 {
        for pc in pcs {
            pc.sync();
        }
    }
}

// ============================ 场景 ============================

#[test]
fn a1_new_users_share_existing_items_and_uploads_local_data_on_link() {
    let server = start_server();
    // Bob 在连接之前已经有本地事项：连接时上传
    let (store, _dir) = Pc::open_store();
    let pc = |s: &Arc<Mutex<Store>>| {
        s.lock().unwrap().call("desktop", SECRET, "daygrid_get_context", json!({}))["data"]["calendar_generation"].as_str().unwrap().to_string()
    };
    let g = pc(&store);
    let r = store.lock().unwrap().call("desktop", SECRET, "daygrid_create_item", json!({"calendar_generation":g,"idempotency_key":Uuid::new_v4().to_string(),"date":"2026-10-20","title":"连接前就有的事项","notes":""}));
    let existing_id = r["data"]["item"]["id"].as_str().unwrap().to_string();
    let owner = Pc::owner(&server);
    let invite = client::create_invite(&owner.cfg, &server.owner_token, "editor", 24, 1).unwrap();
    let joined = client::join(&server.url, invite["code"].as_str().unwrap(), "Bob").unwrap();
    let cfg = SyncConfig {
        url: server.url.clone(),
        workspace_id: server.ws.clone(),
        workspace_name: "团队".into(),
        user_id: joined.user.id,
        user_name: "Bob".into(),
        role: "editor".into(),
        client_id: Uuid::new_v4().simple().to_string(),
    };
    let uploaded = store
        .lock()
        .unwrap()
        .sync_link(&cfg, &joined.token, true)
        .unwrap();
    assert_eq!(uploaded, 1);
    let rt = store.lock().unwrap().runtime();
    client::sync_once(&store, &rt, &cfg, &joined.token).unwrap();
    owner.sync();
    assert_eq!(
        owner.get(&existing_id).unwrap()["title"],
        "连接前就有的事项",
        "上传的事项出现在 owner 那里"
    );
    // 之后 owner 的新事项也同步到 Bob
    let id2 = owner.create("2026-10-21", "Alice 的事项", "");
    owner.sync();
    client::sync_once(&store, &rt, &cfg, &joined.token).unwrap();
    let r = store.lock().unwrap().call(
        "desktop",
        SECRET,
        "daygrid_get_item",
        json!({"item_id":id2}),
    );
    assert_eq!(r["data"]["item"]["title"], "Alice 的事项");
}

#[test]
fn a2_a3_concurrent_edits_to_different_items_and_different_fields_all_survive() {
    let server = start_server();
    let (alice, bob) = (Pc::owner(&server), Pc::join(&server, "Bob", "editor"));
    let shared = alice.create("2026-10-15", "周会", "");
    settle(&[&alice, &bob]);
    // 同时（都还没同步）：各加一个新事项；同时改同一事项的不同字段
    let a_new = alice.create("2026-10-16", "Alice 新增", "");
    let b_new = bob.create("2026-10-16", "Bob 新增", "");
    alice.update(&shared, json!({"color":"rose"}));
    bob.update(&shared, json!({"title":"周例会"}));
    settle(&[&alice, &bob]);
    for pc in [&alice, &bob] {
        assert!(
            pc.get(&a_new).is_some() && pc.get(&b_new).is_some(),
            "不同事项互不影响"
        );
        let it = pc.get(&shared).unwrap();
        assert_eq!(
            (it["title"].as_str(), it["color"].as_str()),
            (Some("周例会"), Some("rose"))
        );
        assert!(pc.conflicts().is_empty(), "不同字段不算冲突");
    }
    assert_eq!(alice.snapshot(), bob.snapshot());
    assert_eq!(alice.snapshot(), server_snapshot(&server, &alice));
}

#[test]
fn a4_same_field_conflict_converges_notifies_loser_and_restore_works() {
    let server = start_server();
    let (alice, bob) = (Pc::owner(&server), Pc::join(&server, "Bob", "editor"));
    let id = alice.create("2026-10-15", "周会", "");
    settle(&[&alice, &bob]);
    alice.update(&id, json!({"title":"Alice 的标题"}));
    std::thread::sleep(Duration::from_millis(5)); // 保证 Bob 的修改发生得更晚
    bob.update(&id, json!({"title":"Bob 的标题"}));
    // 谁先同步都一样：更晚修改的赢
    bob.sync();
    alice.sync();
    settle(&[&alice, &bob]);
    assert_eq!(alice.get(&id).unwrap()["title"], "Bob 的标题");
    assert_eq!(bob.get(&id).unwrap()["title"], "Bob 的标题");
    // Alice 收到冲突通知，Bob 没有
    let c = alice.conflicts();
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(
        (
            c[0]["field"].as_str(),
            c[0]["lost_value"].as_str(),
            c[0]["kept_by"].as_str()
        ),
        (Some("title"), Some("Alice 的标题"), Some("Bob"))
    );
    assert_eq!(c[0]["item_title"], "Bob 的标题");
    assert!(bob.conflicts().is_empty());
    // Alice 一键恢复自己的版本：作为一次新的修改参与合并，最终两边收敛到 Alice 的标题
    let r = alice.call(
        "desktop_sync_resolve_conflict",
        json!({"id":c[0]["id"],"action":"restore"}),
    );
    assert_eq!(r["ok"], true, "{r}");
    settle(&[&alice, &bob]);
    assert_eq!(bob.get(&id).unwrap()["title"], "Alice 的标题");
    assert!(alice.conflicts().is_empty(), "已处理的冲突不再显示");
    assert_eq!(alice.snapshot(), bob.snapshot());
}

#[test]
fn a5_notes_merge_on_different_lines_and_conflict_on_same_line() {
    let server = start_server();
    let (alice, bob) = (Pc::owner(&server), Pc::join(&server, "Bob", "editor"));
    let id = alice.create("2026-10-15", "评审会", "议程一\n议程二\n议程三\n");
    settle(&[&alice, &bob]);
    alice.update(
        &id,
        json!({"notes":"议程一（Alice 补充）\n议程二\n议程三\n"}),
    );
    bob.update(
        &id,
        json!({"notes":"议程一\n议程二\n议程三\n议程四（Bob 新增）\n"}),
    );
    settle(&[&alice, &bob]);
    let expect = "议程一（Alice 补充）\n议程二\n议程三\n议程四（Bob 新增）\n";
    assert_eq!(alice.get(&id).unwrap()["notes"], expect);
    assert_eq!(bob.get(&id).unwrap()["notes"], expect);
    assert!(alice.conflicts().is_empty() && bob.conflicts().is_empty());
    // 同一行：LWW + 冲突记录
    alice.update(
        &id,
        json!({"notes":"议程一（Alice 再改）\n议程二\n议程三\n议程四（Bob 新增）\n"}),
    );
    std::thread::sleep(Duration::from_millis(5));
    bob.update(
        &id,
        json!({"notes":"议程一（Bob 再改）\n议程二\n议程三\n议程四（Bob 新增）\n"}),
    );
    settle(&[&alice, &bob]);
    assert_eq!(
        alice.get(&id).unwrap()["notes"],
        bob.get(&id).unwrap()["notes"]
    );
    assert!(
        alice.get(&id).unwrap()["notes"]
            .as_str()
            .unwrap()
            .contains("Bob 再改")
    );
    assert_eq!(alice.conflicts().len(), 1);
}

#[test]
fn a6_offline_edit_made_earlier_loses_to_a_later_online_edit() {
    let server = start_server();
    let (alice, bob) = (Pc::owner(&server), Pc::join(&server, "Bob", "editor"));
    let id = alice.create("2026-10-15", "周会", "");
    settle(&[&alice, &bob]);
    // Alice "离线"改（更早），之后很久才联网；Bob 在线改（更晚）并已同步
    alice.update(&id, json!({"title":"Alice 离线时改的"}));
    std::thread::sleep(Duration::from_millis(5));
    bob.update(&id, json!({"title":"Bob 在线改的"}));
    bob.sync();
    assert!(alice.pending() > 0, "Alice 的修改还在 outbox 里");
    alice.sync(); // Alice 终于联网：她的旧修改晚到，但仍然输
    settle(&[&alice, &bob]);
    assert_eq!(alice.get(&id).unwrap()["title"], "Bob 在线改的");
    assert_eq!(alice.pending(), 0);
    assert_eq!(alice.snapshot(), bob.snapshot());
}

#[test]
fn a7_delete_beats_concurrent_edit_and_the_edit_can_be_recovered() {
    let server = start_server();
    let (alice, bob) = (Pc::owner(&server), Pc::join(&server, "Bob", "editor"));
    let id = alice.create("2026-10-15", "周会", "");
    settle(&[&alice, &bob]);
    alice.delete(&id);
    bob.update(&id, json!({"title":"Bob 想改的标题"}));
    settle(&[&alice, &bob]);
    assert!(
        alice.get(&id).is_none() && bob.get(&id).is_none(),
        "两边都收敛为已删除"
    );
    let c = bob.conflicts();
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(c[0]["reason"], "item_deleted");
    assert_eq!(c[0]["lost_value"], "Bob 想改的标题");
    // Bob 选择恢复：事项复活并带上他的修改
    let r = bob.call(
        "desktop_sync_resolve_conflict",
        json!({"id":c[0]["id"],"action":"restore"}),
    );
    assert_eq!(r["ok"], true, "{r}");
    settle(&[&alice, &bob]);
    assert_eq!(alice.get(&id).unwrap()["title"], "Bob 想改的标题");
    assert_eq!(alice.snapshot(), bob.snapshot());
}

#[test]
fn a8_a9_replayed_batches_and_lost_responses_do_not_duplicate_or_diverge() {
    let server = start_server();
    let (alice, bob) = (Pc::owner(&server), Pc::join(&server, "Bob", "editor"));
    let id = alice.create("2026-10-15", "周会", "");
    alice.update(&id, json!({"notes":"一\n二\n"}));
    // 手动推送同一批两次：模拟"服务器已应用，但响应丢了，客户端重试"
    let batch = alice
        .store
        .lock()
        .unwrap()
        .sync_prepare_push(200)
        .unwrap()
        .unwrap();
    let req = PushRequest {
        client_id: batch.client_id.clone(),
        ops: batch.ops.clone(),
    };
    let lost = client::push(&alice.cfg, &alice.token, &req).unwrap();
    let retry = client::push(&alice.cfg, &alice.token, &req).unwrap();
    assert_eq!(lost.results, retry.results, "重试得到完全相同的结果");
    alice
        .store
        .lock()
        .unwrap()
        .sync_apply_push(&batch, &retry)
        .unwrap();
    assert_eq!(alice.pending(), 0);
    settle(&[&alice, &bob]);
    assert_eq!(bob.snapshot().len(), 1, "没有重复事项");
    assert_eq!(bob.get(&id).unwrap()["notes"], "一\n二\n");
    assert_eq!(alice.snapshot(), server_snapshot(&server, &alice));
    // 响应丢失后，客户端又产生了新修改，再整体重试：仍然收敛
    alice.update(&id, json!({"title":"v2"}));
    let batch = alice
        .store
        .lock()
        .unwrap()
        .sync_prepare_push(200)
        .unwrap()
        .unwrap();
    let _lost = client::push(
        &alice.cfg,
        &alice.token,
        &PushRequest {
            client_id: batch.client_id.clone(),
            ops: batch.ops.clone(),
        },
    )
    .unwrap();
    alice.update(&id, json!({"notes":"一\n二\n三\n"}));
    settle(&[&alice, &bob]);
    assert_eq!(bob.get(&id).unwrap()["title"], "v2");
    assert_eq!(bob.get(&id).unwrap()["notes"], "一\n二\n三\n");
    assert!(
        alice.conflicts().is_empty(),
        "同一个人的连续修改不应和自己冲突"
    );
    assert_eq!(alice.snapshot(), bob.snapshot());
}

#[test]
fn a10_permissions_viewer_cannot_write_and_removed_member_loses_access() {
    let server = start_server();
    let alice = Pc::owner(&server);
    let viewer = Pc::join(&server, "Vera", "viewer");
    let bob = Pc::join(&server, "Bob", "editor");
    let id = alice.create("2026-10-15", "周会", "");
    settle(&[&alice, &viewer, &bob]);
    assert!(viewer.get(&id).is_some(), "viewer 可以读");
    // viewer 不能写团队共享的事项；自己私有的事项照常可写
    let r = viewer.call("daygrid_create_item", json!({"calendar_generation":viewer.generation(),"idempotency_key":Uuid::new_v4().to_string(),"date":"2026-10-16","title":"x","visibility":"shared"}));
    assert_eq!(r["error"]["code"], "PERMISSION_DENIED");
    let r = viewer.update(&id, json!({"title":"改一下"}));
    assert_eq!(r["error"]["code"], "PERMISSION_DENIED");
    let r = viewer.call("daygrid_create_item", json!({"calendar_generation":viewer.generation(),"idempotency_key":Uuid::new_v4().to_string(),"date":"2026-10-16","title":"我的私事"}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(viewer.pending(), 0, "私有事项不进上传队列");
    // 绕过本地检查直接推送也被服务端拒绝
    let req = PushRequest {
        client_id: viewer.cfg.client_id.clone(),
        ops: vec![],
    };
    match client::push(&viewer.cfg, &viewer.token, &req) {
        Err(NetError::Server {
            status: 403, code, ..
        }) => assert_eq!(code, "READ_ONLY"),
        other => panic!("{other:?}"),
    }
    // editor 不能生成邀请码
    assert!(client::create_invite(&bob.cfg, &bob.token, "editor", 1, 1).is_err());
    // 无效令牌
    assert!(matches!(
        client::pull(&bob.cfg, "dgt_bogus", 0, 0),
        Err(NetError::Server { status: 401, .. })
    ));
    // owner 移除 Bob（通过 HTTP DELETE）：之后他的同步被拒
    let resp = daygrid_server_remove(&server, "Bob");
    assert_eq!(resp, 200);
    assert!(matches!(
        bob.try_sync(),
        Err(NetError::Server {
            status: 401 | 403,
            ..
        })
    ));
}

fn daygrid_server_remove(server: &Server, target: &str) -> u16 {
    let agent = client::agent(Some(Duration::from_secs(5)));
    let resp = agent
        .delete(&format!(
            "{}/v1/workspaces/{}/members/{}",
            server.url, server.ws, target
        ))
        .header("Authorization", format!("Bearer {}", server.owner_token))
        .call()
        .unwrap();
    resp.status().as_u16()
}

#[test]
fn a13_token_never_lands_in_sqlite_or_backups() {
    let server = start_server();
    let bob = Pc::join(&server, "Bob", "editor");
    bob.create("2026-10-15", "x", "");
    bob.sync();
    bob.call("desktop_backup", json!({}));
    let dir = bob.dir.path().to_path_buf();
    let mut checked = 0;
    for entry in walk(&dir) {
        if entry.file_name().unwrap() == "sync.token" {
            let mode = std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(&entry).unwrap().permissions(),
            );
            assert_eq!(mode & 0o777, 0o600, "令牌文件权限必须是 0600");
            continue;
        }
        let bytes = std::fs::read(&entry).unwrap();
        assert!(
            !bytes
                .windows(bob.token.len())
                .any(|w| w == bob.token.as_bytes()),
            "{} 含有令牌",
            entry.display()
        );
        checked += 1;
    }
    assert!(checked >= 2, "至少检查了数据库与备份");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p))
        } else {
            out.push(p)
        }
    }
    out
}

#[test]
fn a14_assistant_writes_sync_too_and_assistant_deletes_still_need_local_approval() {
    let server = start_server();
    let (alice, bob) = (Pc::owner(&server), Pc::join(&server, "Bob", "editor"));
    // Alice 的 AI 助手（MCP 连接）创建事项
    let conn = alice.call(
        "desktop_create_connection",
        json!({"label":"我的助手","permission":"write"}),
    );
    assert_eq!(conn["ok"], true, "{conn}");
    let (cid, ctoken) = (
        conn["data"]["id"].as_str().unwrap().to_string(),
        conn["data"]["credential"].as_str().unwrap().to_string(),
    );
    let g = alice.generation();
    let r = alice.store.lock().unwrap().call(&cid, &ctoken, "daygrid_create_item", json!({"calendar_generation":g,"idempotency_key":Uuid::new_v4().to_string(),"date":"2026-10-18","title":"助手排的会"}));
    assert_eq!(r["ok"], true, "{r}");
    let id = r["data"]["item"]["id"].as_str().unwrap().to_string();
    settle(&[&alice, &bob]);
    assert_eq!(
        r["data"]["item"]["visibility"], "private",
        "助手创建的事项默认只有自己可见"
    );
    assert!(bob.get(&id).is_none(), "私有事项不会出现在同事那里");
    // 助手不能自己决定公开；用户在界面里改成“所有人可见”之后才同步
    let r = alice.store.lock().unwrap().call(&cid, &ctoken, "daygrid_create_item", json!({"calendar_generation":g,"idempotency_key":Uuid::new_v4().to_string(),"date":"2026-10-18","title":"x","visibility":"shared"}));
    assert_eq!(r["ok"], false, "助手不能指定可见范围：{r}");
    alice.set_visibility(&id, "shared");
    settle(&[&alice, &bob]);
    assert_eq!(
        bob.get(&id).unwrap()["title"],
        "助手排的会",
        "公开之后助手的写入同样同步给同事"
    );
    // 助手删除：需要 Alice 本机批准；批准前 Bob 看不到变化
    let item = alice.get(&id).unwrap();
    let r = alice.store.lock().unwrap().call(&cid, &ctoken, "daygrid_delete_item", json!({"calendar_generation":g,"idempotency_key":Uuid::new_v4().to_string(),"item_id":id,"expected_revision":item["revision"],"reason":"测试"}));
    let proposal = r["data"]["proposal"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{r}"))
        .to_string();
    settle(&[&alice, &bob]);
    assert!(bob.get(&id).is_some(), "未批准的删除不会同步");
    let r = alice.call(
        "desktop_decide_proposal",
        json!({"proposal_id":proposal,"decision":"approve"}),
    );
    assert_eq!(r["ok"], true, "{r}");
    settle(&[&alice, &bob]);
    assert!(bob.get(&id).is_none(), "批准后删除同步给同事");
}

#[test]
fn a12_random_offline_edits_from_three_replicas_always_converge() {
    // 固定种子的伪随机：3 个副本、多轮；每轮随机选副本做随机操作（创建/改字段/改备注/完成/删除），
    // 随机选择谁同步。最后全部同步后，三个副本与服务器必须逐字段一致。多个种子，确保覆盖各种交错。
    let mut conflicts = 0;
    for seed in 1..=8_u64 {
        conflicts += random_run(seed.wrapping_mul(0x9E3779B97F4A7C15));
    }
    // 防止测试"空转"：这些随机交错必须真的制造过冲突，否则说明没覆盖到合并路径。
    assert!(conflicts > 0, "随机测试没有产生任何冲突，覆盖不足");
    eprintln!("random convergence: 8 seeds, {conflicts} conflicts resolved");
}

fn random_run(seed: u64) -> usize {
    let server = start_server();
    let pcs = [
        Pc::owner(&server),
        Pc::join(&server, "Bob", "editor"),
        Pc::join(&server, "Carol", "editor"),
    ];
    let mut rng = seed;
    let mut next = |n: usize| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng % n as u64) as usize
    };
    let mut ids: Vec<String> = vec![];
    for round in 0..120 {
        let who = &pcs[next(3)];
        match next(8) {
            0 | 1 if ids.len() < 12 => {
                let notes = (0..next(4))
                    .map(|n| format!("行{n}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                ids.push(who.create(
                    &format!("2026-10-{:02}", 10 + next(10)),
                    &format!("事项{round}"),
                    &notes,
                ));
            }
            2 if !ids.is_empty() => {
                let id = &ids[next(ids.len())];
                if who.get(id).is_some() {
                    who.update(id, json!({"title":format!("标题{round}-{}", next(100))}));
                }
            }
            3 if !ids.is_empty() => {
                let id = &ids[next(ids.len())];
                if let Some(it) = who.get(id) {
                    let mut lines: Vec<String> = it["notes"]
                        .as_str()
                        .unwrap()
                        .lines()
                        .map(String::from)
                        .collect();
                    if lines.is_empty() || next(2) == 0 {
                        lines.push(format!("追加{round}"));
                    } else {
                        let k = next(lines.len());
                        lines[k] = format!("改{round}");
                    }
                    who.update(id, json!({"notes":lines.join("\n")}));
                }
            }
            4 if !ids.is_empty() => {
                let id = &ids[next(ids.len())];
                if who.get(id).is_some() {
                    let color = ["blue", "sage", "amber", "rose", "violet", "gray"][next(6)];
                    let date = format!("2026-10-{:02}", 10 + next(10));
                    who.update(id, json!({"color":color, "date":date}));
                }
            }
            5 if !ids.is_empty() => {
                let id = &ids[next(ids.len())];
                if who.get(id).is_some() {
                    who.set_status(id, if next(2) == 0 { "done" } else { "planned" });
                }
            }
            6 if !ids.is_empty() => {
                let id = &ids[next(ids.len())];
                if who.get(id).is_some() && next(3) == 0 {
                    who.delete(id);
                }
            }
            _ => {}
        }
        // 随机让某个副本同步（有时多个、有时都不同步）
        for pc in &pcs {
            if next(3) == 0 {
                pc.sync();
            }
        }
    }
    settle(&[&pcs[0], &pcs[1], &pcs[2]]);
    for pc in &pcs {
        assert_eq!(pc.pending(), 0);
    }
    let reference = server_snapshot(&server, &pcs[0]);
    assert!(!reference.is_empty());
    for (i, pc) in pcs.iter().enumerate() {
        assert_eq!(
            pc.snapshot(),
            reference,
            "seed {seed}: 副本 {i} 与服务器不一致"
        );
    }
    pcs.iter()
        .map(|pc| {
            pc.call("desktop_sync_conflicts", json!({"state":"all"}))["data"]["conflicts"]
                .as_array()
                .unwrap()
                .len()
        })
        .sum()
}

#[test]
fn e2e_background_workers_sync_in_realtime_with_presence() {
    // 用真正的后台线程（push/pull 循环 + SSE 通知 + 在线状态上报），通过 desktop_sync_* 接口连接。
    let server = start_server();
    let alice = Pc::owner(&server);
    let invite = client::create_invite(&alice.cfg, &server.owner_token, "editor", 24, 1).unwrap();
    // Alice 用后台线程；Bob 通过 desktop_sync_connect（异步连接）
    let (bob_store, _dir) = Pc::open_store();
    client::spawn(alice.store.clone());
    client::spawn(bob_store.clone());
    alice.store.lock().unwrap().runtime().wake();
    let r = bob_store.lock().unwrap().call(
        "desktop",
        SECRET,
        "desktop_sync_connect",
        json!({"url":server.url,"invite":invite["code"],"display_name":"Bob"}),
    );
    assert_eq!(r["ok"], true, "{r}");
    let status = |s: &Arc<Mutex<Store>>| {
        s.lock()
            .unwrap()
            .call("desktop", SECRET, "desktop_sync_status", json!({}))["data"]
            .clone()
    };
    wait_for("Bob 连接成功并在线", || {
        let st = status(&bob_store);
        st["linked"] == true && st["state"] == "online"
    });
    wait_for("Alice 的 SSE 连上", || {
        status(&alice.store)["events_connected"] == true
    });
    wait_for("Bob 的 SSE 连上", || {
        status(&bob_store)["events_connected"] == true
    });
    // Alice 创建事项 → Bob 在数秒内（poke 驱动，不是 30 秒轮询）看到
    let started = Instant::now();
    let id = alice.create("2026-10-15", "实时同步", "");
    wait_for("Bob 收到事项", || {
        bob_store
            .lock()
            .unwrap()
            .call("desktop", SECRET, "daygrid_get_item", json!({"item_id":id}))["ok"]
            == true
    });
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "poke 推送应当很快：{:?}",
        started.elapsed()
    );
    // 在线状态：Bob 打开了这个事项在编辑
    bob_store.lock().unwrap().call(
        "desktop",
        SECRET,
        "desktop_sync_presence",
        json!({"date":"2026-10-15","item_id":id,"editing":true}),
    );
    wait_for("Alice 看到 Bob 正在编辑", || {
        status(&alice.store)["peers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["user"] == "Bob" && p["editing"] == true && p["item_id"] == json!(id))
    });
    // Bob 的修改反向同步
    let item = bob_store.lock().unwrap().call(
        "desktop",
        SECRET,
        "daygrid_get_item",
        json!({"item_id":id}),
    )["data"]["item"]
        .clone();
    let g = bob_store
        .lock()
        .unwrap()
        .call("desktop", SECRET, "daygrid_get_context", json!({}))["data"]["calendar_generation"]
        .clone();
    bob_store.lock().unwrap().call("desktop", SECRET, "daygrid_update_item", json!({"calendar_generation":g,"idempotency_key":Uuid::new_v4().to_string(),"item_id":id,"expected_revision":item["revision"],"patch":{"title":"Bob 改了标题"}}));
    wait_for("Alice 收到 Bob 的修改", || {
        alice.get(&id).unwrap()["title"] == "Bob 改了标题"
    });
    // 历史里能看到是谁改的
    let hist = alice.call("desktop_history", json!({}))["data"]["history"].clone();
    assert!(
        hist.as_array()
            .unwrap()
            .iter()
            .any(|h| h["actor"] == "sync:Bob"),
        "{hist}"
    );
    // 断开：配置、令牌、outbox 清除，本地事项保留
    let r = bob_store
        .lock()
        .unwrap()
        .call("desktop", SECRET, "desktop_sync_disconnect", json!({}));
    assert_eq!(r["ok"], true);
    assert_eq!(status(&bob_store)["linked"], false);
    assert_eq!(
        bob_store.lock().unwrap().call(
            "desktop",
            SECRET,
            "daygrid_get_item",
            json!({"item_id":id})
        )["ok"],
        true
    );
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("等待超时：{what}");
}

#[test]
fn sync_survives_server_restart_and_offline_period() {
    // 服务器不可达时本地照常工作，恢复后追平。
    let server = start_server();
    let alice = Pc::owner(&server);
    let id = alice.create("2026-10-15", "周会", "");
    alice.sync();
    // 把客户端指向一个没人监听的端口，模拟断网
    let mut offline_cfg = alice.cfg.clone();
    offline_cfg.url = "http://127.0.0.1:9".into();
    alice.update(&id, json!({"title":"离线修改"}));
    let r = client::sync_once(&alice.store, &alice.rt, &offline_cfg, &alice.token);
    assert!(matches!(r, Err(NetError::Offline(_))), "{r:?}");
    assert!(alice.pending() > 0, "离线期间修改留在 outbox");
    assert_eq!(alice.get(&id).unwrap()["title"], "离线修改", "本地照常可用");
    alice.sync(); // 网络恢复
    assert_eq!(alice.pending(), 0);
    let bob = Pc::join(&server, "Bob", "editor");
    bob.sync();
    assert_eq!(bob.get(&id).unwrap()["title"], "离线修改");
}

// ============================ 评审发现的问题（回归测试） ============================

impl Pc {
    /// 把这台电脑接到另一个服务器（先断开，再用新邀请码加入）。
    fn switch_to(&mut self, server: &Server, name: &str) {
        self.call("desktop_sync_disconnect", json!({}));
        let owner_cfg = SyncConfig {
            url: server.url.clone(),
            workspace_id: server.ws.clone(),
            workspace_name: "团队".into(),
            user_id: String::new(),
            user_name: String::new(),
            role: "owner".into(),
            client_id: "owner".into(),
        };
        let invite =
            client::create_invite(&owner_cfg, &server.owner_token, "editor", 24, 10).unwrap();
        let joined = client::join(&server.url, invite["code"].as_str().unwrap(), name).unwrap();
        let cfg = SyncConfig {
            url: server.url.clone(),
            workspace_id: server.ws.clone(),
            workspace_name: "团队".into(),
            user_id: joined.user.id,
            user_name: name.into(),
            role: joined.role,
            client_id: Uuid::new_v4().simple().to_string(),
        };
        self.store
            .lock()
            .unwrap()
            .sync_link(&cfg, &joined.token, true)
            .unwrap();
        self.cfg = cfg;
        self.token = joined.token;
    }

    /// 断开后重新连回同一个团队（同一个令牌）。
    fn reconnect(&mut self) {
        self.cfg.client_id = Uuid::new_v4().simple().to_string();
        self.store
            .lock()
            .unwrap()
            .sync_link(&self.cfg, &self.token, false)
            .unwrap();
    }
}

/// 评审 #1：推送途中又产生的修改，不能因为上一条被确认就把 base 往前挪，
/// 否则同事在这期间对同一字段的修改会被"没有并发"的判断直接盖掉。
#[test]
fn r1_edit_made_while_a_push_is_in_flight_does_not_overwrite_colleague() {
    let server = start_server();
    let (a, b) = (Pc::owner(&server), Pc::join(&server, "Bob", "editor"));
    let id = a.create("2026-10-15", "周会", "l1\nl2\nl3");
    settle(&[&a, &b]);
    a.update(&id, json!({"title":"周例会"}));
    let batch = a
        .store
        .lock()
        .unwrap()
        .sync_prepare_push(200)
        .unwrap()
        .unwrap();
    // 推送途中：A 又改了备注第 1 行；B 同时改了备注第 3 行并已同步到服务端。
    a.update(&id, json!({"notes":"A1\nl2\nl3"}));
    b.update(&id, json!({"notes":"l1\nl2\nB3"}));
    b.sync();
    let resp = client::push(
        &a.cfg,
        &a.token,
        &PushRequest {
            client_id: batch.client_id.clone(),
            ops: batch.ops.clone(),
        },
    )
    .unwrap();
    a.store
        .lock()
        .unwrap()
        .sync_apply_push(&batch, &resp)
        .unwrap();
    settle(&[&a, &b]);
    assert_eq!(
        a.get(&id).unwrap()["notes"],
        "A1\nl2\nB3",
        "两个人改的不同行都要保留"
    );
    assert_eq!(a.snapshot(), b.snapshot());
}

/// 评审 #2：同一个用户的两台设备，A 改第 1 行、B 离线改第 3 行，不能整段互相覆盖。
#[test]
fn r2_same_user_on_two_devices_merges_notes_by_line() {
    let server = start_server();
    let d1 = Pc::owner(&server);
    let d2 = Pc::owner(&server);
    let id = d1.create("2026-10-15", "周会", "l1\nl2\nl3");
    settle(&[&d1, &d2]);
    d1.update(&id, json!({"notes":"D1\nl2\nl3"}));
    d2.update(&id, json!({"notes":"l1\nl2\nD3"}));
    d1.sync();
    d2.sync();
    settle(&[&d1, &d2]);
    assert_eq!(d1.get(&id).unwrap()["notes"], "D1\nl2\nD3");
    assert_eq!(d1.snapshot(), d2.snapshot());
}

/// 评审 #3：从团队 A 拉下来的事项，断开后再连到团队 B，不能被上传到 B。
#[test]
fn r3_switching_teams_never_uploads_the_previous_teams_items() {
    let s1 = start_server();
    let bob = Pc::join(&s1, "Bob", "editor");
    let mut x = Pc::join(&s1, "X", "editor");
    let secret = bob.create("2026-10-15", "A 团队的机密", "只给 A 团队看");
    bob.sync();
    x.sync();
    assert!(x.get(&secret).is_some());
    let s2 = start_server();
    x.switch_to(&s2, "X");
    x.sync();
    let owner2 = Pc::owner(&s2);
    owner2.sync();
    assert!(owner2.get(&secret).is_none(), "A 团队的事项泄露到了 B 团队");
    assert!(server_snapshot(&s2, &owner2).is_empty());
}

/// 评审 #4：断开期间的本地修改与删除，重新连回同一团队后不能被服务端旧版本静默覆盖。
#[test]
fn r4_offline_changes_survive_disconnect_and_reconnect() {
    let server = start_server();
    let mut a = Pc::owner(&server);
    let b = Pc::join(&server, "Bob", "editor");
    let keep = a.create("2026-10-15", "周会", "");
    let gone = a.create("2026-10-16", "待删除", "");
    settle(&[&a, &b]);
    a.call("desktop_sync_disconnect", json!({}));
    a.update(&keep, json!({"title":"离线时改的标题"}));
    assert_eq!(a.delete(&gone)["ok"], true);
    b.update(&keep, json!({"color":"rose"}));
    b.sync();
    a.reconnect();
    settle(&[&a, &b]);
    let item = a.get(&keep).unwrap();
    assert_eq!(item["title"], "离线时改的标题", "断开期间的修改丢了");
    assert_eq!(item["color"], "rose", "同事的修改也要保留");
    assert!(a.get(&gone).is_none(), "断开期间删除的事项被拉回来了");
    assert!(b.get(&gone).is_none());
    assert_eq!(a.snapshot(), b.snapshot());
}

/// 评审 #5：服务端拒绝了修改（合并后校验失败），本地必须回到服务端状态，并留下可见记录。
#[test]
fn r5_rejected_edit_rolls_back_locally_and_is_reported() {
    let server = start_server();
    let (a, b) = (Pc::owner(&server), Pc::join(&server, "Bob", "editor"));
    let id = a.create("2026-10-15", "周会", "L1\nMID\nL3");
    settle(&[&a, &b]);
    a.update(
        &id,
        json!({"notes":format!("{}\nMID\nL3","a".repeat(6000))}),
    );
    b.update(
        &id,
        json!({"notes":format!("L1\nMID\n{}","b".repeat(6000))}),
    );
    a.sync();
    b.sync(); // 合并后超过 10000 字：被服务端拒绝
    settle(&[&a, &b]);
    assert_eq!(a.snapshot(), b.snapshot(), "被拒绝的修改让两边永久不一致");
    assert!(
        b.conflicts().iter().any(|c| c["state"] == "open"),
        "被拒绝的修改应该留下记录让用户知道"
    );
}

/// 评审 #6：冲突里的「status」也能用我的版本恢复；不能出现恢复一半的情况。
#[test]
fn r6_restoring_a_status_conflict_works_and_is_all_or_nothing() {
    let server = start_server();
    let (a, b) = (Pc::owner(&server), Pc::join(&server, "Bob", "editor"));
    let id = a.create("2026-10-15", "周会", "");
    settle(&[&a, &b]);
    assert_eq!(a.delete(&id)["ok"], true);
    a.sync();
    assert_eq!(b.set_status(&id, "done")["ok"], true); // B 还没看到删除
    b.sync();
    let conflict = b
        .conflicts()
        .into_iter()
        .find(|c| c["field"] == "status")
        .expect("status 冲突");
    let r = b.call(
        "desktop_sync_resolve_conflict",
        json!({"id":conflict["id"],"action":"restore"}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(b.get(&id).unwrap()["status"], "done");
    settle(&[&a, &b]);
    assert_eq!(a.snapshot(), b.snapshot());
}

/// 评审 #7：降级成只读之后批准旧请求，不能把修改塞进 outbox。
#[test]
fn r7_approving_an_old_proposal_after_becoming_viewer_does_not_write() {
    let server = start_server();
    let a = Pc::join(&server, "Alice2", "editor");
    let id = a.create("2026-10-15", "周会", "");
    a.sync();
    let conn = a.call(
        "desktop_create_connection",
        json!({"label":"助手","permission":"write"}),
    );
    let (cid, secret) = (
        conn["data"]["id"].as_str().unwrap().to_string(),
        conn["data"]["credential"].as_str().unwrap().to_string(),
    );
    a.call(
        "desktop_set_settings",
        json!({"approval_mode":"all_writes"}),
    );
    let item = a.get(&id).unwrap();
    let generation = a.generation();
    let proposed = a.store.lock().unwrap().call(
        &cid,
        &secret,
        "daygrid_update_item",
        json!({
            "calendar_generation":generation,"idempotency_key":Uuid::new_v4().to_string(),
            "item_id":id,"expected_revision":item["revision"],"patch":{"title":"助手想改的标题"}
        }),
    );
    let pid = proposed["data"]["proposal"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    a.store.lock().unwrap().sync_change_role("viewer").unwrap();
    a.call(
        "desktop_decide_proposal",
        json!({"proposal_id":pid,"decision":"approve"}),
    );
    assert_eq!(a.pending(), 0, "只读成员不应产生待推送修改");
    assert_eq!(a.get(&id).unwrap()["title"], "周会");
}

// ============================ 可见范围 / 回滚 / 角色 ============================

fn private_create(pc: &Pc, date: &str, title: &str) -> String {
    let r = pc.call(
        "daygrid_create_item",
        json!({"calendar_generation":pc.generation(),"idempotency_key":Uuid::new_v4().to_string(),"date":date,"title":title}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["data"]["item"]["visibility"], "private", "默认仅自己可见");
    r["data"]["item"]["id"].as_str().unwrap().into()
}

fn change_role(server: &Server, target: &str, role: &str) {
    let agent = client::agent(Some(Duration::from_secs(5)));
    let resp = agent
        .put(&format!(
            "{}/v1/workspaces/{}/members/{}",
            server.url, server.ws, target
        ))
        .header("Authorization", format!("Bearer {}", server.owner_token))
        .send_json(json!({"role":role}))
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
}

#[test]
fn v1_private_items_never_leave_the_machine() {
    let server = start_server();
    let (a, b) = (Pc::owner(&server), Pc::join(&server, "Bob", "editor"));
    let mine = private_create(&a, "2026-10-15", "我的私事");
    a.update(&mine, json!({"notes":"更私密的备注"}));
    assert_eq!(a.pending(), 0, "私有事项不进上传队列");
    settle(&[&a, &b]);
    assert!(b.get(&mine).is_none());
    assert!(
        server_snapshot(&server, &a).is_empty(),
        "服务器上不该有任何私有内容"
    );
    assert_eq!(a.delete(&mine)["ok"], true);
    assert_eq!(a.pending(), 0);
}

#[test]
fn v2_share_unshare_and_reshare_round_trip() {
    let server = start_server();
    let (a, b) = (Pc::owner(&server), Pc::join(&server, "Bob", "editor"));
    let id = private_create(&a, "2026-10-15", "周会");
    assert_eq!(a.set_visibility(&id, "shared")["ok"], true);
    settle(&[&a, &b]);
    let seen = b.get(&id).expect("共享后同事能看到");
    assert_eq!(seen["visibility"], "shared", "同事那里收到的就是共享事项");
    a.update(&id, json!({"title":"周例会"}));
    settle(&[&a, &b]);
    assert_eq!(b.get(&id).unwrap()["title"], "周例会");
    // 撤回共享：同事那里消失，我这里完整保留且变回私有。
    assert_eq!(a.set_visibility(&id, "private")["ok"], true);
    settle(&[&a, &b]);
    assert!(b.get(&id).is_none(), "撤回共享后同事不该再看到");
    let mine = a.get(&id).expect("我自己的事项还在");
    assert_eq!(
        (mine["title"].as_str(), mine["visibility"].as_str()),
        (Some("周例会"), Some("private"))
    );
    // 私有期间的修改不外传；重新共享后同事看到最新内容。
    a.update(&id, json!({"title":"私下改的标题","notes":"私下写的备注"}));
    settle(&[&a, &b]);
    assert!(b.get(&id).is_none());
    assert_eq!(a.set_visibility(&id, "shared")["ok"], true);
    settle(&[&a, &b]);
    let again = b.get(&id).expect("重新共享后可见");
    assert_eq!(
        (again["title"].as_str(), again["notes"].as_str()),
        (Some("私下改的标题"), Some("私下写的备注"))
    );
    assert_eq!(a.snapshot(), b.snapshot());
}

#[test]
fn v3_visibility_needs_a_team_and_a_writable_role() {
    // 没连接团队：不能设成共享。
    let (store, _dir) = Pc::open_store();
    let pc0 =
        |method: &str, args: Value| store.lock().unwrap().call("desktop", SECRET, method, args);
    let g = pc0("daygrid_get_context", json!({}))["data"]["calendar_generation"]
        .as_str()
        .unwrap()
        .to_string();
    let r = pc0(
        "daygrid_create_item",
        json!({"calendar_generation":g,"idempotency_key":Uuid::new_v4().to_string(),"date":"2026-10-15","title":"x","visibility":"shared"}),
    );
    assert_eq!(r["error"]["code"], "VALIDATION_ERROR", "{r}");
    // 只读成员：不能把事项设成共享，也不能新建共享事项。
    let server = start_server();
    let v = Pc::join(&server, "Vera", "viewer");
    let id = private_create(&v, "2026-10-15", "自己的");
    assert_eq!(
        v.set_visibility(&id, "shared")["error"]["code"],
        "PERMISSION_DENIED"
    );
    assert_eq!(v.pending(), 0);
}

#[test]
fn v4_link_without_sharing_keeps_existing_items_local() {
    let server = start_server();
    let (store, _dir) = Pc::open_store();
    let g = store
        .lock()
        .unwrap()
        .call("desktop", SECRET, "daygrid_get_context", json!({}))["data"]["calendar_generation"]
        .as_str()
        .unwrap()
        .to_string();
    for t in ["本地一", "本地二"] {
        let r = store.lock().unwrap().call("desktop", SECRET, "daygrid_create_item", json!({"calendar_generation":g,"idempotency_key":Uuid::new_v4().to_string(),"date":"2026-10-20","title":t}));
        assert_eq!(r["ok"], true);
    }
    let owner = Pc::owner(&server);
    let invite = client::create_invite(&owner.cfg, &server.owner_token, "editor", 24, 1).unwrap();
    let joined = client::join(&server.url, invite["code"].as_str().unwrap(), "Bob").unwrap();
    let cfg = SyncConfig {
        url: server.url.clone(),
        workspace_id: server.ws.clone(),
        workspace_name: "团队".into(),
        user_id: joined.user.id,
        user_name: "Bob".into(),
        role: "editor".into(),
        client_id: Uuid::new_v4().simple().to_string(),
    };
    let shared = store
        .lock()
        .unwrap()
        .sync_link(&cfg, &joined.token, false)
        .unwrap();
    assert_eq!(shared, 0);
    assert_eq!(
        store.lock().unwrap().sync_pending().unwrap(),
        0,
        "默认不上传任何本地事项"
    );
    let rt = store.lock().unwrap().runtime();
    client::sync_once(&store, &rt, &cfg, &joined.token).unwrap();
    assert!(server_snapshot(&server, &owner).is_empty());
}

#[test]
fn v5_presence_never_reveals_private_items() {
    let server = start_server();
    let a = Pc::owner(&server);
    let private = private_create(&a, "2026-10-15", "私有");
    let shared = a.create("2026-10-15", "共享", "");
    a.call(
        "desktop_sync_presence",
        json!({"date":"2026-10-15","item_id":private,"editing":true}),
    );
    assert_eq!(a.rt.presence().item_id, None, "私有事项不进在线状态");
    a.call(
        "desktop_sync_presence",
        json!({"date":"2026-10-15","item_id":shared,"editing":true}),
    );
    assert_eq!(a.rt.presence().item_id.as_deref(), Some(shared.as_str()));
}

#[test]
fn r5b_server_rejection_without_new_server_state_still_rolls_back() {
    // 评审 #5 的真正缺口：服务端拒绝了修改、且没有给出更新的版本（rev 没前进）时，
    // 旧实现既不回滚也不留记录。这里用一个合成的响应直接复现。
    use daygrid::sync::{OpError, OpResult, OpStatus, PushResponse};
    let server = start_server();
    let a = Pc::owner(&server);
    let id = a.create("2026-10-15", "周会", "");
    a.sync();
    a.update(&id, json!({"title":"会被拒绝的标题","notes":"也会被拒绝"}));
    let batch = a
        .store
        .lock()
        .unwrap()
        .sync_prepare_push(200)
        .unwrap()
        .unwrap();
    let resp = PushResponse {
        head_seq: 0,
        results: batch
            .ops
            .iter()
            .map(|o| OpResult {
                op_id: o.op_id.clone(),
                status: OpStatus::Rejected,
                item: None,
                conflicts: vec![],
                error: Some(OpError {
                    code: "VALIDATION_ERROR".into(),
                    message: "标题不合法（测试）".into(),
                }),
                clamped: false,
            })
            .collect(),
    };
    let stats = a
        .store
        .lock()
        .unwrap()
        .sync_apply_push(&batch, &resp)
        .unwrap();
    assert_eq!(stats.rejected, batch.ops.len());
    let item = a.get(&id).unwrap();
    assert_eq!(
        (item["title"].as_str(), item["notes"].as_str()),
        (Some("周会"), Some("")),
        "被拒绝的修改必须从本地撤销"
    );
    assert_eq!(a.pending(), 0);
    let open: Vec<Value> = a
        .conflicts()
        .into_iter()
        .filter(|c| c["state"] == "open")
        .collect();
    assert_eq!(open.len(), 2, "标题和备注各留一条记录：{open:?}");
    let title = open.iter().find(|c| c["field"] == "title").unwrap();
    assert_eq!(title["reason"], "invalid");
    assert_eq!(
        title["lost_value"], "会被拒绝的标题",
        "「你的版本」是你写的内容，不是错误文字"
    );
    assert_eq!(title["message"], "标题不合法（测试）");
}

#[test]
fn v6_role_downgrade_discards_unsendable_edits_and_upgrade_is_detected() {
    let server = start_server();
    let (owner, bob) = (Pc::owner(&server), Pc::join(&server, "Bob", "editor"));
    let id = owner.create("2026-10-15", "周会", "");
    settle(&[&owner, &bob]);
    // Bob 有一条还没上传的修改时被降级。
    bob.update(&id, json!({"title":"Bob 还没上传的修改"}));
    assert_eq!(bob.pending(), 1);
    change_role(&server, "Bob", "viewer");
    bob.sync(); // 推送时被告知只读，随后继续拉取
    let cfg = bob.store.lock().unwrap().sync_config().unwrap().unwrap();
    assert_eq!(cfg.role, "viewer");
    assert_eq!(bob.pending(), 0, "无法上传的修改不能一直堵在队列里");
    assert_eq!(bob.get(&id).unwrap()["title"], "周会", "本地回到服务端状态");
    let c = bob
        .conflicts()
        .into_iter()
        .find(|c| c["reason"] == "read_only")
        .expect("留下记录");
    assert_eq!(c["lost_value"], "Bob 还没上传的修改");
    // 只读期间不能改共享事项。
    assert_eq!(
        bob.update(&id, json!({"title":"x"}))["error"]["code"],
        "PERMISSION_DENIED"
    );
    // 升级回 editor：下一次拉取就能发现，不必等任何失败。
    change_role(&server, "Bob", "editor");
    bob.sync();
    let cfg = bob.store.lock().unwrap().sync_config().unwrap().unwrap();
    assert_eq!(cfg.role, "editor");
    assert_eq!(bob.update(&id, json!({"title":"升级后的修改"}))["ok"], true);
    settle(&[&owner, &bob]);
    assert_eq!(owner.get(&id).unwrap()["title"], "升级后的修改");
    // 仅靠拉取就能发现降级（没有待上传修改时）。
    change_role(&server, "Bob", "viewer");
    bob.sync();
    assert_eq!(
        bob.store
            .lock()
            .unwrap()
            .sync_config()
            .unwrap()
            .unwrap()
            .role,
        "viewer"
    );
}

#[test]
fn v7_offline_edits_while_unlinked_are_reconciled_per_field_not_overwritten() {
    // 评审 #4 的组合场景：断开期间本地改了标题、同事改了备注并且新增了一条；重连后三方都在。
    let server = start_server();
    let mut a = Pc::owner(&server);
    let b = Pc::join(&server, "Bob", "editor");
    let id = a.create("2026-10-15", "周会", "第一行\n第二行\n第三行\n第四行");
    settle(&[&a, &b]);
    a.call("desktop_sync_disconnect", json!({}));
    a.update(&id, json!({"notes":"第一行改\n第二行\n第三行\n第四行"}));
    b.update(&id, json!({"notes":"第一行\n第二行\n第三行\n第四行改"}));
    let extra = b.create("2026-10-16", "同事新增", "");
    b.sync();
    a.reconnect();
    settle(&[&a, &b]);
    assert_eq!(
        a.get(&id).unwrap()["notes"],
        "第一行改\n第二行\n第三行\n第四行改",
        "断开期间双方改的不同行都要保留"
    );
    assert!(a.get(&extra).is_some());
    assert_eq!(a.snapshot(), b.snapshot());
}

// ============================ 提醒：本机字段，不同步 ============================

/// 本机的提醒行：(due_ms, 是否已触发)。
fn reminder_row(pc: &Pc, id: &str) -> Option<(i64, bool)> {
    use rusqlite::OptionalExtension;
    rusqlite::Connection::open(pc.dir.path().join("calendar.db"))
        .unwrap()
        .query_row(
            "SELECT due_ms,fired_at IS NOT NULL FROM reminders WHERE item_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .unwrap()
}

#[test]
fn m1_reminders_stay_local_and_follow_colleague_edits() {
    use daygrid::domain::remind_epoch_ms;
    let server = start_server();
    let (alice, bob) = (Pc::owner(&server), Pc::join(&server, "Bob", "editor"));
    let id = alice.create("2026-10-15", "周会", "");
    settle(&[&alice, &bob]);
    let before = server_snapshot(&server, &alice);
    // 共享事项上设置提醒：不进 outbox，服务端的事项不变，同事那里也没有
    let r = bob.update(&id, json!({"remind_at":"2030-10-15T09:00"}));
    assert_eq!(r["ok"], true, "{r}");
    let remind = r["data"]["item"]["remind_at"].as_str().unwrap().to_string();
    assert_eq!(&remind[..16], "2030-10-15T09:00");
    assert_eq!(bob.pending(), 0, "设置提醒不产生同步操作");
    settle(&[&alice, &bob]);
    assert_eq!(server_snapshot(&server, &alice), before);
    assert!(alice.get(&id).unwrap()["remind_at"].is_null());
    assert_eq!(reminder_row(&alice, &id), None);
    // 同事改标题：提醒保留
    alice.update(&id, json!({"title":"周例会"}));
    alice.sync();
    bob.sync();
    let it = bob.get(&id).unwrap();
    assert_eq!(it["title"], "周例会");
    assert_eq!(it["remind_at"], remind.as_str());
    assert_eq!(
        reminder_row(&bob, &id),
        Some((remind_epoch_ms(&remind).unwrap(), false))
    );
    assert_eq!(bob.pending(), 0);
    // 同事改日期（+2 天）：提醒平移到新日期的同一时刻，平移不产生同步操作
    alice.update(&id, json!({"date":"2026-10-17"}));
    alice.sync();
    bob.sync();
    let it = bob.get(&id).unwrap();
    assert_eq!(it["date"], "2026-10-17");
    let shifted = it["remind_at"].as_str().unwrap().to_string();
    assert_eq!(&shifted[..16], "2030-10-17T09:00");
    assert_eq!(
        reminder_row(&bob, &id),
        Some((remind_epoch_ms(&shifted).unwrap(), false))
    );
    assert_eq!(bob.pending(), 0, "平移不产生同步操作");
    settle(&[&alice, &bob]);
    assert_eq!(bob.snapshot(), server_snapshot(&server, &bob));
    assert!(alice.get(&id).unwrap()["remind_at"].is_null());
    // 同事完成后又重新打开：提醒随状态维护
    alice.set_status(&id, "done");
    alice.sync();
    bob.sync();
    assert_eq!(reminder_row(&bob, &id), None);
    alice.set_status(&id, "planned");
    alice.sync();
    bob.sync();
    assert_eq!(
        reminder_row(&bob, &id),
        Some((remind_epoch_ms(&shifted).unwrap(), false))
    );
    // 同事删除：提醒行删除
    alice.delete(&id);
    alice.sync();
    bob.sync();
    assert!(bob.get(&id).is_none());
    assert_eq!(reminder_row(&bob, &id), None);
    assert_eq!(bob.pending(), 0);
    // 提示文件跟着同步更新（同步线程直接调用的公开方法末尾会刷新）
    let hint: Value = serde_json::from_str(
        &std::fs::read_to_string(bob.dir.path().join("reminders.next")).unwrap(),
    )
    .unwrap();
    assert_eq!(hint["next_due_ms"], Value::Null);
}

#[test]
fn m2_viewer_can_set_a_reminder_on_shared_items_but_nothing_else() {
    let server = start_server();
    let alice = Pc::owner(&server);
    let viewer = Pc::join(&server, "Vera", "viewer");
    let id = alice.create("2026-10-15", "周会", "");
    settle(&[&alice, &viewer]);
    let r = viewer.update(&id, json!({"remind_at":"2030-10-15T09:00"}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(reminder_row(&viewer, &id).is_some());
    assert_eq!(viewer.pending(), 0);
    let r = viewer.update(
        &id,
        json!({"title":"改一下","remind_at":"2030-10-15T10:00"}),
    );
    assert_eq!(r["error"]["code"], "PERMISSION_DENIED");
    settle(&[&alice, &viewer]);
    let it = viewer.get(&id).unwrap();
    assert_eq!(it["title"], "周会");
    assert_eq!(&it["remind_at"].as_str().unwrap()[..16], "2030-10-15T09:00");
    assert_eq!(alice.get(&id).unwrap()["title"], "周会");
    assert!(viewer.conflicts().is_empty());
}
