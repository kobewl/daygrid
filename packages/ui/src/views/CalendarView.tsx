import { useEffect, useMemo } from "react";
import type { Item, SyncPeer } from "../api";
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
import { dayInfo } from "../lib/workday";

export function CalendarView({
  cal,
  peers,
  weekStart,
  holidays,
  keysEnabled,
  onCreate,
  onEdit,
  onToggle,
  onRemove,
  onMove,
  onQuickAdd,
  teamMode,
}: {
  peers: SyncPeer[];
  cal: Calendar;
  weekStart: WeekStart;
  /** 休 / 班是否按法定节假日与调休推算。 */
  holidays: boolean;
  /** 有对话框或命令面板打开时关闭方向键等快捷键。 */
  keysEnabled: boolean;
  onCreate: (date: string) => void;
  onEdit: (item: Item) => void;
  onToggle: (item: Item) => void;
  onRemove: (item: Item) => void;
  onMove: (id: string, date: string) => void;
  onQuickAdd: (date: string, title: string) => Promise<boolean>;
  /** 连接了团队日历。 */
  teamMode: boolean;
}) {
  const { context, month, selected, grid, itemsByDate, loading } = cal;
  const { goDate, goToday, shiftMonth } = cal;
  // 每天是休息日还是工作日：手动标记 > 法定假日与调休 > 周末。
  const days = useMemo(
    () =>
      new Map(
        cal.grid.map((date) => [date, dayInfo(date, cal.dayMarks.get(date), holidays)]),
      ),
    [cal.grid, cal.dayMarks, holidays],
  );

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
  let restDays = 0;
  let workDays = 0;
  for (const [date, info] of days) {
    if (!date.startsWith(month)) continue;
    if (info.kind === "rest") restDays++;
    else workDays++;
  }

  return (
    <div className="calendar">
      <section className="calendar-main">
        <header className="view-head">
          <div className="month-heading">
            <p className="eyebrow">
              <span>{month.slice(0, 4)} 年</span>
              <span className="view-label">月历</span>
            </p>
            <h1
              key={month}
              className="month-title"
              aria-label={monthTitle(month)}
            >
              <span>{Number(month.slice(5))}</span>
              <span className="month-unit">月</span>
            </h1>
            <p className="month-overview">
              {monthItems.length
                ? `${monthItems.length} 项安排`
                : "这个月，慢慢安排"}
              {completed > 0 && (
                <>
                  <span aria-hidden="true">·</span>
                  {completed} 项已完成
                </>
              )}
            </p>
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
              <button
                className="btn today-button"
                onClick={goToday}
                aria-label="回到今天"
                title="回到今天（T）"
              >
                今天
                <span className="today-button-date">
                  {Number(context.today.slice(8))} 日
                </span>
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
          timeZone={context.calendar_timezone}
          selected={selected}
          weekStart={weekStart}
          itemsByDate={itemsByDate}
          days={days}
          fresh={cal.fresh}
          loading={loading}
          onSelect={cal.setSelected}
          onCreate={onCreate}
          onOpen={onEdit}
          onMove={onMove}
        />
        <footer className="calendar-foot">
          <span>
            <i className="today-key" aria-hidden="true" />
            今天
            <i className="selected-key" aria-hidden="true" />
            选中日期
            {` · 本月 ${workDays} 个工作日 · ${restDays} 个休息日`}
          </span>
          <span>双击日期添加 · 拖动事项改期</span>
        </footer>
      </section>
      <DayPanel
        date={selected}
        today={context.today}
        timeZone={context.calendar_timezone}
        items={itemsByDate.get(selected) ?? []}
        day={days.get(selected) ?? dayInfo(selected, cal.dayMarks.get(selected), holidays)}
        onMark={(value) => void cal.setDayMark(selected, value)}
        fresh={cal.fresh}
        peers={peers}
        teamMode={teamMode}
        onToggle={onToggle}
        onOpen={onEdit}
        onDelete={onRemove}
        onQuickAdd={(title) => onQuickAdd(selected, title)}
        onAdd={() => onCreate(selected)}
      />
    </div>
  );
}
