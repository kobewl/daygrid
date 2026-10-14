//! 服务端合并与权限逻辑（直接调用 `ServerDb`，不经过 HTTP）。
use daygrid::sync::{
    FIELDS, FieldEdit, Op, OpKind, OpStatus,
    hlc::{Clock, MAX_DRIFT_MS},
    now_ms,
};
use daygrid_server::db::{ApiError, ServerDb, User};
use std::collections::BTreeMap;
use uuid::Uuid;

struct World {
    db: ServerDb,
    ws: String,
    alice: User,
    bob: User,
    ca: Clock,
    cb: Clock,
    now: u64,
}

impl World {
    fn new() -> Self {
        let mut db = ServerDb::open_in_memory().unwrap();
        let (ws, a_id, a_token) = db.init_workspace("团队", "Alice").unwrap();
        let invite = db
            .create_invite(&ws, &a_id, "editor", 24, 5, now_ms())
            .unwrap();
        let joined = db
            .join(invite["code"].as_str().unwrap(), "Bob", now_ms())
            .unwrap();
        let alice = db.authenticate(&a_token).unwrap();
        let bob = db.authenticate(&joined.token).unwrap();
        Self {
            db,
            ws,
            alice,
            bob,
            ca: Clock::new("aaaa"),
            cb: Clock::new("bbbb"),
            now: now_ms(),
        }
    }
    /// 每次调用时间前进 10ms，保证先后顺序可预测。
    fn tick(&mut self) -> u64 {
        self.now += 10;
        self.now
    }
    fn ts_a(&mut self) -> String {
        let n = self.tick();
        self.ca.send(n).to_string()
    }
    fn ts_b(&mut self) -> String {
        let n = self.tick();
        self.cb.send(n).to_string()
    }
}

fn edits(pairs: &[(&str, &str)], ts: &str) -> BTreeMap<String, FieldEdit> {
    pairs
        .iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                FieldEdit {
                    v: v.to_string(),
                    ts: ts.to_string(),
                    prev: None,
                },
            )
        })
        .collect()
}
fn op(
    kind: OpKind,
    item: &str,
    base_rev: i64,
    fields: BTreeMap<String, FieldEdit>,
    ts: &str,
) -> Op {
    Op {
        op_id: Uuid::new_v4().to_string(),
        item_id: item.to_string(),
        kind,
        base_rev,
        ts: ts.to_string(),
        fields,
        created_at: None,
    }
}
fn create_op(item: &str, title: &str, notes: &str, ts: &str) -> Op {
    let all = edits(
        &[
            ("date", "2026-10-15"),
            ("title", title),
            ("notes", notes),
            ("color", "blue"),
            ("status", "planned"),
        ],
        ts,
    );
    op(OpKind::Create, item, 0, all, ts)
}
fn push(w: &mut World, user: &User, ops: Vec<Op>) -> Vec<daygrid::sync::OpResult> {
    let now = w.now;
    w.db.push(&w.ws.clone(), user, &ops, now).unwrap().results
}
fn item(w: &World, id: &str) -> daygrid::sync::ServerItem {
    w.db.get_item(&w.ws, id).unwrap().unwrap()
}

#[test]
fn create_then_update_applies_and_bumps_rev() {
    let mut w = World::new();
    let id = Uuid::new_v4().to_string();
    let (ts1, ts2) = (w.ts_a(), w.ts_a());
    let alice = w.alice.clone();
    let r = push(&mut w, &alice, vec![create_op(&id, "周会", "", &ts1)]);
    assert_eq!(r[0].status, OpStatus::Applied);
    assert_eq!(item(&w, &id).rev, 1);
    let r = push(
        &mut w,
        &alice,
        vec![op(
            OpKind::Update,
            &id,
            1,
            edits(&[("title", "周例会")], &ts2),
            &ts2,
        )],
    );
    assert_eq!(r[0].status, OpStatus::Applied);
    let it = item(&w, &id);
    assert_eq!(
        (it.rev, it.fields.title.as_str(), it.updated_by.as_str()),
        (2, "周例会", "Alice")
    );
}

