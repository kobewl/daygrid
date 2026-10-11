import { useCallback, useEffect, useMemo, useState } from "react";
import { call, type Connection, type History } from "../api";
import { Icon } from "../components/icons";
import { Segmented } from "../components/Segmented";
import { useNotify } from "../hooks/useToasts";
import { addDays, dayTitle, toISO } from "../lib/dates";
import { ACTION_LABEL, actorLabel, changedFields } from "../lib/labels";
import { cx } from "../lib/util";

type Filter = "all" | "me" | "assistants";

export function HistoryView({
  changeSequence,
  today,
  onRestore,
  onJump,
}: {
  changeSequence: number;
  today: string;
  onRestore: (itemId: string, revision: number) => Promise<unknown>;
  onJump: (date: string) => void;
}) {
  const notify = useNotify();
  const [history, setHistory] = useState<History[]>();
  const [connections, setConnections] = useState<Connection[]>([]);
  const [filter, setFilter] = useState<Filter>("all");
  const [error, setError] = useState("");

  const load = useCallback(async () => {
    try {
      const [h, c] = await Promise.all([
        call<{ history: History[] }>("desktop_history", { limit: 200 }),
        call<{ connections: Connection[] }>("desktop_connections"),
      ]);
      setHistory(h.history);
      setConnections(c.connections);
      setError("");
    } catch (e) {
      setError((e as Error).message);
    }
  }, []);
  useEffect(() => void load(), [load, changeSequence]);

  // 每个事项只有「最新一条」是删除时，才能恢复。
  const restorable = useMemo(() => {
    const seen = new Set<string>();
    const ok = new Set<number>();
    for (const h of history ?? []) {
      if (seen.has(h.item_id)) continue;
      seen.add(h.item_id);
      if (h.action === "delete") ok.add(h.sequence);
    }
    return ok;
  }, [history]);

  const rows = (history ?? []).filter((h) =>
    filter === "all" ? true : filter === "me" ? h.actor === "desktop" : h.actor !== "desktop",
  );
  const groups = useMemo(() => {
    const map = new Map<string, History[]>();
    for (const h of rows) {
      const day = toISO(new Date(h.occurred_at));
      map.set(day, [...(map.get(day) ?? []), h]);
    }
    return [...map];
  }, [rows]);

  const dayLabel = (day: string) =>
    day === today ? "今天" : day === addDays(today, -1) ? "昨天" : `${day.slice(0, 4)} 年 ${dayTitle(day)}`;

  return (
    <div className="page">
      <header className="view-head">
        <div>
          <h1>修改记录</h1>
          <p className="muted">每一次增删改都会留下记录，删除的事项可以在这里恢复。</p>
        </div>
        <Segmented<Filter>
          label="筛选"
          value={filter}
          onChange={setFilter}
          options={[
            { value: "all", label: "全部" },
            { value: "me", label: "我" },
            { value: "assistants", label: "AI 助手" },
          ]}
        />
      </header>

      {error && <p className="form-error">{error}</p>}
      {history && rows.length === 0 && (
        <div className="empty big">
          <div className="empty-art" aria-hidden="true" />
          <p>{filter === "all" ? "还没有修改记录" : "这个筛选下没有记录"}</p>
        </div>
      )}

      {groups.map(([day, list]) => (
        <section key={day} className="group">
          <h2 className="group-title">{dayLabel(day)}</h2>
          <ul className="timeline">
            {list.map((h) => {
              const diff = h.action === "update" || h.action === "status" ? changedFields(h) : [];
              const byAssistant = h.actor !== "desktop";
              return (
                <li key={h.sequence} className={cx("tl-row", h.action)}>
                  <span className={cx("tl-dot", h.action)} aria-hidden="true" />
                  <div className="tl-main">
                    <div className="tl-line">
                      <span className={cx("badge", h.action === "delete" ? "danger" : h.action === "create" ? "ok" : "info")}>
                        {ACTION_LABEL[h.action] ?? h.action}
                      </span>
                      <strong className="tl-title">{h.after.title}</strong>
                      <span className="muted">
                        {h.after.date}
                      </span>
                    </div>
                    {diff.length > 0 && (
                      <ul className="diff">
                        {diff.map((d) => (
                          <li key={d.label}>
                            <b>{d.label}</b> <s>{d.from}</s> → <span>{d.to}</span>
                          </li>
                        ))}
                      </ul>
                    )}
                    <div className="tl-meta muted">
                      {byAssistant && <Icon name="plug" size={13} />}
                      {actorLabel(h.actor, connections)} ·{" "}
                      {new Date(h.occurred_at).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}
                    </div>
                  </div>
                  <div className="tl-actions">
                    {restorable.has(h.sequence) ? (
                      <button
                        className="btn"
                        onClick={() =>
                          onRestore(h.item_id, h.after.revision)
                            .then(() => {
                              notify(`已恢复「${h.after.title}」`);
                              void load();
                            })
                            .catch((e: Error) => notify(e.message, { tone: "error" }))
                        }
                      >
                        <Icon name="undo" size={16} />
                        恢复
                      </button>
                    ) : (
                      h.action !== "delete" && (
                        <button className="btn ghost" onClick={() => onJump(h.after.date)}>
                          查看
                        </button>
                      )
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        </section>
      ))}
    </div>
  );
}
