import type { Connection, History, Item } from "../api";
import { colorName } from "../api";

export const ACTION_LABEL: Record<string, string> = {
  create: "创建",
  update: "修改",
  status: "状态",
  delete: "删除",
  restore: "恢复",
  visibility: "可见范围",
};

export const VISIBILITY_LABEL = { private: "仅自己可见", shared: "所有人可见" } as const;

export const TOOL_LABEL: Record<string, string> = {
  daygrid_get_context: "读取上下文",
  daygrid_list_items: "查询事项",
  daygrid_list_reminders: "查询提醒",
  daygrid_get_item: "读取事项",
  daygrid_month_overview: "读取概览",
  daygrid_get_proposal: "查询确认结果",
  daygrid_create_item: "创建事项",
  daygrid_update_item: "修改事项",
  daygrid_set_item_status: "切换完成状态",
  daygrid_delete_item: "删除事项",
};

/** 历史记录里的 actor：本人是 "desktop"，团队成员是 "sync:<名字>"，助手是连接 ID。 */
export type ActorKind = "me" | "member" | "assistant";
export const actorKind = (actor: string): ActorKind =>
  actor === "desktop" ? "me" : actor.startsWith("sync:") ? "member" : "assistant";

export function actorLabel(actor: string, connections: Connection[]) {
  switch (actorKind(actor)) {
    case "me":
      return "你";
    case "member":
      return actor.slice("sync:".length) || "团队成员";
    default:
      return connections.find((c) => c.id === actor)?.label ?? "已移除的连接";
  }
}

const CONFLICT_FIELD: Record<string, string> = {
  title: "标题",
  date: "日期",
  notes: "备注",
  color: "颜色",
  status: "状态",
  "*": "整个事项",
};
export const conflictFieldLabel = (field: string) => CONFLICT_FIELD[field] ?? field;

/** 冲突里的值是原始字符串，按字段翻成人话。 */
export function conflictValue(field: string, value: string | null) {
  const v = value ?? "";
  if (field === "color") return colorName(v);
  if (field === "status") return v === "done" ? "已完成" : "未完成";
  if (!v) return "（空）";
  return v.length > 80 ? v.slice(0, 80) + "…" : v;
}

const FIELD: [keyof Item, string][] = [
  ["title", "标题"],
  ["date", "日期"],
  ["notes", "备注"],
  ["color", "颜色"],
  ["status", "状态"],
  ["visibility", "可见范围"],
];
const show = (field: keyof Item, v: unknown) => {
  const s = String(v ?? "");
  if (field === "color") return colorName(s);
  if (field === "status") return s === "done" ? "已完成" : "未完成";
  if (field === "visibility") return s === "shared" ? VISIBILITY_LABEL.shared : VISIBILITY_LABEL.private;
  if (field === "notes") return s ? (s.length > 24 ? s.slice(0, 24) + "…" : s) : "（空）";
  return s;
};

/** 两个版本之间发生变化的字段（修改记录与确认请求共用）。 */
export function diffItems(before: Item | null, after: Item) {
  if (!before) return [];
  return FIELD.filter(([f]) => before[f] !== after[f]).map(([f, label]) => ({
    label,
    from: show(f, before[f]),
    to: show(f, after[f]),
  }));
}
export const changedFields = (h: History) => diffItems(h.before, h.after);