#[test]
fn concurrent_edits_to_different_fields_both_survive() {
    let mut w = World::new();
    let id = Uuid::new_v4().to_string();
    let t0 = w.ts_a();
    let (alice, bob) = (w.alice.clone(), w.bob.clone());
    push(&mut w, &alice, vec![create_op(&id, "周会", "", &t0)]);
    // 两人都基于 rev1 修改不同字段
    let (ta, tb) = (w.ts_a(), w.ts_b());
    push(
        &mut w,
        &alice,
        vec![op(
            OpKind::Update,
            &id,
            1,
            edits(&[("color", "rose")], &ta),
            &ta,
        )],
    );
    let r = push(
        &mut w,
        &bob,
        vec![op(
            OpKind::Update,
            &id,
            1,
            edits(&[("title", "周例会")], &tb),
            &tb,
        )],
    );
    assert_eq!(r[0].status, OpStatus::Applied, "不同字段不算冲突");
    assert!(r[0].conflicts.is_empty());
    let it = item(&w, &id);
    assert_eq!(
        (it.fields.title.as_str(), it.fields.color.as_str(), it.rev),
        ("周例会", "rose", 3)
    );
}

#[test]
fn same_field_conflict_later_edit_wins_and_loser_is_notified() {
    let mut w = World::new();
    let id = Uuid::new_v4().to_string();
    let t0 = w.ts_a();
    let (alice, bob) = (w.alice.clone(), w.bob.clone());
    push(&mut w, &alice, vec![create_op(&id, "周会", "", &t0)]);
    let (ta, tb) = (w.ts_a(), w.ts_b()); // Alice 先改，Bob 后改
    push(
        &mut w,
        &alice,
        vec![op(
            OpKind::Update,
            &id,
            1,
            edits(&[("title", "Alice 的标题")], &ta),
            &ta,
        )],
    );
    let r = push(
        &mut w,
        &bob,
        vec![op(
            OpKind::Update,
            &id,
            1,
            edits(&[("title", "Bob 的标题")], &tb),
            &tb,
        )],
    );
    assert_eq!(r[0].status, OpStatus::Merged);
    assert_eq!(item(&w, &id).fields.title, "Bob 的标题", "更晚修改的人赢");
    // Alice 拉取时收到「你的修改被覆盖」
    let pulled = w.db.pull(&w.ws.clone(), &alice, 0, 0, 100).unwrap();
    assert_eq!(pulled.conflicts.len(), 1);
    let c = &pulled.conflicts[0];
    assert_eq!(
        (
            c.field.as_str(),
            c.lost_value.as_str(),
            c.kept_value.as_str(),
            c.kept_by.as_str()
        ),
        ("title", "Alice 的标题", "Bob 的标题", "Bob")
    );
    // Bob 自己没有冲突需要处理
    assert!(
        w.db.pull(&w.ws.clone(), &bob, 0, 0, 100)
            .unwrap()
            .conflicts
            .is_empty()
    );
    // 冲突游标推进后不再重复下发
    assert!(
        w.db.pull(&w.ws.clone(), &alice, 0, pulled.conflict_head, 100)
            .unwrap()
            .conflicts
            .is_empty()
    );
}

#[test]
fn offline_edit_made_earlier_loses_even_though_it_arrives_later() {
    let mut w = World::new();
    let id = Uuid::new_v4().to_string();
    let t0 = w.ts_a();
    let (alice, bob) = (w.alice.clone(), w.bob.clone());
    push(&mut w, &alice, vec![create_op(&id, "周会", "", &t0)]);
    let (t_old, t_new) = (w.ts_a(), w.ts_b()); // Alice 离线时改（更早），Bob 在线改（更晚）
    push(
        &mut w,
        &bob,
        vec![op(
            OpKind::Update,
            &id,
            1,
            edits(&[("title", "Bob 新标题")], &t_new),
            &t_new,
        )],
    );
    let r = push(
        &mut w,
        &alice,
        vec![op(
            OpKind::Update,
            &id,
            1,
            edits(&[("title", "Alice 旧标题")], &t_old),
            &t_old,
        )],
    );
    assert_eq!(r[0].status, OpStatus::Merged);
    assert_eq!(item(&w, &id).fields.title, "Bob 新标题");
    assert_eq!(r[0].conflicts.len(), 1, "败者当场就能在推送结果里看到");
    assert_eq!(r[0].conflicts[0].lost_value, "Alice 旧标题");
}

