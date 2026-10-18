// 提醒相关的纯函数。这里不能有任何运行时导入：测试用 data URL 直接加载这个文件。
//
// 界面里的提醒一律用日历时区的本地串 "YYYY-MM-DDTHH:MM"（下文称 local）；
// 服务端存的是带偏移的规范串 "YYYY-MM-DDTHH:MM:00+08:00"，同样落在日历时区，
// 所以前 16 位就是 local。

/** 对话框里的两个输入框；任一为空表示「不完整」。 */
export type ReminderParts = { date: string; time: string };

/** 对话框草稿里与提醒有关的部分（`date` 是事项日期）。 */
export type ReminderDraft = {
  date: string;
  /** null = 不提醒。 */
  reminder: ReminderParts | null;
  /** 用户直接动过「提醒」一栏；只改事项日期带来的平移不算。 */
  remindTouched: boolean;
};

export type ReminderAction =
  | { type: "enable"; timeZone: string; now?: number }
  | { type: "date"; value: string }
  | { type: "time"; value: string }
  | { type: "clear" }
  | { type: "itemDate"; value: string };

const pad = (n: number) => String(n).padStart(2, "0");

/** 规范串 → local；没有提醒时为 null。 */
export const toInput = (remindAt?: string | null): string | null =>
  remindAt ? remindAt.slice(0, 16) : null;

export const splitReminder = (local: string | null): ReminderParts | null =>
  local ? { date: local.slice(0, 10), time: local.slice(11, 16) } : null;

/** null → null（不提醒）；日期或时间任一为空 → undefined（不完整）。 */
export const joinReminder = (r: ReminderParts | null): string | null | undefined =>
  r === null ? null : r.date && r.time ? `${r.date}T${r.time}` : undefined;

const formatters = new Map<string, Intl.DateTimeFormat>();

/** 日历时区的当前时间（local 格式）。写法与 calendarClock.ts 一致。 */
export function nowInZone(timeZone: string, now = Date.now()): string {
  let formatter = formatters.get(timeZone);
  if (!formatter) {
    const options: Intl.DateTimeFormatOptions = {
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      hourCycle: "h23",
    };
    try {
      formatter = new Intl.DateTimeFormat("en-US", { ...options, timeZone });
    } catch {
      // 本机不认识这个时区：退回电脑时区。这里只用于界面提示，服务端仍然是权威。
      formatter = new Intl.DateTimeFormat("en-US", options);
    }
    formatters.set(timeZone, formatter);
  }
  const parts = new Map(formatter.formatToParts(now).map((p) => [p.type, p.value]));
  return `${parts.get("year")}-${parts.get("month")}-${parts.get("day")}T${parts.get("hour")}:${parts.get("minute")}`;
}

/** 严格早于当前分钟才算过去：同一分钟内的时间仍可保存（服务端有 60 秒容差）。 */
export const isPast = (local: string, timeZone: string, now?: number) =>
  local < nowInZone(timeZone, now);

/**
 * 点击「不提醒」后的默认值（FR-2.2）：事项在今天取下一个整点，在以后取 09:00；
 * 事项日期已经过去时改用今天的下一个整点。下一个整点跨过午夜时取 23:59。
 */
export function defaultReminder(itemDate: string, timeZone: string, now?: number): ReminderParts {
  const current = nowInZone(timeZone, now);
  const today = current.slice(0, 10);
  if (itemDate > today) return { date: itemDate, time: "09:00" };
  const next = Number(current.slice(11, 13)) + 1;
  return { date: today, time: next > 23 ? "23:59" : `${pad(next)}:00` };
}

const dayNumber = (date: string) => {
  const [y, m, d] = date.split("-").map(Number);
  return Date.UTC(y, m - 1, d) / 86_400_000;
};

/** 事项日期从 oldDate 改到 newDate 时，提醒按同样的天数平移，时刻不变（与服务端一致）。 */
export function shiftReminder(local: string, oldDate: string, newDate: string): string {
  const days = dayNumber(newDate) - dayNumber(oldDate);
  if (!Number.isFinite(days) || days === 0) return local;
  const d = new Date((dayNumber(local.slice(0, 10)) + days) * 86_400_000);
  if (Number.isNaN(d.getTime())) return local;
  return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}${local.slice(10)}`;
}

/** 「10月20日 09:00」，与 Rust 的 zh_datetime 一致（月日不补零）。 */
export const zhDateTime = (local: string) =>
  `${Number(local.slice(5, 7))}月${Number(local.slice(8, 10))}日 ${local.slice(11, 16)}`;

/** 角标文字：提醒在事项当天时只显示时刻，否则用紧凑写法「10/19 09:00」；full 给无障碍和 title 用。 */
export function reminderLabel(local: string, itemDate: string): { label: string; full: string } {
  const time = local.slice(11, 16);
  const label =
    local.slice(0, 10) === itemDate
      ? time
      : `${Number(local.slice(5, 7))}/${Number(local.slice(8, 10))} ${time}`;
  return { label, full: `提醒 ${zhDateTime(local)}` };
}

/** 旧记录里是 undefined、新记录里是 null，都视为「不提醒」；只比较到分钟。 */
export const sameRemindAt = (a?: string | null, b?: string | null) =>
  (a ?? null)?.slice(0, 16) === (b ?? null)?.slice(0, 16);

/** 对话框的提醒一栏是否有改动；不完整也算改动（关闭时要确认）。 */
export function reminderDirty(reminder: ReminderParts | null, originalRemindAt: string | null): boolean {
  const joined = joinReminder(reminder);
  return joined === undefined || !sameRemindAt(joined, originalRemindAt);
}

/**
 * 对话框里与提醒有关的每一次修改都经过这里。用户对「提醒」一栏的直接操作都会把
 * remindTouched 设为 true（保存时才会把 remind_at 放进 patch）；只改事项日期时
 * 提醒跟着平移，但不算用户改过，交给服务端按同样的规则平移。
 */
export function applyReminderEdit<D extends ReminderDraft>(draft: D, action: ReminderAction): D {
  switch (action.type) {
    case "enable":
      return {
        ...draft,
        reminder: defaultReminder(draft.date, action.timeZone, action.now),
        remindTouched: true,
      };
    case "date":
    case "time":
      return {
        ...draft,
        reminder: { date: "", time: "", ...draft.reminder, [action.type]: action.value },
        remindTouched: true,
      };
    case "clear":
      return { ...draft, reminder: null, remindTouched: true };
    case "itemDate": {
      const local = joinReminder(draft.reminder);
      const shift = typeof local === "string" && draft.date && action.value;
      return {
        ...draft,
        date: action.value,
        reminder: shift ? splitReminder(shiftReminder(local, draft.date, action.value)) : draft.reminder,
      };
    }
  }
}
