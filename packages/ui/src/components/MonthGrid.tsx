import { memo, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { Item } from "../api";
import {
  MAX_DATE,
  MIN_DATE,
  weekdayNames,
  type WeekStart,
} from "../lib/dates";
import { isPast, toInput, zhDateTime } from "../lib/reminder";
import { cx } from "../lib/util";
import { Icon } from "./icons";
import { kindLabel, type DayInfo, type DayKind } from "../lib/workday";

type CellProps = {
  date: string;
  month: string;
  today: string;
  /** 日历时区：只给未过的提醒显示铃铛。 */
  timeZone: string;
  selected: boolean;
  dropping: boolean;
  items: Item[] | undefined;
  /** 这一天是休息日还是工作日；special 表示法定假日、调休补班或手动标记，月历里要突出。 */
  kind: DayKind;
  special: boolean;
  dayHint: string;
  /** 正在被拖拽的事项（用于让原位置变淡）。 */
  dragId: string | undefined;
  /** 刚放下的事项（用于落点动效）。 */
  settledId: string | undefined;
  /** 刚由外部写入到达的事项（用于到达动效）。 */
  fresh: ReadonlySet<string>;
  /** 能放下几条（含/不含「还有 N 项」那一行）。 */
  cap: number;
  capAll: number;
  /** 放不下事项条和更多入口时，改在表头显示折叠数量。 */
  compact: boolean;
  onSelect: (date: string) => void;
  onCreate: (date: string) => void;
  onOpen: (item: Item) => void;
  onDragItem: (id: string | undefined, fromDate?: string) => void;
  onDropTarget: (date: string | undefined) => void;
  onDrop: (date: string) => void;
};

const Cell = memo(function Cell(p: CellProps) {
  const disabled = p.date < MIN_DATE || p.date > MAX_DATE;
  const other = p.date.slice(0, 7) !== p.month;
  const day = Number(p.date.slice(8));
  const list = p.items ?? [];
  const shown = p.compact
    ? Math.min(list.length, p.capAll)
    : list.length <= p.capAll ? list.length : p.cap;
  const hidden = list.length - shown;
  const open = list.filter((i) => i.status !== "done").length;
  const label = `${Number(p.date.slice(5, 7))}月${day}日，${kindLabel(p.kind)}，${
    list.length ? `${list.length} 项，${open} 项未完成` : "没有事项"
  }${p.date === p.today ? "，今天" : ""}`;
  return (
    <div
      role="gridcell"
      aria-selected={p.selected}
      aria-label={label}
      aria-disabled={disabled || undefined}
      aria-current={p.date === p.today ? "date" : undefined}
      className={cx(
        "cell",
        p.kind === "rest" && "rest",
        other && "other",
        p.date === p.today && "today",
        p.selected && "selected",
        p.dropping && "drop",
        disabled && "disabled",
      )}
      data-date={p.date}
      onClick={() => !disabled && p.onSelect(p.date)}
      onDoubleClick={() => !disabled && p.onCreate(p.date)}
      onDragOver={(e) => {
        if (disabled) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
      }}
      onDragEnter={() => !disabled && p.onDropTarget(p.date)}
      onDrop={(e) => {
        e.preventDefault();
        if (!disabled) p.onDrop(p.date);
      }}
    >
      <div className="cell-head">
        <span className="num-wrap">
          <span className="num">
            {day}
            {day === 1 && other && <small>{Number(p.date.slice(5, 7))}月</small>}
          </span>
          <span
            className={cx("daytag", p.kind, p.special && "special")}
            title={`${kindLabel(p.kind)} · ${p.dayHint}`}
            aria-hidden="true"
          >
            {p.kind === "rest" ? "休" : "班"}
          </span>
        </span>
        {p.date === p.today && <span className="cell-today-label" aria-hidden="true">今天</span>}
        {list.length > 0 && (
          <span className="dots" aria-hidden="true">
            {hidden > 0 && p.compact ? (
              // 紧凑格子里没有「还有 N 项」那一行，直接标出被折叠的数量。
              <span className="count compact" title={`还有 ${hidden} 项未显示`}>
                +{hidden}
              </span>
            ) : (
              <span className="count" title={`${list.length} 项，${open} 项待完成`}>{list.length}</span>
            )}
          </span>
        )}
      </div>
      <div className="chips">
        {list.slice(0, shown).map((item) => {
          // 只有未完成事项的未来提醒才显示铃铛（已完成或已过的不会再触发）。
          const local = toInput(item.remind_at);
          const bell = !!local && item.status === "planned" && !isPast(local, p.timeZone);
          return (
            <button
              key={item.id}
              className={cx(
                "chip",
                `c-${item.color}`,
                item.status === "done" && "done",
                p.dragId === item.id && "dragging",
                p.settledId === item.id && "settle",
                p.fresh.has(item.id) && "arrive",
              )}
              draggable
              title={bell ? `${item.title} · 提醒 ${zhDateTime(local!)}` : item.title}
              onClick={(e) => {
                e.stopPropagation();
                p.onSelect(p.date);
                p.onOpen(item);
              }}
              onDoubleClick={(e) => e.stopPropagation()}
              onDragStart={(e) => {
                e.dataTransfer.effectAllowed = "move";
                e.dataTransfer.setData("text/plain", item.id);
                p.onDragItem(item.id, p.date);
              }}
              onDragEnd={() => {
                p.onDragItem(undefined);
                p.onDropTarget(undefined);
              }}
            >
              <span className="chip-status" aria-hidden="true" />
              <span className="chip-title">{item.title}</span>
              {bell && <Icon name="bell" size={9} className="chip-bell" aria-hidden />}
            </button>
          );
        })}
        {hidden > 0 && !p.compact && (
          <button
            type="button"
            className="more"
            title="在右侧查看当天全部事项"
            aria-label={`${label.split("，")[0]}，查看全部 ${list.length} 项`}
            onClick={(e) => {
              // 选中这一天：右侧当天面板会列出全部事项。
              e.stopPropagation();
              if (!disabled) p.onSelect(p.date);
            }}
            onDoubleClick={(e) => e.stopPropagation()}
          >
            还有 {hidden} 项
          </button>
        )}
      </div>
    </div>
  );
});

export function MonthGrid({
  grid,
  month,
  today,
  timeZone,
  selected,
  weekStart,
  itemsByDate,
  days,
  fresh,
  loading,
  onSelect,
  onCreate,
  onOpen,
  onMove,
}: {
  grid: string[];
  month: string;
  today: string;
  timeZone: string;
  selected: string;
  weekStart: WeekStart;
  itemsByDate: Map<string, Item[]>;
  days: ReadonlyMap<string, DayInfo>;
  fresh: ReadonlySet<string>;
  loading: boolean;
  onSelect: (date: string) => void;
  onCreate: (date: string) => void;
  onOpen: (item: Item) => void;
  onMove: (itemId: string, date: string) => void;
}) {
  const dragging = useRef<string>();
  const dragFrom = useRef<string>();
  const dragTimer = useRef<number>();
  const settleTimer = useRef<number>();
  const [dropDate, setDropDate] = useState<string>();
  const [dragId, setDragId] = useState<string>();
  const [settledId, setSettledId] = useState<string>();
  useEffect(
    () => () => {
      window.clearTimeout(dragTimer.current);
      window.clearTimeout(settleTimer.current);
    },
    [],
  );
  const startDrag = (id: string | undefined, fromDate?: string) => {
    dragging.current = id;
    dragFrom.current = id ? fromDate : undefined;
    window.clearTimeout(dragTimer.current);
    if (id) {
      // 拖拽图像在 dragstart 返回后才截取；同步改样式会让「被拖着走的那个」也变淡，所以延后一拍。
      dragTimer.current = window.setTimeout(() => setDragId(id), 0);
    } else setDragId(undefined);
  };
  // 只展示包含当月日期的周，避免多出一整行下月的空格。
  let visibleLength = grid.length;
  while (
    visibleLength > 28 &&
    grid
      .slice(visibleLength - 7, visibleLength)
      .every((date) => !date.startsWith(month))
  ) {
    visibleLength -= 7;
  }
  // 跳转到相邻月日期时，仍将选中的日期所在周保留在视图中。
  const selectedIndex = grid.indexOf(selected);
  if (selectedIndex >= visibleLength)
    visibleLength = Math.ceil((selectedIndex + 1) / 7) * 7;
  const visibleGrid = grid.slice(0, visibleLength);
  const rows = visibleGrid.length / 7;
  // 按实际头部、留白和事项尺寸计算容量，大小窗口都不裁切内容。
  const cellsRef = useRef<HTMLDivElement>(null);
  // 翻月：按方向让内容淡入并带一点位移。用 Web Animations 而不是重新挂载，
  // 否则 ResizeObserver 绑定的节点会被替换。「减少动态效果」时不播放。
  const prevMonth = useRef(month);
  useLayoutEffect(() => {
    const before = prevMonth.current;
    prevMonth.current = month;
    const node = cellsRef.current;
    if (!node || before === month || typeof node.animate !== "function") return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const direction = month > before ? 1 : -1; // "YYYY-MM" 字符串可直接比较先后
    node.animate(
      [
        { opacity: 0.35, transform: `translateX(${direction * 14}px)` },
        { opacity: 1, transform: "translateX(0)" },
      ],
      { duration: 140, easing: "cubic-bezier(0.2, 0, 0, 1)" },
    );
  }, [month]);
  const [cap, setCap] = useState({ cap: 2, capAll: 3, compact: false });
  useEffect(() => {
    const node = cellsRef.current;
    if (!node) return;
    const measure = () => {
      const row = node.clientHeight / rows;
      const cell = node.firstElementChild as HTMLElement | null;
      if (!cell) return;
      const style = getComputedStyle(cell);
      const padding = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
      const head = cell.querySelector(".cell-head")?.getBoundingClientRect().height ?? 30;
      const available = Math.max(0, row - padding - head - parseFloat(style.rowGap));
      const itemHeight = parseFloat(style.getPropertyValue("--calendar-item-height")) || 26;
      const itemGap = parseFloat(style.getPropertyValue("--calendar-item-gap")) || 5;
      const step = itemHeight + itemGap;
      const withMore = Math.floor((available - 20) / step);
      const next = {
        cap: Math.max(0, Math.min(6, withMore)),
        capAll: Math.max(0, Math.min(6, Math.floor((available + itemGap) / step))),
        compact: withMore < 1,
      };
      setCap((old) =>
        old.cap === next.cap &&
        old.capAll === next.capAll &&
        old.compact === next.compact
          ? old
          : next,
      );
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    return () => observer.disconnect();
  }, [rows]);
  return (
    <div
      className={cx("month", loading && "loading")}
      role="grid"
      aria-label="月历"
      aria-busy={loading}
    >
      <div className="weekdays" role="row">
        {weekdayNames(weekStart).map((n) => (
          <div
            key={n}
            role="columnheader"
            className={n === "周六" || n === "周日" ? "weekend" : undefined}
          >
            {n}
          </div>
        ))}
      </div>
      <div
        className="cells"
        ref={cellsRef}
        style={{ gridTemplateRows: `repeat(${rows}, minmax(0, 1fr))` }}
      >
        {visibleGrid.map((date) => (
          <Cell
            key={date}
            date={date}
            month={month}
            today={today}
            timeZone={timeZone}
            selected={date === selected}
            dropping={date === dropDate}
            items={itemsByDate.get(date)}
            kind={days.get(date)?.kind ?? "work"}
            special={days.get(date)?.special ?? false}
            dayHint={days.get(date)?.reason ?? ""}
            dragId={dragId}
            settledId={settledId}
            fresh={fresh}
            cap={cap.cap}
            capAll={cap.capAll}
            compact={cap.compact}
            onSelect={onSelect}
            onCreate={onCreate}
            onOpen={onOpen}
            onDragItem={startDrag}
            onDropTarget={setDropDate}
            onDrop={(target) => {
              const id = dragging.current;
              const from = dragFrom.current;
              startDrag(undefined);
              setDropDate(undefined);
              if (!id) return;
              onMove(id, target);
              if (from !== target) {
                // 放到别的日期：新位置的事项条轻轻「吸」一下。
                setSettledId(id);
                window.clearTimeout(settleTimer.current);
                settleTimer.current = window.setTimeout(
                  () => setSettledId(undefined),
                  400,
                );
              }
            }}
          />
        ))}
      </div>
    </div>
  );
}