#[test]
fn notes_merge_by_line_across_users() {
    let mut w = World::new();
    let id = Uuid::new_v4().to_string();
    let t0 = w.ts_a();
    let (alice, bob) = (w.alice.clone(), w.bob.clone());
    push(
        &mut w,
        &alice,
        vec![create_op(&id, "会", "议程一\n议程二\n议程三\n", &t0)],
    );
    let (ta, tb) = (w.ts_a(), w.ts_b());
    push(
        &mut w,
        &alice,
        vec![op(
            OpKind::Update,
            &id,
            1,
            edits(&[("notes", "议程一（改）\n议程二\n议程三\n")], &ta),
            &ta,
        )],
    );
    let r = push(
        &mut w,
        &bob,
        vec![op(
            OpKind::Update,
            &id,
            1,
            edits(&[("notes", "议程一\n议程二\n议程三\n议程四\n")], &tb),
            &tb,
        )],
    );
    assert_eq!(r[0].status, OpStatus::Merged);
    assert!(
        r[0].conflicts.is_empty(),
        "不同行的修改自动合并，不产生冲突"
    );
    assert_eq!(
        item(&w, &id).fields.notes,
        "议程一（改）\n议程二\n议程三\n议程四\n"
    );
}

#[test]
fn delete_wins_over_concurrent_edit_and_edit_is_recoverable() {
    let mut w = World::new();
    let id = Uuid::new_v4().to_string();
    let t0 = w.ts_a();
    let (alice, bob) = (w.alice.clone(), w.bob.clone());
    push(&mut w, &alice, vec![create_op(&id, "周会", "", &t0)]);
    let (td, te) = (w.ts_a(), w.ts_b());
    push(
        &mut w,
        &alice,
        vec![op(OpKind::Delete, &id, 1, BTreeMap::new(), &td)],
    );
    let r = push(
        &mut w,
        &bob,
        vec![op(
            OpKind::Update,
            &id,
            1,
            edits(&[("title", "Bob 想改的标题")], &te),
            &te,
        )],
    );
    assert_eq!(r[0].status, OpStatus::Rejected);
    assert_eq!(r[0].error.as_ref().unwrap().code, "ITEM_DELETED");
    assert!(r[0].item.as_ref().unwrap().deleted);
    assert_eq!(r[0].conflicts[0].lost_value, "Bob 想改的标题");
    assert_eq!(r[0].conflicts[0].reason, "item_deleted");
    // 恢复：更晚的 restore 生效
    let tr = w.ts_b();
    let r = push(
        &mut w,
        &bob,
        vec![op(OpKind::Restore, &id, 2, BTreeMap::new(), &tr)],
    );
    assert_eq!(r[0].status, OpStatus::Applied);
    assert!(!item(&w, &id).deleted);
    // 比删除更早的恢复被压住
    let t_late_delete = w.ts_a();
    push(
        &mut w,
        &alice,
        vec![op(OpKind::Delete, &id, 3, BTreeMap::new(), &t_late_delete)],
    );
    let r = push(
        &mut w,
        &bob,
        vec![op(OpKind::Restore, &id, 3, BTreeMap::new(), &tr)],
    );
    assert_eq!(r[0].error.as_ref().unwrap().code, "LOST_TO_NEWER");
    assert!(item(&w, &id).deleted);
}

#[test]
fn replaying_an_op_is_idempotent() {
    let mut w = World::new();
    let id = Uuid::new_v4().to_string();
    let ts = w.ts_a();
    let alice = w.alice.clone();
    let c = create_op(&id, "周会", "", &ts);
    let first = push(&mut w, &alice, vec![c.clone()]);
    let again = push(&mut w, &alice, vec![c.clone()]);
    assert_eq!(first[0], again[0], "重放返回当初的结果");
    assert_eq!(item(&w, &id).rev, 1);
    assert_eq!(w.db.count("SELECT COUNT(*) FROM changes"), 1);
    let ts2 = w.ts_a();
    let u = op(
        OpKind::Update,
        &id,
        1,
        edits(&[("title", "改")], &ts2),
        &ts2,
    );
    push(&mut w, &alice, vec![u.clone()]);
    push(&mut w, &alice, vec![u.clone()]);
    push(&mut w, &alice, vec![u]);
    assert_eq!(item(&w, &id).rev, 2, "同一 op_id 不会重复递增版本");
}

