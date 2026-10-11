import { invoke, isTauri } from "@tauri-apps/api/core";
export type Item = {
  id: string;
  date: string;
  title: string;
  notes: string;
  color: string;
  status: "planned" | "done";
  revision: number;
  created_at: string;
  updated_at: string;
};
export type Context = {
  today: string;
  calendar_timezone: string;
  calendar_generation: string;
  change_sequence: number;
};
export type History = {
  sequence: number;
  item_id: string;
  action: string;
  before: Item | null;
  after: Item;
  actor: string;
  occurred_at: string;
};
export type Connection = {
  id: string;
  label: string;
  permission: "read" | "write";
  state: "active" | "paused" | "revoked";
  config: object;
};
type Envelope<T> = {
  ok: boolean;
  data: T;
  error?: { code: string; message: string; details: Record<string, unknown> };
};
export class ApiError extends Error {
  constructor(
    public code: string,
    message: string,
    public details: Record<string, unknown> = {},
  ) {
    super(message);
  }
}
export const preview = !isTauri();
export const colors = [
  ["blue", "蓝色"],
  ["sage", "绿色"],
  ["amber", "橙色"],
  ["rose", "红色"],
  ["violet", "紫色"],
  ["gray", "灰色"],
] as const;
export const key = () => {
  // getRandomValues also works in WebKit custom-scheme contexts where
  // randomUUID may not be exposed as a secure-context-only API.
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join(
    "",
  );
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
};
export async function call<T>(method: string, args: object = {}): Promise<T> {
  const response: Envelope<T> = preview
    ? ((await demo(method, args)) as Envelope<T>)
    : await invoke("calendar_call", { method, args });
  if (!response.ok)
    throw new ApiError(
      response.error!.code,
      response.error!.message,
      response.error!.details,
    );
  return response.data;
}

// Browser-only visual preview. Never silently masquerade as persistent desktop storage.
const today = new Date().toLocaleDateString("en-CA", {
  timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone,
});
const generation = preview ? key() : "";
let sequence = 0;
const items: Item[] = [];
const deleted = new Set<string>();
const history: History[] = [];
async function demo(method: string, raw: object): Promise<Envelope<unknown>> {
  const a = raw as Record<string, any>;
  const ok = (data: unknown) => ({ ok: true, data });
  if (method === "daygrid_get_context")
    return ok({
      today,
      calendar_timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
      calendar_generation: generation,
      change_sequence: sequence,
    });
  if (method === "daygrid_list_items")
    return ok({
      items: items
        .filter(
          (i) =>
            !deleted.has(i.id) &&
            i.date >= a.from_date &&
            i.date <= a.to_date &&
            (!a.query || (i.title + " " + i.notes).includes(a.query)),
        )
        .sort(
          (a, b) =>
            a.date.localeCompare(b.date) ||
            Number(a.status === "done") - Number(b.status === "done") ||
            a.created_at.localeCompare(b.created_at) ||
            a.id.localeCompare(b.id),
        ),
      next_cursor: null,
      snapshot_sequence: sequence,
    });
  if (method === "desktop_history") return ok({ history });
  if (method === "desktop_connections") return ok({ connections: [] });
  if (method.includes("connection"))
    return {
      ok: false,
      data: null,
      error: {
        code: "PREVIEW_ONLY",
        message: "浏览器仅供界面预览，请在桌面应用中建立真实 MCP 连接。",
        details: {},
      },
    };
  let item = items.find((i) => i.id === a.item_id);
  const before = item ? { ...item } : null;
  const now = new Date().toISOString();
  let action = "";
  if (method === "daygrid_create_item") {
    item = {
      id: key(),
      date: a.date,
      title: a.title.trim(),
      notes: a.notes ?? "",
      color: a.color ?? "blue",
      status: "planned",
      revision: 1,
      created_at: now,
      updated_at: now,
    };
    items.push(item);
    action = "create";
  } else if (item) {
    if (item.revision !== a.expected_revision)
      return {
        ok: false,
        data: null,
        error: {
          code: "REVISION_CONFLICT",
          message: "事项已被修改，草稿已保留。",
          details: { current_item: item },
        },
      };
    if (method === "daygrid_update_item") {
      Object.assign(item, a.patch);
      action = "update";
    }
    if (method === "daygrid_set_item_status") {
      item.status = a.status;
      action = "status";
    }
    if (method === "desktop_delete") {
      deleted.add(item.id);
      action = "delete";
    }
    if (method === "desktop_restore") {
      deleted.delete(item.id);
      action = "restore";
    }
    item.revision++;
    item.updated_at = now;
  }
  if (!item || !action)
    return {
      ok: false,
      data: null,
      error: { code: "NOT_FOUND", message: "事项不存在。", details: {} },
    };
  history.unshift({
    sequence: ++sequence,
    item_id: item.id,
    action,
    before,
    after: { ...item },
    actor: "desktop",
    occurred_at: now,
  });
  return ok({ item, changed: true });
}
