import { memo, useEffect, useRef, useState } from "react";
import type { Item } from "../api";
import {
  MAX_DATE,
  MIN_DATE,
  parseISO,
  weekdayNames,
  type WeekStart,
} from "../lib/dates";
import { cx } from "../lib/util";

type CellProps = {
  date: string;
  month: string;
  today: string;
  selected: boolean;
  dropping: boolean;
  items: Item[] | undefined;
  /** 能放下几条（含/不含「还有 N 项」那一行）。 */
  cap: number;
  capAll: number;
  onSelect: (date: string) => void;
  onCreate: (date: string) => void;
  onOpen: (item: Item) => void;
  onDragItem: (id: string | undefined) => void;
  onDropTarget: (date: string | undefined) => void;
  onDrop: (date: string) => void;
};

const Cell = memo(function Cell(p: CellProps) {
  const disabled = p.date < MIN_DATE || p.date > MAX_DATE;
  const other = p.date.slice(0, 7) !== p.month;
  const day = Number(p.date.slice(8));
  const list = p.items ?? [];
  const shown = list.length <= p.capAll ? list.length : p.cap;
  const open = list.filter((i) => i.status !== "done").length;
  const label = `${Number(p.date.slice(5, 7))}月${day}日，${
    list.length ? `${list.length} 项，${open} 项未完成` : "没有事项"
  }${p.date === p.today ? "，今天" : ""}`;
  return (
    <div
      role="gridcell"
      aria-selected={p.selected}
      aria-label={label}
      aria-disabled={disabled || undefined}
      className={cx(
        "cell",
        [0, 6].includes(parseISO(p.date).getDay()) && "weekend",
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
        <span className="num">
          {day === 1 ? `${Number(p.date.slice(5, 7))}月${day}日` : day}
        </span>
        {list.length > 0 && (
          <span className="dots" aria-hidden="true">
            {list.length > shown && (
              <span className="count">{list.length}</span>
            )}
          </span>
        )}
      </div>
      <div className="chips">
        {list.slice(0, shown).map((item) => (
          <button
            key={item.id}
            className={cx(
              "chip",
              `c-${item.color}`,
              item.status === "done" && "done",
            )}
            draggable
            title={item.title}
            onClick={(e) => {
              e.stopPropagation();
              p.onSelect(p.date);
              p.onOpen(item);
            }}
            onDoubleClick={(e) => e.stopPropagation()}
            onDragStart={(e) => {
              e.dataTransfer.effectAllowed = "move";
              e.dataTransfer.setData("text/plain", item.id);
              p.onDragItem(item.id);
            }}
            onDragEnd={() => {
              p.onDragItem(undefined);
              p.onDropTarget(undefined);
            }}
          >
            <span className="chip-title">{item.title}</span>
          </button>
        ))}
        {list.length > shown && (
          <span className="more">还有 {list.length - shown} 项</span>
        )}
      </div>
    </div>
  );
});

export function MonthGrid({
  grid,
  month,
  today,
  selected,
  weekStart,
  itemsByDate,
  loading,
  onSelect,
  onCreate,
  onOpen,
  onMove,
}: {
  grid: string[];
  month: string;
  today: string;
  selected: string;
  weekStart: WeekStart;
  itemsByDate: Map<string, Item[]>;
  loading: boolean;
  onSelect: (date: string) => void;
  onCreate: (date: string) => void;
  onOpen: (item: Item) => void;
  onMove: (itemId: string, date: string) => void;
}) {
  const dragging = useRef<string>();
  const [dropDate, setDropDate] = useState<string>();
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
  // 按实际高度决定显示条数：头部与留白约 44px，每条含间距 26px。
  const cellsRef = useRef<HTMLDivElement>(null);
  const [cap, setCap] = useState({ cap: 2, capAll: 3 });
  useEffect(() => {
    const node = cellsRef.current;
    if (!node) return;
    const measure = () => {
      const row = node.clientHeight / rows;
      const next = {
        cap: Math.max(1, Math.min(6, Math.floor((row - 44 - 18) / 26))),
        capAll: Math.max(1, Math.min(6, Math.floor((row - 44) / 26))),
      };
      setCap((old) =>
        old.cap === next.cap && old.capAll === next.capAll ? old : next,
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
            selected={date === selected}
            dropping={date === dropDate}
            items={itemsByDate.get(date)}
            cap={cap.cap}
            capAll={cap.capAll}
            onSelect={onSelect}
            onCreate={onCreate}
            onOpen={onOpen}
            onDragItem={(id) => (dragging.current = id)}
            onDropTarget={setDropDate}
            onDrop={(target) => {
              const id = dragging.current;
              dragging.current = undefined;
              setDropDate(undefined);
              if (id) onMove(id, target);
            }}
          />
        ))}
      </div>
    </div>
  );
}