#[test]
fn creating_an_existing_item_is_a_noop() {
    let mut w = World::new();
    let id = Uuid::new_v4().to_string();
    let ts = w.ts_a();
    let (alice, bob) = (w.alice.clone(), w.bob.clone());
    push(&mut w, &alice, vec![create_op(&id, "周会", "", &ts)]);
    let r = push(&mut w, &bob, vec![create_op(&id, "撞 ID", "", &ts)]);
    assert_eq!(r[0].status, OpStatus::Noop);
    assert_eq!(item(&w, &id).fields.title, "周会");
}

#[test]
fn future_timestamps_are_clamped_and_cannot_win_forever() {
    let mut w = World::new();
    let id = Uuid::new_v4().to_string();
    let t0 = w.ts_a();
    let (alice, bob) = (w.alice.clone(), w.bob.clone());
    push(&mut w, &alice, vec![create_op(&id, "周会", "", &t0)]);
    // Bob 的电脑时钟被设到 2099 年
    let evil = Clock::new("bbbb").send(4_000_000_000_000).to_string();
    let r = push(
        &mut w,
        &bob,
        vec![op(
            OpKind::Update,
            &id,
            1,
            edits(&[("title", "我永远赢")], &evil),
            &evil,
        )],
    );
    assert!(r[0].clamped);
    assert_eq!(item(&w, &id).fields.title, "我永远赢", "第一次确实生效");
    // 之后 Alice 在正常时间的修改仍然能赢回来（夹紧后的时间戳只领先 5 分钟）
    w.now += MAX_DRIFT_MS + 60_000;
    let ts = w.ts_a();
    let r = push(
        &mut w,
        &alice,
        vec![op(
            OpKind::Update,
            &id,
            2,
            edits(&[("title", "Alice 夺回")], &ts),
            &ts,
        )],
    );
    assert_eq!(item(&w, &id).fields.title, "Alice 夺回", "{r:?}");
}

#[test]
fn invalid_ops_are_rejected_without_touching_data() {
    let mut w = World::new();
    let id = Uuid::new_v4().to_string();
    let ts = w.ts_a();
    let alice = w.alice.clone();
    let mut empty_title = create_op(&id, "   ", "", &ts);
    empty_title.fields.insert(
        "title".into(),
        FieldEdit {
            v: "   ".into(),
            ts: ts.clone(),
            prev: None,
        },
    );
    let bad_color = {
        let mut o = create_op(&Uuid::new_v4().to_string(), "x", "", &ts);
        o.fields.insert(
            "color".into(),
            FieldEdit {
                v: "pink".into(),
                ts: ts.clone(),
                prev: None,
            },
        );
        o
    };
    let bad_ts = {
        let mut o = create_op(&Uuid::new_v4().to_string(), "x", "", &ts);
        o.ts = "not-a-timestamp".into();
        o
    };
    let partial_create = {
        let mut o = create_op(&Uuid::new_v4().to_string(), "x", "", &ts);
        o.fields.remove("notes");
        o
    };
    let unknown_field = op(OpKind::Update, &id, 1, edits(&[("bogus", "1")], &ts), &ts);
    let bad_id = create_op("not-a-uuid", "x", "", &ts);
    let r = push(
        &mut w,
        &alice,
        vec![
            empty_title,
            bad_color,
            bad_ts,
            partial_create,
            unknown_field,
            bad_id,
        ],
    );
    assert!(r.iter().all(|r| r.status == OpStatus::Rejected), "{r:?}");
    assert_eq!(w.db.count("SELECT COUNT(*) FROM items"), 0);
    // 更新不存在的事项
    let r = push(
        &mut w,
        &alice,
        vec![op(
            OpKind::Update,
            &Uuid::new_v4().to_string(),
            1,
            edits(&[("title", "x")], &ts),
            &ts,
        )],
    );
    assert_eq!(r[0].error.as_ref().unwrap().code, "NOT_FOUND");
    // 合并后非法（把标题改成空）
    push(&mut w, &alice, vec![create_op(&id, "周会", "", &ts)]);
    let ts2 = w.ts_a();
    let r = push(
        &mut w,
        &alice,
        vec![op(
            OpKind::Update,
            &id,
            1,
            edits(&[("title", "  ")], &ts2),
            &ts2,
        )],
    );
    assert_eq!(r[0].error.as_ref().unwrap().code, "VALIDATION_ERROR");
    assert_eq!(item(&w, &id).fields.title, "周会");
}

