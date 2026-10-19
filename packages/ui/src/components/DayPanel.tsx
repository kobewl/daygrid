import { useState } from "react";
import type { Item, SyncPeer } from "../api";
import { addDays, dayTitle, weekdayLong } from "../lib/dates";
import { isPast, reminderLabel, toInput } from "../lib/reminder";
import { cx } from "../lib/util";
import { kindLabel, type DayInfo, type MarkValue } from "../lib/workday";
import { Avatars } from "./Avatars";
import { Icon } from "./icons";
import { firstNoteLine, Markdown } from "./Markdown";
import { Segmented } from "./Segmented";

/** 事项行上的提醒角标。「已过」只在渲染时判断，不为它挂计时器。 */
function RemindBadge({ item, timeZone }: { item: Item; timeZone: string }) {
  const local = toInput(item.remind_at);
  if (!local) return null;
  const { label, full: base } = reminderLabel(local, item.date);
  const done = item.status === "done";
  const muted = done || isPast(local, timeZone);
  // 已完成事项的提醒行已删除，不会触发。
  const full = done ? `${base}（已完成，不提醒）` : base;
  return (
    <span className={cx("remind-badge", muted && "past")} title={full}>
      <Icon name="bell" size={11} />
      <span aria-hidden="true">{label}</span>
      <span className="sr-only">{full}</span>
    </span>
  );
}

