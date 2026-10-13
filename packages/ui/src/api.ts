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
export type DayMark = { date: string; kind: "rest" | "work" };
export type ApprovalMode = "deletes" | "all_writes";
export type Context = {
  today: string;
  weekday: string;
  calendar_timezone: string;
  calendar_generation: string;
  change_sequence: number;
  approval_mode: ApprovalMode;
  service_version: string;
};
export type History = {
  sequence: number;
  operation_id: string;
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
  created_at: string | null;
  last_seen_at: string | null;
  calls: number;
  config: object;
};
export type ProposalKind = "create" | "update" | "status" | "delete";
export type Proposal = {
  id: string;
  state: "pending" | "approved" | "rejected" | "expired" | "failed";
  summary: string;
  created_at: string;
  expires_at: string;
  decided_at: string | null;
  actor: string;
  connection_label: string | null;
  method: string;
  preview: {
    kind: ProposalKind;
    before: Item | null;
    after: Item | null;
    reason?: string;
  };
  error?: { code: string; message: string };
};
export type Activity = {
  sequence: number;
  occurred_at: string;
  connection_id: string;
  connection_label: string | null;
  tool: string;
  outcome: "ok" | "error";
  error_code: string | null;
  duration_ms: number;
  summary: string;
};
export type Pulse = {
  change_sequence: number;
  pending_proposals: number;
  proposal_rev: number;
  activity_sequence: number;
};
export type Diagnostics = {
  service_version: string;
  ipc_version: number;
  schema_version: number;
  uptime_seconds: number;
  database_bytes: number;
  items_active: number;
  items_deleted: number;
  history_entries: number;
  activity_entries: number;
  connections_active: number;
  timezone: string;
  backups: { name: string; bytes: number; modified_at: string }[];
  data_dir: string;
  log_dir: string;
};
export type ClientStatus = {
  id: string;
  name: string;
  kind: "file" | "command";
  config_path: string;
  detected: boolean;
  installed: boolean;
  configured_for_other_connection: boolean;
  manual_command: string | null;
};
export type AppInfo = {
  app_version: string;
  platform: string;
  arch: string;
  data_dir: string;
  log_dir: string;
  mcp_path: string;
};