#[test]
fn pull_is_incremental_compact_and_paged() {
    let mut w = World::new();
    let alice = w.alice.clone();
    let ids: Vec<String> = (0..5).map(|_| Uuid::new_v4().to_string()).collect();
    for id in &ids {
        let ts = w.ts_a();
        push(&mut w, &alice, vec![create_op(id, "事项", "", &ts)]);
    }
    // 对第一个事项再改两次：pull 只返回它的最新状态一次
    for n in 0..2 {
        let ts = w.ts_a();
        let title = format!("改{n}");
        push(
            &mut w,
            &alice,
            vec![op(
                OpKind::Update,
                &ids[0],
                n + 1,
                edits(&[("title", &title)], &ts),
                &ts,
            )],
        );
    }
    let first = w.db.pull(&w.ws.clone(), &alice, 0, 0, 3).unwrap();
    assert!(first.has_more);
    assert_eq!(first.items.len(), 3);
    let second =
        w.db.pull(&w.ws.clone(), &alice, first.next_since, 0, 3)
            .unwrap();
    assert!(!second.has_more);
    let mut seen: Vec<String> = first
        .items
        .iter()
        .chain(&second.items)
        .map(|i| i.id.clone())
        .collect();
    seen.sort();
    let mut expect = ids.clone();
    expect.sort();
    assert_eq!(seen, expect, "分页不重不漏");
    let head = second.head_seq;
    assert_eq!(second.next_since, head);
    assert!(
        w.db.pull(&w.ws.clone(), &alice, head, 0, 3)
            .unwrap()
            .items
            .is_empty()
    );
    let first_item = first
        .items
        .iter()
        .chain(&second.items)
        .find(|i| i.id == ids[0])
        .unwrap();
    assert_eq!(
        (first_item.rev, first_item.fields.title.as_str()),
        (3, "改1")
    );
}

#[test]
fn invites_expire_run_out_and_members_can_be_removed() {
    let mut db = ServerDb::open_in_memory().unwrap();
    let (ws, owner, token) = db.init_workspace("团队", "Alice").unwrap();
    let now = now_ms();
    let once = db.create_invite(&ws, &owner, "editor", 1, 1, now).unwrap();
    let code = once["code"].as_str().unwrap().to_string();
    let joined = db.join(&code, "Bob", now).unwrap();
    assert_eq!(joined.role, "editor");
    let again: ApiError = db.join(&code, "Eve", now).map(|_| ()).unwrap_err();
    assert_eq!(again.code, "INVITE_INVALID", "用尽的邀请码不能再用");
    let short = db.create_invite(&ws, &owner, "viewer", 1, 5, now).unwrap();
    let expired: ApiError = db
        .join(short["code"].as_str().unwrap(), "Late", now + 2 * 3_600_000)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(expired.code, "INVITE_INVALID", "过期的邀请码不能用");
    assert!(db.join("dgi_nonexistent", "X", now).is_err());
    assert!(
        db.create_invite(&ws, &owner, "owner", 1, 1, now).is_err(),
        "不能邀请 owner"
    );
    // 移除 Bob：令牌立即失效
    let bob = db.authenticate(&joined.token).unwrap();
    assert_eq!(db.role(&ws, &bob.id).unwrap(), "editor");
    db.remove_member(&ws, None, "Bob").unwrap();
    assert!(db.authenticate(&joined.token).is_err());
    // 不能移除自己 / owner
    let alice = db.authenticate(&token).unwrap();
    assert!(db.remove_member(&ws, Some(&alice.id), &alice.id).is_err());
    assert!(db.remove_member(&ws, None, "Alice").is_err());
    // 非成员访问被拒
    let (ws2, _, _) = db.init_workspace("另一个", "Carol").unwrap();
    assert_eq!(db.role(&ws2, &alice.id).unwrap_err().status, 403);
}