export function DayPanel({
  date,
  today,
  timeZone,
  items,
  day,
  fresh,
  peers,
  teamMode,
  onMark,
  onToggle,
  onOpen,
  onDelete,
  onQuickAdd,
  onAdd,
}: {
  /** 协同时：在线的同事（含他们正在看哪一天、正在编辑哪个事项）。 */
  peers: SyncPeer[];
  /** 连接了团队日历：事项行显示「仅自己」标记。 */
  teamMode: boolean;
  date: string;
  today: string;
  /** 日历时区：判断提醒是否已过。 */
  timeZone: string;
  items: Item[];
  /** 这一天是休息日还是工作日，以及原因。 */
  day: DayInfo;
  /** 手动标记这一天；default 清除标记。 */
  onMark: (value: MarkValue) => void;
  /** 刚由外部写入到达的事项（用于到达动效）。 */
  fresh: ReadonlySet<string>;
  onToggle: (item: Item) => void;
  onOpen: (item: Item) => void;
  onDelete: (item: Item) => void;
  onQuickAdd: (title: string) => Promise<boolean>;
  onAdd: () => void;
}) {
  const [title, setTitle] = useState("");
  const [busy, setBusy] = useState(false);
  const done = items.filter((i) => i.status === "done").length;
  const viewing = peers.filter((p) => p.date === date);
  const editors = (id: string) => [...new Set(peers.filter((p) => p.editing && p.item_id === id).map((p) => p.user))];
  const relative =
    date === today
      ? "今天"
      : date === addDays(today, 1)
        ? "明天"
        : date === addDays(today, -1)
          ? "昨天"
          : "";

  const submit = async () => {
    const text = title.trim();
    if (!text || busy) return;
    setBusy(true);
    try {
      if (await onQuickAdd(text)) setTitle("");
    } finally {
      setBusy(false);
    }
  };

  return (
    <aside className="panel" aria-label={`${dayTitle(date)}的事项`}>
      <header className="panel-head">
        <div>
          <h2>
            {dayTitle(date)}
            {relative && <span className="tag">{relative}</span>}
          </h2>
          <p className="muted">
            {weekdayLong(date)}
            {items.length > 0 && ` · ${items.length} 项，${done} 项已完成`}
          </p>
        </div>
        <button
          className="btn ghost icon-btn"
          onClick={onAdd}
          aria-label="添加事项（详细）"
          title="添加事项（N）"
        >
          <Icon name="plus" />
        </button>
      </header>
      {viewing.length > 0 && (
        <p className="presence" role="status">
          <Avatars peers={viewing} />
          <span>{[...new Set(viewing.map((p) => p.user))].join("、")} 也在看这一天</span>
        </p>
      )}
      <section
        className={cx("daykind", day.kind, day.special && "special")}
        aria-label="这一天是休息日还是工作日"
      >
        <div className="daykind-main">
          <span className={cx("daytag", day.kind, day.special && "special")} aria-hidden="true">
            {day.kind === "rest" ? "休" : "班"}
          </span>
          <div>
            <strong>{kindLabel(day.kind)}</strong>
            <small>{day.reason}</small>
          </div>
        </div>
        <Segmented<MarkValue>
          label="标记这一天"
          value={day.manual ? day.kind : "default"}
          onChange={onMark}
          options={[
            { value: "default", label: "默认" },
            { value: "rest", label: "休" },
            { value: "work", label: "班" },
          ]}
        />
      </section>
      <div className="day-summary">
        <span>
          {items.length ? `${items.length - done} 项待完成` : "每日安排"}
        </span>
        {items.length > 0 && (
          <span>
            {done} / {items.length}
          </span>
        )}
      </div>
      {items.length > 0 && (
        <div
          className="day-progress"
          role="progressbar"
          aria-label="当天完成进度"
          aria-valuemin={0}
          aria-valuemax={items.length}
          aria-valuenow={done}
        >
          <span style={{ width: `${(done / items.length) * 100}%` }} />
        </div>
      )}

      {items.length === 0 ? (
        <div className="empty">
          <div className="day-empty-art" aria-hidden="true">
            <Icon name="sun" size={32} />
            <span />
          </div>
          <p>留白，也是一种安排</p>
          <small>
            记下一件想做的事，
            <br />
            让这一天有个小小的开始。
          </small>
          <button className="btn" onClick={onAdd}>
            <Icon name="plus" size={15} /> 添加第一件事
          </button>
        </div>
      ) : (
        <ul className="agenda">
          {items.map((item) => (
            <li
              key={item.id}
              className={cx(
                "row",
                item.status === "done" && "done",
                fresh.has(item.id) && "arrive",
              )}
            >
              <button
                className={cx("check", `c-${item.color}`)}
                role="checkbox"
                aria-checked={item.status === "done"}
                aria-label={`${item.status === "done" ? "取消完成" : "标记完成"}：${item.title}`}
                onClick={() => onToggle(item)}
              >
                {/* 勾号常驻 DOM，由 CSS 过渡显隐，状态改变时才有动效。 */}
                <Icon name="check" size={13} strokeWidth={2.6} />
              </button>
              {/* 整块可点开编辑；标题是键盘入口，备注摘要里的链接单独可点（在系统浏览器打开）。 */}
              <div
                className="row-main"
                onClick={(e) => {
                  if ((e.target as Element).closest("a")) return;
                  onOpen(item);
                }}
              >
                <button type="button" className="row-title">
                  {item.title}
                </button>
                <RemindBadge item={item} timeZone={timeZone} />
                {teamMode && item.visibility === "private" && (
                  <span className="vis-badge" title="仅自己可见：不会上传，同事看不到">
                    <Icon name="lock" size={11} /> 仅自己
                  </span>
                )}
                {editors(item.id).length > 0 && (
                  <span className="row-editing">{editors(item.id).join("、")} 正在编辑…</span>
                )}
                {firstNoteLine(item.notes) && (
                  <div className="row-sub">
                    <Markdown source={firstNoteLine(item.notes)} inline />
                  </div>
                )}
              </div>
              <button
                className="row-del"
                onClick={() => onDelete(item)}
                aria-label={`删除：${item.title}`}
                title="删除（可撤销）"
              >
                <Icon name="trash" size={16} />
              </button>
            </li>
          ))}
        </ul>
      )}

      <form
        className="quick"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <Icon name="plus" size={16} />
        <input
          id="quick-add"
          value={title}
          maxLength={200}
          disabled={busy}
          onChange={(e) => setTitle(e.target.value)}
          placeholder="记下一件事，回车添加"
          aria-label="快速添加事项"
        />
        <button
          type="submit"
          className="quick-submit"
          disabled={busy || !title.trim()}
          aria-label="保存快速事项"
        >
          ↵
        </button>
      </form>
    </aside>
  );
}
