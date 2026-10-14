//! 字段级三方合并（纯函数，无 I/O）。服务端在一个事务里对每条 `update` 调用它。
//!
//! 类比 Java：`git merge` + `@Version` 乐观锁的结合——
//! - `base`  = 客户端修改时看到的版本（共同祖先，来自 `item_versions`）；
//! - `cur`   = 服务器现在的版本（"ours"）；
//! - `edits` = 客户端想做的修改（"theirs"，只含它真正改了的字段）。
//!
//! 规则（逐字段）：
//! 1. 新值等于现值 → 已收敛，什么都不做；
//! 2. 新值等于基准值 → 客户端其实没改这个字段（例如改了又改回去），**不能**覆盖别人的修改；
//! 3. 基准与现值相同 → 没有并发修改，直接应用；
//! 4. 否则是并发修改：备注先尝试行级三方合并；合不拢（或其他字段）按 HLC 做 LWW，
//!    败者写入冲突记录，保证"不静默丢数据"。
//!
//! "基准"优先取客户端随修改一起带来的 `prev`（它改之前看到的值），没有才退回历史版本。
//! 这样同一台设备连续的修改天然是前后衔接的（后一条的 `prev` 就是前一条的结果），
//! 不需要"同一个用户 / 同一台设备就不算冲突"这种按身份判断的捷径——
//! 那种捷径会让同一用户的两台设备互相整段覆盖。
use super::{FieldEdit, ItemFields, REASON_CONCURRENT};
use std::collections::BTreeMap;

/// 某字段最近一次被修改的时间戳与修改者（用户 ID）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldMeta {
    pub ts: String,
    pub by: String,
}

pub type Meta = BTreeMap<String, FieldMeta>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeConflict {
    pub field: String,
    pub kept_value: String,
    pub lost_value: String,
    /// 保留方 / 被覆盖方的用户 ID（空串表示"从未被人修改过"）。
    pub kept_by: String,
    pub lost_by: String,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub fields: ItemFields,
    pub meta: Meta,
    /// 是否有字段值发生变化（决定 `rev` 是否递增）。
    pub changed: bool,
    /// 是否经过了自动合并或冲突仲裁（决定返回 `merged` 还是 `applied`）。
    pub merged: bool,
    pub conflicts: Vec<MergeConflict>,
}

fn newer(a: &str, b: &str) -> String {
    // HLC 字符串定长，字典序即时间序；调用方已验证格式。
    if a >= b { a } else { b }.to_string()
}

