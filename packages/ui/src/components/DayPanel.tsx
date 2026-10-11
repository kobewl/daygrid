import { useState } from "react";
import type { Item } from "../api";
import { colorName } from "../api";
import { addDays, dayTitle, weekdayLong } from "../lib/dates";
import { cx } from "../lib/util";
import { Icon } from "./icons";

export function DayPanel({
  date,
  today,
  items,
  onToggle,
  onOpen,
  onDelete,
  onQuickAdd,
  onAdd,
}: {
  date: string;
  today: string;
  items: Item[];
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
    date === today ? "今天" : date === addDays(today, 1) ? "明天" : date === addDays(today, -1) ? "昨天" : "";

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
        <button className="btn ghost icon-btn" onClick={onAdd} aria-label="添加事项（详细）" title="添加事项（N）">
          <Icon name="plus" />
        </button>
      </header>

      {items.length === 0 ? (
        <div className="empty">
          <div className="empty-art" aria-hidden="true" />
          <p>这一天还没有安排</p>
          <small>在下面输入后按回车，或者把其他日期的事项拖到这天。</small>
        </div>
      ) : (
        <ul className="agenda">
          {items.map((item) => (
            <li key={item.id} className={cx("row", item.status === "done" && "done")}>
              <button
                className={cx("check", `c-${item.color}`)}
                role="checkbox"
                aria-checked={item.status === "done"}
                aria-label={`${item.status === "done" ? "取消完成" : "标记完成"}：${item.title}`}
                onClick={() => onToggle(item)}
              >
                {item.status === "done" && <Icon name="check" size={13} strokeWidth={2.6} />}
              </button>
              <button className="row-main" onClick={() => onOpen(item)}>
                <span className="row-title">{item.title}</span>
                <span className="row-sub">
                  <i className={cx("swatch", `c-${item.color}`)} aria-hidden="true" />
                  {colorName(item.color)}
                  {item.notes && <> · {item.notes.split("\n")[0]}</>}
                </span>
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
          placeholder={`添加到 ${dayTitle(date)}，回车保存`}
          aria-label="快速添加事项"
        />
      </form>
    </aside>
  );
}
