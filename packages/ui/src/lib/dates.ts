// 日历只处理「纯日期」（YYYY-MM-DD 字符串），不带时区。所有运算都用本地正午构造 Date，
// 避免夏令时边界把日期推到前一天或后一天。
export const MIN_DATE = "1900-01-01";
export const MAX_DATE = "2100-12-31";

const pad = (n: number) => String(n).padStart(2, "0");
export const toISO = (d: Date) =>
  `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
export const parseISO = (s: string) =>
  new Date(
    Number(s.slice(0, 4)),
    Number(s.slice(5, 7)) - 1,
    Number(s.slice(8, 10)),
    12,
  );
export const monthKey = (s: string) => s.slice(0, 7);
export const inRange = (s: string) => s >= MIN_DATE && s <= MAX_DATE;
export const clampDate = (s: string) =>
  s < MIN_DATE ? MIN_DATE : s > MAX_DATE ? MAX_DATE : s;

export function addDays(s: string, n: number) {
  const d = parseISO(s);
  d.setDate(d.getDate() + n);
  return toISO(d);
}
/** 加减月份，日号超出目标月天数时落在月末（1 月 31 日 +1 月 = 2 月末）。 */
export function addMonths(s: string, n: number) {
  const d = parseISO(s);
  const day = d.getDate();
  d.setDate(1);
  d.setMonth(d.getMonth() + n);
  const last = new Date(d.getFullYear(), d.getMonth() + 1, 0).getDate();
  d.setDate(Math.min(day, last));
  return toISO(d);
}

export type WeekStart = 0 | 1;
/** 6 行 × 7 列，共 42 个日期；weekStart 1 = 周一开头，0 = 周日开头。 */
export function monthGrid(month: string, weekStart: WeekStart): string[] {
  const first = parseISO(month + "-01");
  const offset =
    weekStart === 1 ? (first.getDay() + 6) % 7 : first.getDay();
  first.setDate(1 - offset);
  return Array.from({ length: 42 }, (_, i) => {
    const d = new Date(first);
    d.setDate(first.getDate() + i);
    return toISO(d);
  });
}

const NAMES = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];
export const weekdayNames = (weekStart: WeekStart) =>
  weekStart === 1 ? [...NAMES.slice(1), NAMES[0]] : NAMES;
export const weekdayLong = (s: string) =>
  parseISO(s).toLocaleDateString("zh-CN", { weekday: "long" });
export const monthTitle = (month: string) =>
  `${Number(month.slice(0, 4))} 年 ${Number(month.slice(5))} 月`;
export const dayTitle = (s: string) =>
  `${Number(s.slice(5, 7))} 月 ${Number(s.slice(8))} 日`;
/** 事项列表里的短日期：同年省略年份。 */
export const shortDate = (s: string, today: string) =>
  s.slice(0, 4) === today.slice(0, 4)
    ? `${Number(s.slice(5, 7))} 月 ${Number(s.slice(8))} 日`
    : `${s.slice(0, 4)} 年 ${Number(s.slice(5, 7))} 月 ${Number(s.slice(8))} 日`;

export function relativeTime(iso: string | null | undefined, now = Date.now()) {
  if (!iso) return "从未";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "—";
  const s = Math.max(0, Math.round((now - t) / 1000));
  if (s < 45) return "刚刚";
  if (s < 3600) return `${Math.round(s / 60)} 分钟前`;
  if (s < 86400) return `${Math.round(s / 3600)} 小时前`;
  if (s < 86400 * 7) return `${Math.round(s / 86400)} 天前`;
  return new Date(t).toLocaleDateString("zh-CN", {
    month: "numeric",
    day: "numeric",
  });
}

/**
 * 命令面板里的「跳转到日期」：认 2026-10-20、10-20、10/20、10月20日、20（本月）、
 * 今天/明天/后天/昨天。不认的返回 undefined，让面板回落到搜索。
 */
export function parseLooseDate(input: string, today: string): string | undefined {
  const text = input.trim();
  const year = Number(today.slice(0, 4));
  const valid = (y: number, m: number, d: number) => {
    const date = new Date(y, m - 1, d, 12);
    return date.getFullYear() === y &&
      date.getMonth() === m - 1 &&
      date.getDate() === d &&
      inRange(toISO(date))
      ? toISO(date)
      : undefined;
  };
  const relative: Record<string, number> = {
    昨天: -1,
    今天: 0,
    明天: 1,
    后天: 2,
  };
  if (text in relative) return addDays(today, relative[text]);
  let m = text.match(/^(\d{4})[-/.](\d{1,2})[-/.](\d{1,2})$/);
  if (m) return valid(+m[1], +m[2], +m[3]);
  m = text.match(/^(\d{1,2})[-/.月](\d{1,2})日?$/);
  if (m) return valid(year, +m[1], +m[2]);
  m = text.match(/^(\d{1,2})[日号]$/);
  if (m) return valid(year, Number(today.slice(5, 7)), +m[1]);
  return undefined;
}