pub fn merge_update(
    cur: &ItemFields,
    meta: &Meta,
    base: Option<&ItemFields>,
    edits: &BTreeMap<String, FieldEdit>,
    user: &str,
) -> Merged {
    let mut out = Merged {
        fields: cur.clone(),
        meta: meta.clone(),
        changed: false,
        merged: false,
        conflicts: Vec::new(),
    };
    for (field, edit) in edits {
        let Some(current) = cur.get(field) else {
            continue; // 未知字段由调用方在校验阶段拒绝
        };
        let v = edit.v.as_str();
        let old = meta.get(field).cloned().unwrap_or_default();
        let record = |out: &mut Merged, value: &str, ts: &str, by: &str| {
            out.fields.set(field, value.to_string());
            out.meta.insert(
                field.clone(),
                FieldMeta {
                    ts: newer(ts, &old.ts),
                    by: by.to_string(),
                },
            );
        };
        // 1. 已收敛：只推进时间戳，保持修改者不变。
        if v == current {
            if edit.ts > old.ts {
                out.meta.entry(field.clone()).or_default().ts = edit.ts.clone();
            }
            continue;
        }
        // 共同祖先：客户端带来的 prev 优先，其次是 base_rev 对应的历史版本。
        let ancestor: Option<&str> = edit
            .prev
            .as_deref()
            .or_else(|| base.and_then(|b| b.get(field)));
        // 2. 客户端没有真正修改这个字段。
        if ancestor == Some(v) {
            continue;
        }
        // 3/4. 是否有并发修改：有共同祖先就比字段，没有就保守地当作并发。
        let concurrent = match ancestor {
            Some(b) => b != current,
            None => true,
        };
        if !concurrent {
            record(&mut out, v, &edit.ts, user);
            out.changed = true;
            continue;
        }
        // 备注：行级三方合并（对任何人、任何设备一视同仁）。
        if field == "notes"
            && let Some(ancestor) = ancestor
            && let Ok(text) = diffy::merge(ancestor, current, v)
        {
            if text != current {
                record(&mut out, &text, &edit.ts, user);
                out.changed = true;
            }
            out.merged = true;
            continue;
        }
        // LWW：谁的修改发生得更晚谁赢（HLC 全序）。
        out.merged = true;
        if edit.ts > old.ts {
            out.conflicts.push(MergeConflict {
                field: field.clone(),
                kept_value: v.to_string(),
                lost_value: current.to_string(),
                kept_by: user.to_string(),
                lost_by: old.by.clone(),
                reason: REASON_CONCURRENT,
            });
            record(&mut out, v, &edit.ts, user);
            out.changed = true;
        } else {
            out.conflicts.push(MergeConflict {
                field: field.clone(),
                kept_value: current.to_string(),
                lost_value: v.to_string(),
                kept_by: old.by.clone(),
                lost_by: user.to_string(),
                reason: REASON_CONCURRENT,
            });
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Delete,
    Restore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleOutcome {
    /// 已经是目标状态。
    Noop,
    Apply,
    /// 被更晚的相反操作压住（删除与恢复是一个 LWW 布尔寄存器）。
    LostToNewer,
}

/// 删除 / 恢复：`last_ts` 是最近一次生命周期事件（删除或恢复）的时间戳。
pub fn lifecycle(
    deleted: bool,
    last_ts: Option<&str>,
    op: Lifecycle,
    ts: &str,
) -> LifecycleOutcome {
    let target_deleted = op == Lifecycle::Delete;
    if deleted == target_deleted {
        return LifecycleOutcome::Noop;
    }
    match last_ts {
        Some(last) if last > ts => LifecycleOutcome::LostToNewer,
        _ => LifecycleOutcome::Apply,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(title: &str, notes: &str) -> ItemFields {
        ItemFields {
            date: "2026-10-15".into(),
            title: title.into(),
            notes: notes.into(),
            color: "blue".into(),
            status: "planned".into(),
        }
    }
    fn edit(field: &str, v: &str, ts: &str) -> BTreeMap<String, FieldEdit> {
        BTreeMap::from([(
            field.to_string(),
            FieldEdit {
                v: v.into(),
                ts: ts.into(),
                prev: None,
            },
        )])
    }
    fn meta(field: &str, ts: &str, by: &str) -> Meta {
        Meta::from([(
            field.to_string(),
            FieldMeta {
                ts: ts.into(),
                by: by.into(),
            },
        )])
    }
    const T1: &str = "0000000000100-00000-aaaa";
    const T2: &str = "0000000000200-00000-bbbb";
    const T3: &str = "0000000000300-00000-aaaa";

    #[test]
    fn no_concurrency_applies_directly() {
        let base = fields("周会", "");
        let r = merge_update(
            &base,
            &Meta::new(),
            Some(&base),
            &edit("title", "周例会", T2),
            "u2",
        );
        assert_eq!(r.fields.title, "周例会");
        assert!(r.changed && !r.merged && r.conflicts.is_empty());
        assert_eq!(
            r.meta["title"],
            FieldMeta {
                ts: T2.into(),
                by: "u2".into()
            }
        );
    }

    #[test]
    fn different_fields_both_survive() {
        let base = fields("周会", "");
        let mut cur = base.clone();
        cur.color = "rose".into(); // 别人先把颜色改了
        let r = merge_update(
            &cur,
            &meta("color", T1, "u1"),
            Some(&base),
            &edit("title", "周例会", T2),
            "u2",
        );
        assert_eq!(
            (r.fields.title.as_str(), r.fields.color.as_str()),
            ("周例会", "rose")
        );
        assert!(r.changed && r.conflicts.is_empty());
    }

    #[test]
    fn converged_value_changes_nothing_but_advances_timestamp() {
        let base = fields("周会", "");
        let mut cur = base.clone();
        cur.title = "周例会".into();
        let r = merge_update(
            &cur,
            &meta("title", T1, "u1"),
            Some(&base),
            &edit("title", "周例会", T2),
            "u2",
        );
        assert!(!r.changed && r.conflicts.is_empty());
        assert_eq!(
            r.meta["title"],
            FieldMeta {
                ts: T2.into(),
                by: "u1".into()
            }
        );
    }

    #[test]
    fn client_that_did_not_really_change_a_field_cannot_overwrite_others() {
        // 客户端把标题改了又改回基准值 → 合并后 v == base，必须尊重服务器上别人的修改。
        let base = fields("周会", "");
        let mut cur = base.clone();
        cur.title = "周例会".into();
        let r = merge_update(
            &cur,
            &meta("title", T1, "u1"),
            Some(&base),
            &edit("title", "周会", T2),
            "u2",
        );
        assert_eq!(r.fields.title, "周例会");
        assert!(!r.changed && r.conflicts.is_empty());
    }

    #[test]
    fn same_field_later_edit_wins_and_loser_is_recorded() {
        let base = fields("周会", "");
        let mut cur = base.clone();
        cur.title = "周例会".into();
        // 别人在 T1 改的；我在 T2（更晚）改的 → 我赢。
        let r = merge_update(
            &cur,
            &meta("title", T1, "u1"),
            Some(&base),
            &edit("title", "团队周会", T2),
            "u2",
        );
        assert_eq!(r.fields.title, "团队周会");
        assert!(r.changed && r.merged);
        assert_eq!(
            r.conflicts,
            vec![MergeConflict {
                field: "title".into(),
                kept_value: "团队周会".into(),
                lost_value: "周例会".into(),
                kept_by: "u2".into(),
                lost_by: "u1".into(),
                reason: REASON_CONCURRENT,
            }]
        );
    }

    #[test]
    fn same_field_older_edit_loses_even_if_it_arrives_later() {
        // 离线很久的旧修改（T1）晚到服务器，不应覆盖昨天（T2）的新修改。
        let base = fields("周会", "");
        let mut cur = base.clone();
        cur.title = "周例会".into();
        let r = merge_update(
            &cur,
            &meta("title", T2, "u1"),
            Some(&base),
            &edit("title", "团队周会", T1),
            "u2",
        );
        assert_eq!(r.fields.title, "周例会");
        assert!(!r.changed && r.merged);
        assert_eq!(r.conflicts.len(), 1);
        assert_eq!(r.conflicts[0].lost_value, "团队周会");
        assert_eq!(r.conflicts[0].lost_by, "u2");
        assert_eq!(r.conflicts[0].kept_by, "u1");
    }

    #[test]
    fn notes_edits_on_different_lines_merge_automatically() {
        let base = fields("会", "第一行\n第二行\n第三行\n");
        let mut cur = base.clone();
        cur.notes = "第一行（改）\n第二行\n第三行\n".into(); // 别人改第一行
        let r = merge_update(
            &cur,
            &meta("notes", T1, "u1"),
            Some(&base),
            &edit("notes", "第一行\n第二行\n第三行（我补充）\n", T2),
            "u2",
        );
        assert_eq!(r.fields.notes, "第一行（改）\n第二行\n第三行（我补充）\n");
        assert!(r.changed && r.merged && r.conflicts.is_empty());
    }

    #[test]
    fn notes_edits_on_the_same_line_fall_back_to_lww() {
        let base = fields("会", "第一行\n第二行\n");
        let mut cur = base.clone();
        cur.notes = "第一行 A\n第二行\n".into();
        let r = merge_update(
            &cur,
            &meta("notes", T1, "u1"),
            Some(&base),
            &edit("notes", "第一行 B\n第二行\n", T2),
            "u2",
        );
        assert_eq!(r.fields.notes, "第一行 B\n第二行\n");
        assert_eq!(r.conflicts.len(), 1);
        assert_eq!(r.conflicts[0].lost_value, "第一行 A\n第二行\n");
    }

    #[test]
    fn missing_base_is_treated_as_concurrent() {
        let cur = fields("周例会", "");
        let r = merge_update(
            &cur,
            &meta("title", T3, "u1"),
            None,
            &edit("title", "团队周会", T2),
            "u2",
        );
        assert_eq!(r.fields.title, "周例会", "没有共同祖先时按 LWW，更晚者赢");
        assert_eq!(r.conflicts.len(), 1);
    }

    #[test]
    fn field_without_meta_loses_to_any_timestamp() {
        // 服务器上从未被修改过的字段（meta 为空）：客户端的任何时间戳都更晚。
        let base = fields("周会", "");
        let mut cur = base.clone();
        cur.title = "旧值".into();
        let r = merge_update(
            &cur,
            &Meta::new(),
            Some(&base),
            &edit("title", "新值", T1),
            "u2",
        );
        assert_eq!(r.fields.title, "新值");
        assert_eq!(r.conflicts[0].lost_by, "");
    }

    /// 带 `prev` 的修改（改之前看到的值）。
    fn edit_from(field: &str, prev: &str, v: &str, ts: &str) -> BTreeMap<String, FieldEdit> {
        BTreeMap::from([(
            field.to_string(),
            FieldEdit {
                v: v.into(),
                ts: ts.into(),
                prev: Some(prev.into()),
            },
        )])
    }

    #[test]
    fn consecutive_edits_chain_through_prev_without_conflicts() {
        // 同一台设备连续两次修改：第二条的 prev 就是第一条的结果，不需要任何"同一个人"的特例。
        let base = fields("周会", "a\nb\n");
        let mut cur = base.clone();
        cur.title = "周例会".into(); // 第一条已经落地
        cur.notes = "a\nB\n".into();
        let mut m = meta("title", T2, "u2");
        m.extend(meta("notes", T2, "u2"));
        let mut edits = edit_from("title", "周例会", "团队周会", T3);
        edits.extend(edit_from("notes", "a\nB\n", "a\nBB\n", T3));
        // base_rev 对应的历史版本已经过期（还是最初的 base），但 prev 是准的。
        let r = merge_update(&cur, &m, Some(&base), &edits, "u2");
        assert_eq!(
            (r.fields.title.as_str(), r.fields.notes.as_str()),
            ("团队周会", "a\nBB\n")
        );
        assert!(r.changed && !r.merged && r.conflicts.is_empty());
    }

    #[test]
    fn stale_prev_after_colleague_edit_merges_notes_instead_of_overwriting() {
        // 评审 #1：我连续改了备注两次，第一条（改第 1 行）落地后同事改了第 4 行。
        // 第二条（改第 2 行，与第 4 行之间隔着未改的行）的 prev 是我第一条的结果；服务端现值多了同事的修改，必须按行合并，不能整段覆盖。
        let cur = fields("周会", "A1\nl2\nl3\nC4\n");
        let r = merge_update(
            &cur,
            &meta("notes", T2, "u1"),
            None,
            &edit_from("notes", "A1\nl2\nl3\nl4\n", "A1\nA2\nl3\nl4\n", T3),
            "u2",
        );
        assert_eq!(r.fields.notes, "A1\nA2\nl3\nC4\n");
        assert!(r.changed && r.merged && r.conflicts.is_empty());
    }

    #[test]
    fn same_user_on_two_devices_merges_notes_by_line() {
        // 评审 #2：同一个用户的两台设备，各改不同的行；后到的设备时间戳更晚，也不能整段覆盖。
        let base = fields("周会", "l1\nl2\nl3\n");
        let cur = fields("周会", "D1\nl2\nl3\n"); // 设备 1 先到
        let r = merge_update(
            &cur,
            &meta("notes", T1, "u1"),
            Some(&base),
            &edit_from("notes", "l1\nl2\nl3\n", "l1\nl2\nD3\n", T3),
            "u1", // 同一个用户
        );
        assert_eq!(r.fields.notes, "D1\nl2\nD3\n");
        assert!(r.conflicts.is_empty());
    }

    #[test]
    fn same_user_two_devices_same_title_is_a_real_conflict() {
        // 同一个用户的两台设备改了同一个字段：不能静默覆盖，更早的那份进冲突表（用户能找回）。
        let base = fields("周会", "");
        let mut cur = base.clone();
        cur.title = "设备1的标题".into();
        let r = merge_update(
            &cur,
            &meta("title", T1, "u1"),
            Some(&base),
            &edit_from("title", "周会", "设备2的标题", T3),
            "u1",
        );
        assert_eq!(r.fields.title, "设备2的标题");
        assert_eq!(r.conflicts.len(), 1);
        assert_eq!(r.conflicts[0].lost_value, "设备1的标题");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let base = fields("周会", "");
        let r = merge_update(
            &base,
            &Meta::new(),
            Some(&base),
            &edit("bogus", "x", T1),
            "u",
        );
        assert!(!r.changed && r.conflicts.is_empty());
    }

    #[test]
    fn delete_and_restore_form_an_lww_register() {
        use Lifecycle::*;
        use LifecycleOutcome::*;
        assert_eq!(lifecycle(false, None, Delete, T1), Apply);
        assert_eq!(lifecycle(true, Some(T1), Delete, T2), Noop);
        assert_eq!(lifecycle(true, Some(T2), Restore, T3), Apply);
        // 恢复比删除更早 → 被删除压住
        assert_eq!(lifecycle(true, Some(T2), Restore, T1), LostToNewer);
        // 更早的删除遇到更晚的恢复 → 恢复赢
        assert_eq!(lifecycle(false, Some(T2), Delete, T1), LostToNewer);
        assert_eq!(lifecycle(false, None, Restore, T1), Noop);
    }

    /// 收敛性：两个副本以相反顺序应用同一对并发修改，应得到相同结果。
    #[test]
    fn merge_is_order_independent_for_concurrent_field_edits() {
        let base = fields("周会", "a\nb\nc\n");
        let ea = {
            let mut e = edit("title", "A 标题", T1);
            e.extend(edit("notes", "a1\nb\nc\n", T1));
            e
        };
        let eb = {
            let mut e = edit("title", "B 标题", T2);
            e.extend(edit("notes", "a\nb\nc1\n", T2));
            e
        };
        let run = |first: (&BTreeMap<String, FieldEdit>, &str),
                   second: (&BTreeMap<String, FieldEdit>, &str)| {
            let m1 = merge_update(&base, &Meta::new(), Some(&base), first.0, first.1);
            // 第二个操作的基准仍是 base，现值是第一个应用后的结果
            let m2 = merge_update(&m1.fields, &m1.meta, Some(&base), second.0, second.1);
            m2.fields
        };
        assert_eq!(run((&ea, "ua"), (&eb, "ub")), run((&eb, "ub"), (&ea, "ua")));
    }
}