type Envelope<T> = {
  ok: boolean;
  data: T;
  error?: {
    code: string;
    message: string;
    details: Record<string, unknown>;
    hint?: string;
  };
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
export const colorName = (s: string) =>
  colors.find((c) => c[0] === s)?.[1] ?? s;

export const key = () => {
  // getRandomValues 在 WebKit 的自定义协议页面里也可用，randomUUID 不一定。
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
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

/** 有新的待确认请求而窗口不在前台时让 Dock 图标跳动（仅桌面应用）。 */
export function requestAttention() {
  if (!preview) void invoke("app_attention").catch(() => undefined);
}

// ───────────────────────── 浏览器预览（仅用于看界面） ─────────────────────────
// 内存中的示例数据，刷新即清空；永远不会冒充桌面应用的持久存储，界面顶部有明确提示。
const pad = (n: number) => String(n).padStart(2, "0");
const iso = (d: Date) =>
  `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
const today = iso(new Date());
const shift = (days: number) => {
  const d = new Date();
  d.setHours(12);
  d.setDate(d.getDate() + days);
  return iso(d);
};
const ago = (minutes: number) =>
  new Date(Date.now() - minutes * 60_000).toISOString();
const generation = preview ? key() : "";
let sequence = 0;
let activitySeq = 6;
let proposalRev = 1;
let approvalMode: ApprovalMode = "deletes";

const mk = (
  date: string,
  title: string,
  color: string,
  notes = "",
  status: Item["status"] = "planned",
): Item => ({
  id: key(),
  date,
  title,
  notes,
  color,
  status,
  revision: 1,
  created_at: ago(5000 - items0.length * 7),
  updated_at: ago(5000 - items0.length * 7),
});
const items0: Item[] = [];
if (preview) {
  [
    [-6, "团队周例会", "blue"],
    [-5, "读完一章书", "sage", "《设计心理学》第三章"],
    [-3, "需求评审", "blue", "带上评审清单"],
    [-1, "整理周报", "violet", "", "done"],
    [0, "去公园跑步", "sage", "带水，慢慢跑\n不要超过 5 公里"],
    [0, "安排下周计划", "blue", "把重要的事先记下来"],
    [0, "刷两道算法题", "violet", "LC509 斐波那契数（https://leetcode.cn/problems/fibonacci-number/）；两题都已完成，再接续未完成的 LC740 删除并获得点数（https://leetcode.cn/problems/delete-and-earn/）。\n\n- [x] LC509\n- [ ] LC740\n\n**今天只做一题**，详见 [题解](https://leetcode.cn/)"],
    [0, "给家人打电话", "rose"],
    [2, "旧的周报模板", "gray", "已经被新版取代"],
    [2, "体检预约", "amber", "空腹，早上 9 点"],
    [2, "季度复盘", "violet"],
    [2, "复习英语", "blue"],
    [4, "提交材料", "amber", "附件两份，检查签名"],
    [5, "产品体验走查", "violet"],
    [10, "和朋友吃晚饭", "rose"],
    [12, "周末散步", "sage"],
  ].forEach(([d, t, c, n, s]) =>
    items0.push(
      mk(shift(d as number), t as string, c as string, (n as string) ?? "", (s as Item["status"]) ?? "planned"),
    ),
  );
}
const items: Item[] = items0;
// 休息日 / 工作日手动标记（调休）。演示数据：国庆后的周六补班，仅存内存。
const dayMarks = new Map<string, DayMark["kind"]>();
const trash = new Map<string, Item>();
const history: History[] = [];
const demoId = key();
const connections: Connection[] = preview
  ? [
      {
        id: demoId,
        label: "Cursor（演示）",
        permission: "write",
        state: "active",
        created_at: ago(60 * 48),
        last_seen_at: ago(3),
        calls: 14,
        config: {
          mcpServers: {
            daygrid: {
              command: "/Applications/Daygrid.app/Contents/MacOS/daygrid-mcp",
              args: ["--connection", demoId],
            },
          },
        },
      },
    ]
  : [];
const stale = items.find((i) => i.title === "旧的周报模板");
const proposals: Proposal[] =
  preview && stale
    ? [
        {
          id: key(),
          state: "pending",
          summary: `删除「${stale.title}」· ${stale.date}`,
          created_at: ago(1),
          expires_at: new Date(Date.now() + 9 * 60_000).toISOString(),
          decided_at: null,
          actor: demoId,
          connection_label: "Cursor（演示）",
          method: "daygrid_delete_item",
          preview: {
            kind: "delete",
            before: stale,
            after: null,
            reason: "和「整理周报」重复，且已被新版模板取代",
          },
        },
      ]
    : [];
const activity: Activity[] = preview
  ? [
      [2, "查询 本周，3 条", "daygrid_list_items", 4, "ok"],
      [3, "读取日历上下文", "daygrid_get_context", 2, "ok"],
      [9, "创建「体检预约」· " + shift(2), "daygrid_create_item", 11, "ok"],
      [9, "读取 本月概览，15 条", "daygrid_month_overview", 6, "ok"],
      [
        14,
        "修改失败：事项已被修改，请读取最新内容。",
        "daygrid_update_item",
        5,
        "error",
      ],
      [20, "读取日历上下文", "daygrid_get_context", 2, "ok"],
    ].map((a, i) => ({
      sequence: 6 - i,
      occurred_at: ago(a[0] as number),
      connection_id: demoId,
      connection_label: "Cursor（演示）",
      tool: a[2] as string,
      outcome: a[4] as "ok" | "error",
      error_code: a[4] === "error" ? "REVISION_CONFLICT" : null,
      duration_ms: a[3] as number,
      summary: a[1] as string,
    }))
  : [];
const installed = new Set<string>();

type Fail = { code: string; message: string; details?: object };
const fail = (code: string, message: string, details: object = {}): never => {
  throw { code, message, details } satisfies Fail;
};

async function demo(method: string, raw: object): Promise<Envelope<unknown>> {
  try {
    await new Promise((r) => setTimeout(r, 12));
    // 像真实后端一样返回 JSON 副本，避免界面状态与演示数据共享引用。
    const data = demoHandler(method, structuredClone(raw) as Record<string, any>);
    return { ok: true, data: structuredClone(data) };
  } catch (e) {
    const f = e as Fail;
    if (!f?.code) throw e;
    return {
      ok: false,
      data: null,
      error: { code: f.code, message: f.message, details: structuredClone((f.details ?? {}) as Record<string, unknown>) },
    };
  }
}

function record(
  item: Item,
  before: Item | null,
  action: string,
  actor = "desktop",
  at = new Date().toISOString(),
) {
  history.unshift({
    sequence: ++sequence,
    operation_id: key(),
    item_id: item.id,
    action,
    before,
    after: { ...item },
    actor,
    occurred_at: at,
  });
}
const sorted = (list: Item[]) =>
  [...list].sort(
    (a, b) =>
      a.date.localeCompare(b.date) ||
      Number(a.status === "done") - Number(b.status === "done") ||
      a.created_at.localeCompare(b.created_at) ||
      a.id.localeCompare(b.id),
  );
// 预览用的示例修改记录（含一条已删除、可恢复的事项）。
if (preview) {
  const minutes = (m: number) => new Date(Date.now() - m * 60_000).toISOString();
  for (const item of items) {
    const byAssistant = ["体检预约", "季度复盘"].includes(item.title);
    record({ ...item }, null, "create", byAssistant ? demoId : "desktop", item.created_at);
  }
  const renamed = items.find((i) => i.title === "读完一章书");
  if (renamed) {
    record({ ...renamed, revision: 2 }, { ...renamed, title: "读一章书" }, "update", demoId, minutes(60 * 20));
    renamed.revision = 2;
  }
  const done = items.find((i) => i.status === "done");
  if (done) record({ ...done, revision: 2 }, { ...done, status: "planned" }, "status", "desktop", minutes(60 * 26));
  if (done) done.revision = 2;
  const gone = mk(shift(1), "旧的会议纪要", "gray", "已归档到文档库");
  record({ ...gone }, null, "create", "desktop", minutes(60 * 72));
  gone.revision = 2;
  record({ ...gone }, { ...gone, revision: 1 }, "delete", "desktop", minutes(120));
  trash.set(gone.id, gone);
}
const find = (id: string) =>
  items.find((i) => i.id === id) ?? fail("NOT_FOUND", "事项不存在或生命周期状态不符。");

function demoHandler(method: string, a: Record<string, any>): unknown {
  switch (method) {
    case "daygrid_get_context":
      return {
        today: iso(new Date()),
        weekday: new Date().toLocaleDateString("en-US", { weekday: "long" }),
        calendar_timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
        calendar_generation: generation,
        change_sequence: sequence,
        approval_mode: approvalMode,
        service_version: "preview",
      };
    case "daygrid_list_items":
      return {
        items: sorted(
          items.filter(
            (i) =>
              i.date >= a.from_date &&
              i.date <= a.to_date &&
              (!a.query || (i.title + " " + i.notes).toLowerCase().includes(String(a.query).toLowerCase())),
          ),
        ),
        next_cursor: null,
        snapshot_sequence: sequence,
      };
    case "daygrid_get_item":
      return { item: find(a.item_id) };
    case "daygrid_create_item": {
      const now = new Date().toISOString();
      const item: Item = {
        id: key(),
        date: a.date,
        title: String(a.title).trim(),
        notes: a.notes ?? "",
        color: a.color ?? "blue",
        status: "planned",
        revision: 1,
        created_at: now,
        updated_at: now,
      };
      items.push(item);
      record(item, null, "create");
      return { item, changed: true };
    }
    case "daygrid_update_item":
    case "daygrid_set_item_status":
    case "desktop_delete": {
      const item = find(a.item_id);
      if (item.revision !== a.expected_revision)
        fail("REVISION_CONFLICT", "事项已被修改，草稿已保留，请读取最新内容。", {
          current_revision: item.revision,
          current_item: item,
        });
      const before = { ...item };
      let action = "delete";
      if (method === "daygrid_update_item") {
        Object.assign(item, a.patch);
        action = "update";
      } else if (method === "daygrid_set_item_status") {
        item.status = a.status;
        action = "status";
      }
      item.revision++;
      item.updated_at = new Date().toISOString();
      if (action === "delete") {
        items.splice(items.indexOf(item), 1);
        trash.set(item.id, item);
      }
      record(item, before, action);
      return { item: { ...item }, changed: true };
    }
    case "desktop_restore": {
      const item = trash.get(a.item_id) ?? fail("NOT_FOUND", "事项不存在或生命周期状态不符。");
      const before = { ...item };
      trash.delete(item.id);
      item.revision++;
      items.push(item);
      record(item, before, "restore");
      return { item: { ...item }, changed: true };
    }
    case "desktop_history":
      return { history: history.slice(0, 100) };
    case "desktop_search": {
      const q = String(a.query ?? "").trim().toLowerCase();
      if (!q) fail("VALIDATION_ERROR", "搜索词需要 1–200 个字符。");
      const hits = [...items]
        .sort((x, y) => y.date.localeCompare(x.date))
        .flatMap((item) =>
          item.title.toLowerCase().includes(q)
            ? [{ item, match: "title" }]
            : item.notes.toLowerCase().includes(q)
              ? [{ item, match: "notes" }]
              : [],
        );
      const limit = a.limit ?? 30;
      return { results: hits.slice(0, limit), has_more: hits.length > limit };
    }
    case "desktop_pulse":
      return {
        change_sequence: sequence,
        pending_proposals: proposals.filter((p) => p.state === "pending").length,
        proposal_rev: proposalRev,
        activity_sequence: activitySeq,
      };
    case "desktop_connections":
      return { connections };
    case "daygrid_month_overview": {
      const days = new Map<string, { planned: number; done: number; colors: string[] }>();
      for (const i of items) {
        if (i.date < a.from_date || i.date > a.to_date) continue;
        const d = days.get(i.date) ?? { planned: 0, done: 0, colors: [] };
        d[i.status === "done" ? "done" : "planned"]++;
        if (!d.colors.includes(i.color)) d.colors.push(i.color);
        days.set(i.date, d);
      }
      return {
        from_date: a.from_date,
        to_date: a.to_date,
        total_items: [...days.values()].reduce((n, d) => n + d.planned + d.done, 0),
        days: [...days].sort(([x], [y]) => x.localeCompare(y)).map(([date, d]) => ({ date, ...d })),
      };
    }
    case "desktop_create_connection": {
      const id = key();
      connections.push({
        id,
        label: String(a.label).trim(),
        permission: a.permission,
        state: "active",
        created_at: new Date().toISOString(),
        last_seen_at: null,
        calls: 0,
        config: {
          mcpServers: {
            daygrid: {
              command: "/Applications/Daygrid.app/Contents/MacOS/daygrid-mcp",
              args: ["--connection", id],
            },
          },
        },
      });
      return { id, config: connections[connections.length - 1].config };
    }
    case "desktop_set_connection": {
      const c = connections.find((x) => x.id === a.id);
      if (!c || c.state === "revoked") fail("NOT_FOUND", "连接不存在或已永久撤销。");
      c!.state = a.state;
      return { changed: true };
    }
    case "desktop_activity":
      return { activity };
    case "desktop_proposals":
      return { proposals };
    case "desktop_decide_proposal": {
      const p = proposals.find((x) => x.id === a.proposal_id);
      if (!p || p.state !== "pending")
        fail("PROPOSAL_NOT_PENDING", "该请求已处理或已过期。", { state: p?.state });
      p!.decided_at = new Date().toISOString();
      if (a.decision === "reject") p!.state = "rejected";
      else {
        const item = items.find((i) => i.id === p!.preview.before?.id);
        if (item) {
          const before = { ...item };
          item.revision++;
          items.splice(items.indexOf(item), 1);
          trash.set(item.id, item);
          record(item, before, "delete", p!.actor);
          p!.state = "approved";
        } else p!.state = "failed";
      }
      proposalRev++;
      return { proposal: p };
    }
    case "desktop_list_day_marks":
      return {
        marks: [...dayMarks]
          .filter(([date]) => date >= a.from_date && date <= a.to_date)
          .sort(([x], [y]) => x.localeCompare(y))
          .map(([date, kind]) => ({ date, kind })),
      };
    case "desktop_set_day_mark":
      if (!["rest", "work", "default"].includes(a.kind))
        fail("VALIDATION_ERROR", "日期类型需要是 rest、work 或 default。");
      if (a.kind === "default") dayMarks.delete(a.date);
      else dayMarks.set(a.date, a.kind);
      return { date: a.date, kind: a.kind };
    case "desktop_get_settings":
      return { approval_mode: approvalMode };
    case "desktop_set_settings":
      approvalMode = a.approval_mode;
      return { approval_mode: approvalMode };
    case "desktop_diagnostics":
      return {
        service_version: "preview",
        ipc_version: 1,
        schema_version: 2,
        uptime_seconds: 1234,
        database_bytes: 98304,
        items_active: items.length,
        items_deleted: trash.size,
        history_entries: history.length,
        activity_entries: activity.length,
        connections_active: connections.filter((c) => c.state === "active").length,
        timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
        backups: [
          { name: "calendar-20261010-093000.db", bytes: 98304, modified_at: ago(60 * 24) },
        ],
        data_dir: "~/Library/Application Support/Daygrid",
        log_dir: "~/Library/Application Support/Daygrid/logs",
      } satisfies Diagnostics;
    case "desktop_backup":
      return fail("PREVIEW_ONLY", "浏览器预览不提供备份。");
    case "desktop_reveal":
      return fail("PREVIEW_ONLY", "浏览器预览无法打开本机目录。");
    case "desktop_app_info":
      return {
        app_version: "preview",
        platform: "browser",
        arch: "-",
        data_dir: "~/Library/Application Support/Daygrid",
        log_dir: "~/Library/Application Support/Daygrid/logs",
        mcp_path: "/Applications/Daygrid.app/Contents/MacOS/daygrid-mcp",
      } satisfies AppInfo;
    case "desktop_client_status":
      return {
        clients: [
          ["cursor", "Cursor", "file", ".cursor/mcp.json"],
          ["claude_desktop", "Claude Desktop", "file", "Library/Application Support/Claude/claude_desktop_config.json"],
          ["claude_code", "Claude Code", "command", ".claude.json"],
          ["codex", "Codex", "command", ".codex/config.toml"],
        ].map(([id, name, kind, path]) => ({
          id,
          name,
          kind,
          config_path: `~/${path}`,
          detected: id !== "claude_desktop",
          installed: installed.has(id),
          configured_for_other_connection: false,
          manual_command:
            kind === "command"
              ? id === "codex"
                ? `codex mcp add daygrid -- /Applications/Daygrid.app/Contents/MacOS/daygrid-mcp --connection ${a.connection_id}`
                : `claude mcp add --transport stdio --scope user daygrid -- /Applications/Daygrid.app/Contents/MacOS/daygrid-mcp --connection ${a.connection_id}`
              : null,
        })) as ClientStatus[],
      };
    case "desktop_client_install":
      installed.add(a.client);
      return { client: a.client, name: a.client, config_path: "~/.cursor/mcp.json", backup: null, replaced_existing: false, restart_hint: "预览：未写入任何真实文件。" };
    case "desktop_client_uninstall":
      installed.delete(a.client);
      return { client: a.client, removed: true };
    default:
      return fail("VALIDATION_ERROR", "未实现的操作。");
  }
}
