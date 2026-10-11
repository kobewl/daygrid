import { useEffect } from "react";
import type { Item } from "../api";
import { DayPanel } from "../components/DayPanel";
import { Icon } from "../components/icons";
import { MonthGrid } from "../components/MonthGrid";
import type { Calendar } from "../hooks/useCalendar";
import {
  MAX_DATE,
  MIN_DATE,
  addDays,
  monthTitle,
  type WeekStart,
} from "../lib/dates";
import { isTyping } from "../lib/util";

export function CalendarView({
  cal,
  weekStart,
  keysEnabled,
  onCreate,
  onEdit,
  onToggle,
  onRemove,
  onMove,
  onQuickAdd,
}: {
  cal: Calendar;
  weekStart: WeekStart;
  /** 有对话框或命令面板打开时关闭方向键等快捷键。 */
  keysEnabled: boolean;
  onCreate: (date: string) => void;
  onEdit: (item: Item) => void;
  onToggle: (item: Item) => void;
  onRemove: (item: Item) => void;
  onMove: (id: string, date: string) => void;
  onQuickAdd: (date: string, title: string) => Promise<boolean>;
}) {
  const { context, month, selected, grid, itemsByDate, loading } = cal;
  const { goDate, goToday, shiftMonth } = cal;

  // 方向键移动选中日期；[ ] 翻月；T 回到今天；N 新建。
  useEffect(() => {
    if (!keysEnabled || !selected) return;
    const onKey = (e: KeyboardEvent) => {
      if (isTyping(e.target) || e.metaKey || e.ctrlKey || e.altKey) return;
      const step: Record<string, number> = {
        ArrowLeft: -1,
        ArrowRight: 1,
        ArrowUp: -7,
        ArrowDown: 7,
      };
      if (e.key in step) {
        e.preventDefault();
        const next = addDays(selected, step[e.key]);
        if (next >= MIN_DATE && next <= MAX_DATE) goDate(next);
      } else if (e.key === "[" || e.key === "PageUp") {
        e.preventDefault();
        shiftMonth(-1);
      } else if (e.key === "]" || e.key === "PageDown") {
        e.preventDefault();
        shiftMonth(1);
      } else if (e.key.toLowerCase() === "t") {
        goToday();
      } else if (e.key.toLowerCase() === "n") {
        e.preventDefault();
        onCreate(selected);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [keysEnabled, selected, goDate, goToday, shiftMonth, onCreate]);

  if (!context || !month || !selected) return null;
  const atMin = month <= MIN_DATE.slice(0, 7);
  const atMax = month >= MAX_DATE.slice(0, 7);
  const monthItems = cal.items.filter((item) => item.date.startsWith(month));
  const completed = monthItems.filter((item) => item.status === "done").length;

  return (
    <div className="calendar">
      <section className="calendar-main">
        <header className="view-head">
          <div className="month-heading">
            <p className="eyebrow">{month.slice(0, 4)} 年 · 月历</p>
            <h1 aria-label={monthTitle(month)}>{Number(month.slice(5))} 月</h1>
          </div>
          <div className="head-actions">
            <div className="btn-group" role="group" aria-label="切换月份">
              <button
                className="btn icon-btn"
                onClick={() => shiftMonth(-1)}
                disabled={atMin}
                aria-label="上个月"
                title="上个月（[）"
              >
                <Icon name="left" />
              </button>
              <button className="btn" onClick={goToday} title="回到今天（T）">
                今天
              </button>
              <button
                className="btn icon-btn"
                onClick={() => shiftMonth(1)}
                disabled={atMax}
                aria-label="下个月"
                title="下个月（]）"
              >
                <Icon name="right" />
              </button>
            </div>
            <button
              className="btn primary"
              onClick={() => onCreate(selected)}
              title="添加事项（N）"
            >
              <Icon name="plus" size={16} />
              添加事项
            </button>
          </div>
        </header>
        <MonthGrid
          grid={grid}
          month={month}
          today={context.today}
          selected={selected}
          weekStart={weekStart}
          itemsByDate={itemsByDate}
          loading={loading}
          onSelect={cal.setSelected}
          onCreate={onCreate}
          onOpen={onEdit}
          onMove={onMove}
        />
        <footer className="calendar-foot">
          <span>
            {monthItems.length
              ? `本月 ${monthItems.length} 项安排 · ${completed} 项已完成`
              : "从一件小事，开始安排这个月"}
          </span>
          <span>双击日期添加 · 拖动事项改期</span>
        </footer>
      </section>
      <DayPanel
        date={selected}
        today={context.today}
        items={itemsByDate.get(selected) ?? []}
        onToggle={onToggle}
        onOpen={onEdit}
        onDelete={onRemove}
        onQuickAdd={(title) => onQuickAdd(selected, title)}
        onAdd={() => onCreate(selected)}
      />
    </div>
  );
}