#[test]
fn tokens_and_invites_are_stored_hashed_only() {
    let mut db = ServerDb::open_in_memory().unwrap();
    let (ws, owner, token) = db.init_workspace("团队", "Alice").unwrap();
    let invite = db
        .create_invite(&ws, &owner, "editor", 1, 1, now_ms())
        .unwrap();
    let code = invite["code"].as_str().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.db");
    db.backup_to(&path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let contains = |needle: &str| bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
    assert!(
        !contains(&token) && !contains(code),
        "明文令牌/邀请码不应出现在数据库里"
    );
}

#[test]
fn all_syncable_fields_are_covered_by_the_protocol() {
    // 防止有人给 Item 加字段却忘了同步：协议字段与领域字段必须一致。
    assert_eq!(FIELDS, ["date", "title", "notes", "color", "status"]);
}

#[test]
fn replaying_someone_elses_op_id_is_refused_and_leaks_nothing() {
    // 评审（低）：applied_ops 不看用户——别人撞上同一个 op_id 会读到别人当初的结果。
    let mut w = World::new();
    let id = Uuid::new_v4().to_string();
    let ts = w.ts_a();
    let (alice, bob) = (w.alice.clone(), w.bob.clone());
    let create = create_op(&id, "Alice 的机密", "", &ts);
    let first = push(&mut w, &alice, vec![create.clone()]);
    assert_eq!(first[0].status, OpStatus::Applied);
    let stolen = push(&mut w, &bob, vec![create.clone()]);
    assert_eq!(stolen[0].status, OpStatus::Rejected);
    assert_eq!(stolen[0].error.as_ref().unwrap().code, "OP_ID_REUSED");
    assert!(stolen[0].item.is_none(), "不能把别人的结果回给他");
    // 本人重放仍然是幂等的。
    let again = push(&mut w, &alice, vec![create]);
    assert_eq!(again[0], first[0]);
}

#[test]
fn oversized_values_and_hlc_counters_are_rejected() {
    // 评审 #8：HLC 计数器无上界；字段值（含 prev）长度也要有上限。
    let mut w = World::new();
    let id = Uuid::new_v4().to_string();
    let ts = w.ts_a();
    let alice = w.alice.clone();
    assert_eq!(
        push(&mut w, &alice, vec![create_op(&id, "周会", "", &ts)])[0].status,
        OpStatus::Applied
    );
    let wall = &ts[..13];
    let overflow = format!("{wall}-100000-aaaa");
    let mut o = op(
        OpKind::Update,
        &id,
        1,
        edits(&[("title", "x")], &overflow),
        &overflow,
    );
    assert_eq!(
        push(&mut w, &alice, vec![o.clone()])[0].status,
        OpStatus::Rejected
    );
    let ts2 = w.ts_a();
    o = op(OpKind::Update, &id, 1, edits(&[("title", "x")], &ts2), &ts2);
    o.fields.get_mut("title").unwrap().prev = Some("p".repeat(30_000));
    assert_eq!(push(&mut w, &alice, vec![o])[0].status, OpStatus::Rejected);
}

#[test]
fn owner_can_change_roles_but_not_the_owners_own() {
    let mut w = World::new();
    let ws = w.ws.clone();
    w.db.set_role(&ws, "Bob", "viewer").unwrap();
    assert_eq!(w.db.role(&ws, &w.bob.id).unwrap(), "viewer");
    let pulled = w.db.pull(&ws, &w.bob.clone(), 0, 0, 10).unwrap();
    assert_eq!(pulled.role.as_deref(), Some("viewer"), "角色随拉取下发");
    w.db.set_role(&ws, "Bob", "editor").unwrap();
    assert!(w.db.set_role(&ws, "Alice", "viewer").is_err());
    assert!(w.db.set_role(&ws, "Bob", "owner").is_err());
    assert!(w.db.set_role(&ws, "路人", "editor").is_err());
}
