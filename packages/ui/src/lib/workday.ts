import { holidayOf } from "./holidays";
import { parseISO } from "./dates";

/** rest = 休息日，work = 工作日。 */
export type DayKind = "rest" | "work";
/** 手动标记的取值；default 表示清除标记、回到按周末与法定假日推算。 */
export type MarkValue = DayKind | "default";

export type DayInfo = {
  kind: DayKind;
  /** 值得在月历里突出：法定假日、调休补班、手动标记。普通的周末与工作日只淡淡标注。 */
  special: boolean;
  /** 是否是用户手动标记的。 */
  manual: boolean;
  /** 不考虑手动标记时的类型，用于「默认」选项上的提示。 */
  defaultKind: DayKind;
  /** 说明这一天为什么是休 / 班，例如「国庆节 · 法定假日」。 */
  reason: string;
};

const WEEK = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];

/** 不含手动标记：周末休息、工作日上班，再叠加法定放假与调休补班。 */
function baseline(
  date: string,
  useHolidays: boolean,
): { kind: DayKind; special: boolean; reason: string } {
  const weekday = parseISO(date).getDay();
  const weekend = weekday === 0 || weekday === 6;
  const holiday = useHolidays ? holidayOf(date) : undefined;
  if (holiday)
    return holiday.off
      ? { kind: "rest", special: true, reason: `${holiday.name} · 法定假日` }
      : { kind: "work", special: true, reason: `${holiday.name}调休 · 补班` };
  return {
    kind: weekend ? "rest" : "work",
    special: false,
    reason: `${WEEK[weekday]} · ${weekend ? "周末" : "工作日"}`,
  };
}

/**
 * 某一天是休息日还是工作日。
 * 优先级：手动标记 > 法定放假 / 调休补班 > 周末与工作日。
 */
export function dayInfo(
  date: string,
  mark: DayKind | undefined,
  useHolidays: boolean,
): DayInfo {
  const base = baseline(date, useHolidays);
  if (!mark) {
    return {
      kind: base.kind,
      special: base.special,
      manual: false,
      defaultKind: base.kind,
      reason: base.reason,
    };
  }
  return {
    kind: mark,
    // 与默认一致的手动标记（例如把普通周六标成休）没有改变任何东西，不必突出。
    special: mark !== base.kind,
    manual: true,
    defaultKind: base.kind,
    reason:
      mark === base.kind
        ? `${base.reason} · 已确认`
        : `手动标记为${mark === "rest" ? "休息日" : "工作日"}（默认：${base.reason}）`,
  };
}

export const kindLabel = (kind: DayKind) =>
  kind === "rest" ? "休息日" : "工作日";
