import { useState } from "react";
import type { Item } from "../api";
import { addDays, dayTitle, weekdayLong } from "../lib/dates";
import { cx } from "../lib/util";
import { Icon } from "./icons";

export function DayPanel({
  date,
  today,
  items,
  fresh,
  onToggle,
  onOpen,
  onDelete,
  onQuickAdd,
  onAdd,
}: {
  date: string;
  today: string;
  items: Item[];
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
              <button className="row-main" onClick={() => onOpen(item)}>
                <span className="row-title">{item.title}</span>
                {item.notes && (
                  <span className="row-sub">{item.notes.split("\n")[0]}</span>
                )}
              </button>
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
