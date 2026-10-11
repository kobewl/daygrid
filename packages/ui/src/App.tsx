import { useCallback, useEffect, useRef, useState } from "react";
import {
  ApiError,
  call,
  colors,
  key,
  preview,
  type Item,
  type Context,
  type History,
  type Connection,
} from "./api";

type View = "calendar" | "history" | "connections" | "settings";
type Draft = {
  id?: string;
  date: string;
  title: string;
  notes: string;
  color: string;
  revision?: number;
  generation: string;
  requestKey: string;
};
const localDate = (d: Date) =>
  `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
const parse = (s: string) =>
  new Date(
    Number(s.slice(0, 4)),
    Number(s.slice(5, 7)) - 1,
    Number(s.slice(8, 10)),
    12,
  );
const monthKey = (s: string) => s.slice(0, 7);
const colorName = (s: string) => colors.find((c) => c[0] === s)?.[1] ?? s;
const actions: Record<string, string> = {
  create: "添加",
  update: "编辑",
  status: "切换完成状态",
  delete: "删除",
  restore: "恢复",
};
function monthDays(month: string) {
  const d = parse(month + "-01");
  d.setDate(1 - ((d.getDay() + 6) % 7));
  return Array.from({ length: 42 }, (_, i) => {
    const n = new Date(d);
    n.setDate(d.getDate() + i);
    return localDate(n);
  });
}

export default function App() {
  const [context, setContext] = useState<Context>();
  const [selected, setSelected] = useState("");
  const [month, setMonth] = useState("");
  const [items, setItems] = useState<Item[]>([]);
  const [view, setView] = useState<View>("calendar");
  const [query, setQuery] = useState("");
  const [draft, setDraft] = useState<Draft>();
  const [original, setOriginal] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [formError, setFormError] = useState("");
  const [conflict, setConflict] = useState<Item>();
  const [toast, setToast] = useState("");
  const [history, setHistory] = useState<History[]>([]);
  const [connections, setConnections] = useState<Connection[]>([]);
  const [label, setLabel] = useState("我的助手");
  const [permission, setPermission] = useState<"read" | "write">("read");
  const [drawer, setDrawer] = useState(false);
  const [lastColor, setLastColor] = useState("blue");
  const epoch = useRef(0);
  const busy = useRef(false);
  const search = useRef<HTMLInputElement>(null);
  const focusDate = useRef(false);
  const notice = (message: string) => setToast(message);
  useEffect(() => {
    if (!toast) return;
    const timer = setTimeout(() => setToast(""), 4000);
    return () => clearTimeout(timer);
  }, [toast]);
  useEffect(() => {
    call<Context>("daygrid_get_context")
      .then((c) => {
        setContext(c);
        setSelected(c.today);
        setMonth(monthKey(c.today));
      })
      .catch((e) => setError(e.message));
  }, []);
  const refresh = useCallback(async () => {
    if (!month) return;
    const current = ++epoch.current;
    const days = monthDays(month);
    const all: Item[] = [];
    let cursor: string | null = null;
    do {
      const data: { items: Item[]; next_cursor: string | null } = await call(
        "daygrid_list_items",
        {
          from_date: days[0] < "1900-01-01" ? "1900-01-01" : days[0],
          to_date: days[41] > "2100-12-31" ? "2100-12-31" : days[41],
          limit: 100,
          ...(query ? { query } : {}),
          ...(cursor ? { cursor } : {}),
        },
      );
      all.push(...data.items);
      cursor = data.next_cursor;
    } while (cursor);
    if (current === epoch.current) {
      setItems(all);
      setError("");
    }
  }, [month, query]);
  useEffect(() => {
    let cancelled = false;
    const poll = async () => {
      if (busy.current) return;
      busy.current = true;
      try {
        await refresh();
        if (view === "history") {
          const d = await call<{ history: History[] }>("desktop_history");
          if (!cancelled) setHistory(d.history);
        }
        if (view === "connections") {
          const d = await call<{ connections: Connection[] }>(
            "desktop_connections",
          );
          if (!cancelled) setConnections(d.connections);
        }
      } catch (e) {
        if (!cancelled) setError((e as Error).message);
      } finally {
        busy.current = false;
      }
    };
    void poll();
    const timer = setInterval(poll, 750);
    return () => {
      cancelled = true;
      epoch.current++;
      clearInterval(timer);
    };
  }, [refresh, view]);
  useEffect(() => {
    if (focusDate.current) {
      document
        .querySelector<HTMLDivElement>(`[data-date="${selected}"]`)
        ?.focus();
      focusDate.current = false;
    }
  }, [selected, items]);
  const choose = (date: string) => {
    if (date < "1900-01-01" || date > "2100-12-31") return;
    setSelected(date);
    setMonth(monthKey(date));
    setDrawer(true);
    setView("calendar");
  };
  const newItem = () => {
    if (!context || draft) return;
    const d: Draft = {
      date: selected || context.today,
      title: "",
      notes: "",
      color: lastColor,
      generation: context.calendar_generation,
      requestKey: key(),
    };
    setDraft(d);
    setOriginal(JSON.stringify(d));
    setFormError("");
    setConflict(undefined);
  };
  const edit = async (item: Item) => {
    try {
      const latest = preview
        ? item
        : (await call<{ item: Item }>("daygrid_get_item", { item_id: item.id }))
            .item;
      const d: Draft = {
        id: latest.id,
        date: latest.date,
        title: latest.title,
        notes: latest.notes,
        color: latest.color,
        revision: latest.revision,
        generation: context!.calendar_generation,
        requestKey: key(),
      };
      setDraft(d);
      setOriginal(JSON.stringify(d));
      setFormError("");
      setConflict(undefined);
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const close = () => {
    if (saving) return;
    if (
      draft &&
      JSON.stringify(draft) !== original &&
      !window.confirm("有未保存的修改，确定放弃吗？")
    )
      return;
    setDraft(undefined);
    setConflict(undefined);
  };
  useEffect(() => {
    const listener = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (draft) {
          e.preventDefault();
          close();
        } else setDrawer(false);
      }
      if (e.key === "Tab" && draft) {
        const controls = Array.from(
          document.querySelectorAll<HTMLElement>(
            ".modal button:not(:disabled),.modal input:not(:disabled),.modal textarea:not(:disabled)",
          ),
        );
        const first = controls[0],
          last = controls[controls.length - 1];
        if (e.shiftKey && document.activeElement === first) {
          e.preventDefault();
          last?.focus();
        } else if (!e.shiftKey && document.activeElement === last) {
          e.preventDefault();
          first?.focus();
        }
      }
      if (e.metaKey || e.ctrlKey) {
        if (e.key.toLowerCase() === "k" && !draft) {
          e.preventDefault();
          search.current?.focus();
        }
        if (e.key.toLowerCase() === "n") {
          e.preventDefault();
          newItem();
        }
        if (e.key === "Enter" && draft) {
          e.preventDefault();
          document
            .querySelector<HTMLFormElement>("#editor-form")
            ?.requestSubmit();
        }
      }
    };
    document.addEventListener("keydown", listener);
    return () => document.removeEventListener("keydown", listener);
  });
  const change = (field: keyof Draft, value: string) =>
    setDraft((d) => (d ? { ...d, [field]: value, requestKey: key() } : d));
  const save = async () => {
    if (!draft || saving) return;
    const title = draft.title.trim();
    if (!title || [...title].length > 200 || [...draft.notes].length > 10000) {
      setFormError("标题需要 1–200 个字符，备注最多 10000 个字符。");
      return;
    }
    setSaving(true);
    setFormError("");
    try {
      const patch = {
        date: draft.date,
        title,
        notes: draft.notes,
        color: draft.color,
      };
      const args = {
        idempotency_key: draft.requestKey,
        calendar_generation: draft.generation,
        ...(draft.id
          ? { item_id: draft.id, expected_revision: draft.revision, patch }
          : patch),
      };
      await call(
        draft.id ? "daygrid_update_item" : "daygrid_create_item",
        args,
      );
      setLastColor(draft.color);
      choose(draft.date);
      setDraft(undefined);
      await refresh();
      notice(preview ? "已更新预览，刷新后恢复" : "已保存");
    } catch (e) {
      const a = e as ApiError;
      setFormError(a.message);
      if (a.code === "REVISION_CONFLICT")
        setConflict(a.details.current_item as Item);
    } finally {
      setSaving(false);
    }
  };
  const status = async (i: Item) => {
    try {
      await call("daygrid_set_item_status", {
        item_id: i.id,
        expected_revision: i.revision,
        calendar_generation: context!.calendar_generation,
        idempotency_key: key(),
        status: i.status === "done" ? "planned" : "done",
      });
      await refresh();
      notice("已更新完成状态");
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const remove = async () => {
    if (
      !draft?.id ||
      !window.confirm(`删除“${draft.title}”？可从修改记录恢复。`)
    )
      return;
    setSaving(true);
    try {
      await call("desktop_delete", {
        item_id: draft.id,
        expected_revision: draft.revision,
        calendar_generation: draft.generation,
        idempotency_key: key(),
      });
      setDraft(undefined);
      await refresh();
      notice("已删除，可从修改记录恢复");
    } catch (e) {
      setFormError((e as Error).message);
    } finally {
      setSaving(false);
    }
  };
  const restore = async (h: History) => {
    try {
      await call("desktop_restore", {
        item_id: h.item_id,
        expected_revision: h.after.revision,
        calendar_generation: context!.calendar_generation,
        idempotency_key: key(),
      });
      setHistory(
        (await call<{ history: History[] }>("desktop_history")).history,
      );
      await refresh();
      notice("事项已恢复");
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const createConnection = async () => {
    try {
      await call("desktop_create_connection", { label, permission });
      setConnections(
        (await call<{ connections: Connection[] }>("desktop_connections"))
          .connections,
      );
      notice("连接已建立，复制配置到你的 MCP 客户端");
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const connectionState = async (c: Connection, state: string) => {
    if (
      state === "revoked" &&
      !window.confirm("撤销后此连接将无法恢复，确定撤销吗？")
    )
      return;
    try {
      await call("desktop_set_connection", { id: c.id, state });
      setConnections(
        (await call<{ connections: Connection[] }>("desktop_connections"))
          .connections,
      );
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const rows = items.filter((i) => i.date === selected);
  const days = month ? monthDays(month) : [];
  return (
    <div className="app">
      <aside className="sidebar">
        <div className="logo">
          <span className="logo-mark">▦</span>日格
        </div>
        <nav aria-label="主导航">
          {(
            [
              ["calendar", "▦", "日历"],
              ["history", "↶", "修改记录"],
              ["connections", "⌁", "助手连接"],
              ["settings", "⚙", "设置"],
            ] as const
          ).map(([v, icon, text]) => (
            <button
              key={v}
              className={view === v ? "active" : ""}
              onClick={() => setView(v)}
            >
              <span aria-hidden>{icon}</span>
              {text}
            </button>
          ))}
        </nav>
        <div className="sidebar-foot">
          <span className={`dot ${error ? "offline" : ""}`} />
          {preview ? "浏览器预览" : "本地日历"}
          <small>
            {preview ? "示例仅保留在当前页面" : "开发预览版 · 0.1.0"}
          </small>
        </div>
      </aside>
      <main>
        <header>
          <strong>把每一天，安排清楚。</strong>
          <div className="top-actions">
            <label className="search">
              <span aria-hidden>⌕</span>
              <input
                ref={search}
                type="search"
                aria-label="搜索当前月事项"
                placeholder="搜索当前月事项"
                value={query}
                onChange={(e) => {
                  setQuery(e.target.value);
                  setView("calendar");
                }}
              />
              <kbd>⌘ K</kbd>
            </label>
            <button className="primary" onClick={newItem} disabled={!context}>
              ＋ 添加事项
            </button>
          </div>
        </header>
        {preview && (
          <div className="banner preview">
            浏览器界面预览，数据不会持久保存。真实日历和 MCP 请使用桌面应用。
          </div>
        )}
        {error && (
          <div className="banner error" role="alert">
            {error}
            <button
              onClick={() => {
                call<Context>("daygrid_get_context")
                  .then((c) => {
                    setContext(c);
                    if (!month) {
                      setMonth(monthKey(c.today));
                      setSelected(c.today);
                    }
                    return refresh();
                  })
                  .catch((e) => setError(e.message));
              }}
            >
              重试
            </button>
          </div>
        )}
        <div className="content">
          {view === "calendar" && (
            <div className="calendar-layout">
              <section className="calendar-area">
                <div className="toolbar">
                  <div className="month-title">
                    <h1>
                      {month
                        ? `${Number(month.slice(0, 4))} 年 ${Number(month.slice(5))} 月`
                        : "正在连接日历…"}
                    </h1>
                    <small>
                      {items.filter((i) => monthKey(i.date) === month).length}{" "}
                      条安排
                    </small>
                  </div>
                  <div className="month-controls">
                    <button
                      className="secondary"
                      onClick={() => context && choose(context.today)}
                    >
                      今天
                    </button>
                    {[-1, 1].map((n) => (
                      <button
                        key={n}
                        className="icon-button"
                        aria-label={n < 0 ? "上个月" : "下个月"}
                        onClick={() => {
                          const d = parse(month + "-01");
                          d.setMonth(d.getMonth() + n);
                          choose(localDate(d));
                        }}
                        disabled={!month}
                      >
                        {n < 0 ? "‹" : "›"}
                      </button>
                    ))}
                  </div>
                </div>
                <div className="weekdays" aria-hidden>
                  {["周一", "周二", "周三", "周四", "周五", "周六", "周日"].map(
                    (d) => (
                      <span key={d}>{d}</span>
                    ),
                  )}
                </div>
                <div className="calendar" role="grid" aria-label="月历">
                  {Array.from({ length: 6 }, (_, week) => (
                    <div role="row" className="calendar-row" key={week}>
                      {days.slice(week * 7, week * 7 + 7).map((date) => {
                        const list = items.filter((i) => i.date === date);
                        return (
                          <div
                            role="gridcell"
                            tabIndex={date === selected ? 0 : -1}
                            data-date={date}
                            key={date}
                            aria-selected={date === selected}
                            aria-label={`${date}，${list.length} 条事项`}
                            className={`day ${monthKey(date) !== month ? "other" : ""} ${date === selected ? "selected" : ""}`}
                            onClick={() => choose(date)}
                            onKeyDown={(e) => {
                              if (e.target !== e.currentTarget) return;
                              const step: Record<string, number> = {
                                ArrowLeft: -1,
                                ArrowRight: 1,
                                ArrowUp: -7,
                                ArrowDown: 7,
                              };
                              if (step[e.key]) {
                                e.preventDefault();
                                const d = parse(date);
                                d.setDate(d.getDate() + step[e.key]);
                                focusDate.current = true;
                                choose(localDate(d));
                              }
                              if (e.key === "Enter") {
                                e.preventDefault();
                                choose(date);
                                document
                                  .querySelector<HTMLButtonElement>("#day-add")
                                  ?.focus();
                              }
                            }}
                          >
                            <span
                              className={`day-number ${date === context?.today ? "today" : ""}`}
                            >
                              {Number(date.slice(8))}
                            </span>
                            {list.slice(0, 3).map((i) => (
                              <button
                                key={i.id}
                                className={`pill c-${i.color} ${i.status === "done" ? "done" : ""}`}
                                aria-label={`${i.title}，${colorName(i.color)}，${i.status === "done" ? "已完成" : "未完成"}`}
                                onClick={(e) => {
                                  e.stopPropagation();
                                  choose(date);
                                  void edit(i);
                                }}
                              >
                                {i.status === "done" ? "✓ " : ""}
                                {i.title}
                              </button>
                            ))}
                            {list.length > 3 && (
                              <span className="more">
                                还有 {list.length - 3} 项
                              </span>
                            )}
                          </div>
                        );
                      })}
                    </div>
                  ))}
                </div>
                <div className="legend">
                  {colors.map(([c, name]) => (
                    <span key={c}>
                      <i className={`c-${c}`} />
                      {name}
                    </span>
                  ))}
                </div>
                <p className="footer-note">
                  选择一个日期，记下这一天的安排。
                  {query && " 当前仅显示匹配事项。"}
                </p>
              </section>
              <aside
                className={`date-panel ${drawer ? "opened" : ""}`}
                aria-label="所选日期事项"
              >
                <div className="panel-head">
                  <div className="eyebrow">
                    {selected === context?.today ? "今天" : "所选日期"} ·{" "}
                    {selected.slice(0, 4)} 年
                    <button
                      className="drawer-close"
                      aria-label="关闭日期面板"
                      onClick={() => {
                        setDrawer(false);
                        document
                          .querySelector<HTMLDivElement>(
                            `[data-date="${selected}"]`,
                          )
                          ?.focus();
                      }}
                    >
                      ×
                    </button>
                  </div>
                  <h2>
                    {Number(selected.slice(5, 7))} 月{" "}
                    {Number(selected.slice(8))} 日
                  </h2>
                  <small>
                    {selected &&
                      parse(selected).toLocaleDateString("zh-CN", {
                        weekday: "long",
                      })}
                  </small>
                </div>
                <div className="panel-body">
                  <div className="section-label">
                    <span>当天事项</span>
                    <span>{rows.length} 项</span>
                  </div>
                  {rows.length === 0 && (
                    <p className="empty">
                      这一天还没有安排。
                      <br />
                      记下第一件想做的事吧。
                    </p>
                  )}
                  {rows.map((i) => (
                    <div
                      className={`item-card c-${i.color} ${i.status === "done" ? "done" : ""}`}
                      key={i.id}
                    >
                      <button
                        className="check"
                        aria-label={`${i.status === "done" ? "恢复未完成" : "完成"}：${i.title}`}
                        onClick={() => void status(i)}
                      >
                        {i.status === "done" ? "✓" : ""}
                      </button>
                      <button
                        className="item-open"
                        onClick={() => void edit(i)}
                      >
                        <span>{i.title}</span>
                        {i.notes && <small>{i.notes}</small>}
                      </button>
                    </div>
                  ))}
                  <button
                    className="add-line"
                    id="day-add"
                    onClick={newItem}
                    disabled={!context}
                  >
                    ＋ 添加一条事项
                  </button>
                </div>
                <div className="panel-foot">
                  事项颜色由你选择。
                  <br />
                  手动与助手修改，都会记入历史。
                </div>
              </aside>
            </div>
          )}
          {view === "history" && (
            <section className="wide">
              <h1>修改记录</h1>
              <p className="intro">
                手动与助手修改都在这里，可查看前后内容并恢复删除的事项。
              </p>
              <div className="surface">
                {!history.length && <p className="empty">还没有修改记录。</p>}
                {history.map((h, index) => (
                  <article className="history-row" key={h.sequence}>
                    <div>
                      <small>
                        {new Date(h.occurred_at).toLocaleString("zh-CN", {
                          timeZone: context?.calendar_timezone,
                        })}
                      </small>
                      <span className="badge">
                        {h.actor === "desktop" ? "手动" : "助手"}
                      </span>
                    </div>
                    <strong>
                      {actions[h.action]} · {h.after.title}
                    </strong>
                    <details>
                      <summary>查看前后内容</summary>
                      <div className="diff">
                        <div>
                          <small>修改前</small>
                          <p>
                            {h.before
                              ? `${h.before.date} · ${h.before.title} · ${colorName(h.before.color)} · ${h.before.status === "done" ? "已完成" : "未完成"}`
                              : "尚未创建"}
                          </p>
                          <pre>{h.before?.notes}</pre>
                        </div>
                        <div>
                          <small>
                            修改后{h.action === "delete" ? "（已删除）" : ""}
                          </small>
                          <p>
                            {h.after.date} · {h.after.title} ·{" "}
                            {colorName(h.after.color)} ·{" "}
                            {h.after.status === "done" ? "已完成" : "未完成"}
                          </p>
                          <pre>{h.after.notes}</pre>
                        </div>
                      </div>
                    </details>
                    {h.action === "delete" &&
                      !history
                        .slice(0, index)
                        .some((x) => x.item_id === h.item_id) && (
                        <button
                          className="secondary"
                          onClick={() => void restore(h)}
                        >
                          恢复事项
                        </button>
                      )}
                  </article>
                ))}
              </div>
              <small>当前显示最近 100 条记录，完整历史仍保存在本地。</small>
            </section>
          )}
          {view === "connections" && (
            <section className="wide">
              <h1>助手连接</h1>
              <p className="intro">通过 MCP 让外部助手读取或修改你的日历。</p>
              <div className="surface">
                <h2>建立连接</h2>
                <p className="callout">
                  助手读取的事项可能发送到其模型服务。只读可查询；读写还可创建、编辑和完成事项。你可以随时暂停或撤销。
                </p>
                <div className="connection-form">
                  <label>
                    连接名称
                    <input
                      value={label}
                      onChange={(e) => setLabel(e.target.value)}
                    />
                  </label>
                  <label>
                    权限
                    <select
                      value={permission}
                      onChange={(e) =>
                        setPermission(e.target.value as "read" | "write")
                      }
                    >
                      <option value="read">只读</option>
                      <option value="write">读写</option>
                    </select>
                  </label>
                  <button
                    className="primary"
                    onClick={() => void createConnection()}
                  >
                    建立连接
                  </button>
                </div>
              </div>
              {connections.map((c) => (
                <div className="surface" key={c.id}>
                  <div className="surface-head">
                    <h2>{c.label}</h2>
                    <span className="badge">
                      {c.state === "active"
                        ? "已启用"
                        : c.state === "paused"
                          ? "已暂停"
                          : "已撤销"}{" "}
                      · {c.permission === "read" ? "只读" : "读写"}
                    </span>
                  </div>
                  {c.state !== "revoked" && (
                    <>
                      <label className="field">
                        MCP 客户端配置
                        <textarea
                          readOnly
                          aria-label={`${c.label} MCP 配置`}
                          value={JSON.stringify(c.config, null, 2)}
                        />
                      </label>
                      <div className="actions">
                        <button
                          className="secondary"
                          onClick={() => {
                            navigator.clipboard
                              .writeText(JSON.stringify(c.config, null, 2))
                              .then(() => notice("配置已复制"))
                              .catch(() => notice("请选中上方配置并复制"));
                          }}
                        >
                          复制配置
                        </button>
                        <button
                          className="secondary"
                          onClick={() =>
                            void connectionState(
                              c,
                              c.state === "active" ? "paused" : "active",
                            )
                          }
                        >
                          {c.state === "active" ? "暂停连接" : "恢复连接"}
                        </button>
                        <button
                          className="danger"
                          onClick={() => void connectionState(c, "revoked")}
                        >
                          撤销连接
                        </button>
                      </div>
                    </>
                  )}
                </div>
              ))}
              <p className="footer-note">
                本预览提供 6 个 MCP
                工具。删除与批量修改将在应用确认流程完成后开放。
              </p>
            </section>
          )}
          {view === "settings" && (
            <section className="wide">
              <h1>设置</h1>
              <p className="intro">
                数据保存在本机，软件不需要账号或模型密钥。
              </p>
              <div className="surface">
                <h2>日历时区</h2>
                <p className="intro">{context?.calendar_timezone}</p>
                <small>
                  用于定义今天与显示修改时间；纯日期事项不会随时区移动。此预览暂不提供时区修改。
                </small>
              </div>
              <div className="surface">
                <h2>开发预览版</h2>
                <p className="intro">
                  已提供本地日历、修改记录、软删除恢复和 MCP 单条读写。
                </p>
                <small>
                  备份恢复、10
                  秒撤销、批量确认、全局搜索与筛选、签名和自动更新仍待开发。
                </small>
              </div>
            </section>
          )}
        </div>
      </main>
      {draft && (
        <div className="modal-backdrop">
          <section
            className="modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="editor-title"
          >
            <form
              id="editor-form"
              onSubmit={(e) => {
                e.preventDefault();
                void save();
              }}
            >
              <div className="modal-body">
                <div className="surface-head">
                  <h2 id="editor-title">
                    {draft.id ? "编辑事项" : "添加事项"}
                  </h2>
                  <button
                    type="button"
                    aria-label="关闭编辑器"
                    className="icon-button"
                    onClick={close}
                    disabled={saving}
                  >
                    ×
                  </button>
                </div>
                <label className="field">
                  标题
                  <input
                    autoFocus
                    required
                    value={draft.title}
                    onChange={(e) => change("title", e.target.value)}
                    placeholder="这一天，想做些什么？"
                    disabled={saving}
                  />
                </label>
                <label className="field">
                  日期
                  <input
                    type="date"
                    required
                    min="1900-01-01"
                    max="2100-12-31"
                    value={draft.date}
                    onChange={(e) => change("date", e.target.value)}
                    disabled={saving}
                  />
                </label>
                <fieldset>
                  <legend>事项颜色</legend>
                  <div className="swatches">
                    {colors.map(([c, name]) => (
                      <label className={`swatch c-${c}`} key={c}>
                        <input
                          type="radio"
                          name="color"
                          aria-label={name}
                          checked={draft.color === c}
                          onChange={() => change("color", c)}
                          disabled={saving}
                        />
                        <span aria-hidden>{draft.color === c ? "✓" : ""}</span>
                      </label>
                    ))}
                  </div>
                  <small>已选择{colorName(draft.color)}</small>
                </fieldset>
                <label className="field">
                  备注
                  <textarea
                    value={draft.notes}
                    onChange={(e) => change("notes", e.target.value)}
                    placeholder="补充一些细节（可选）"
                    disabled={saving}
                  />
                </label>
                {formError && (
                  <p className="form-error" role="alert">
                    {formError}
                  </p>
                )}
                {conflict && (
                  <div className="conflict">
                    <p>
                      最新内容：{conflict.title} · {conflict.date} ·{" "}
                      {colorName(conflict.color)}
                    </p>
                    <pre>{conflict.notes}</pre>
                    <button
                      type="button"
                      className="secondary"
                      onClick={() => {
                        const d = {
                          ...draft,
                          date: conflict.date,
                          title: conflict.title,
                          notes: conflict.notes,
                          color: conflict.color,
                          revision: conflict.revision,
                          requestKey: key(),
                        };
                        setDraft(d);
                        setOriginal(JSON.stringify(d));
                        setConflict(undefined);
                        setFormError("");
                      }}
                    >
                      采用最新内容
                    </button>
                    <button
                      type="button"
                      className="secondary"
                      onClick={() => {
                        setDraft({
                          ...draft,
                          revision: conflict.revision,
                          requestKey: key(),
                        });
                        setConflict(undefined);
                        setFormError("已保留草稿，请核对后再次保存。");
                      }}
                    >
                      保留草稿，重新核对
                    </button>
                  </div>
                )}
              </div>
              <div className="modal-footer">
                {draft.id ? (
                  <button
                    type="button"
                    className="danger"
                    onClick={() => void remove()}
                    disabled={saving}
                  >
                    删除事项
                  </button>
                ) : (
                  <small>{preview ? "预览不持久保存" : "保存到本地日历"}</small>
                )}
                <div className="actions">
                  <button
                    className="secondary"
                    type="button"
                    onClick={close}
                    disabled={saving}
                  >
                    取消
                  </button>
                  <button
                    className="primary"
                    type="submit"
                    disabled={saving || !!conflict}
                  >
                    {saving ? "保存中…" : "保存事项"}
                  </button>
                </div>
              </div>
            </form>
          </section>
        </div>
      )}
      {toast && (
        <div className="toast" role="status">
          {toast}
        </div>
      )}
    </div>
  );
}
