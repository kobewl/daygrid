import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import ts from "typescript";

const source = readFileSync(
  new URL("../src/lib/reminder.ts", import.meta.url),
  "utf8",
);
const js = ts.transpileModule(source, {
  compilerOptions: {
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.ES2022,
  },
}).outputText;
const {
  toInput,
  splitReminder,
  joinReminder,
  nowInZone,
  isPast,
  defaultReminder,
  shiftReminder,
  reminderLabel,
  zhDateTime,
  sameRemindAt,
  reminderDirty,
  applyReminderEdit,
} = await import(
  `data:text/javascript;base64,${Buffer.from(js).toString("base64")}`
);

const SH = "Asia/Shanghai";
// 2026-10-20 14:37 上海时间
const AFTERNOON = Date.parse("2026-10-20T06:37:12Z");

test("toInput 把规范串切成本地分钟", () => {
  assert.equal(toInput("2026-10-20T09:00:00+08:00"), "2026-10-20T09:00");
  assert.equal(toInput(null), null);
  assert.equal(toInput(undefined), null);
});

test("splitReminder / joinReminder 互转，含不完整情况", () => {
  assert.deepEqual(splitReminder("2026-10-20T09:00"), { date: "2026-10-20", time: "09:00" });
  assert.equal(splitReminder(null), null);
  assert.equal(joinReminder({ date: "2026-10-20", time: "09:00" }), "2026-10-20T09:00");
  assert.equal(joinReminder(null), null);
  assert.equal(joinReminder({ date: "2026-10-20", time: "" }), undefined);
  assert.equal(joinReminder({ date: "", time: "09:00" }), undefined);
});

test("nowInZone 按日历时区计算，使用 24 小时制", () => {
  const t = Date.parse("2026-10-20T16:05:00Z");
  assert.equal(nowInZone(SH, t), "2026-10-21T00:05");
  assert.equal(nowInZone("America/New_York", t), "2026-10-20T12:05");
  assert.equal(nowInZone("UTC", Date.parse("2026-10-20T00:00:30Z")), "2026-10-20T00:00");
});

test("isPast 严格小于：同一分钟不算过去", () => {
  assert.equal(isPast("2026-10-20T14:37", SH, AFTERNOON), false);
  assert.equal(isPast("2026-10-20T14:36", SH, AFTERNOON), true);
  assert.equal(isPast("2026-10-20T14:38", SH, AFTERNOON), false);
});

test("defaultReminder：今天取下一个整点，以后取 09:00，过去改用今天", () => {
  assert.deepEqual(defaultReminder("2026-10-20", SH, AFTERNOON), { date: "2026-10-20", time: "15:00" });
  assert.deepEqual(defaultReminder("2026-10-25", SH, AFTERNOON), { date: "2026-10-25", time: "09:00" });
  assert.deepEqual(defaultReminder("2026-10-01", SH, AFTERNOON), { date: "2026-10-20", time: "15:00" });
});

test("defaultReminder：下一个整点跨过午夜时取 23:59", () => {
  const late = Date.parse("2026-10-20T15:30:00Z"); // 上海 23:30
  assert.deepEqual(defaultReminder("2026-10-20", SH, late), { date: "2026-10-20", time: "23:59" });
  const ten = Date.parse("2026-10-20T14:00:00Z"); // 上海 22:00
  assert.deepEqual(defaultReminder("2026-10-20", SH, ten), { date: "2026-10-20", time: "23:00" });
});

test("shiftReminder 按天数平移，时刻不变，可跨月跨年", () => {
  assert.equal(shiftReminder("2026-10-20T09:00", "2026-10-20", "2026-10-21"), "2026-10-21T09:00");
  assert.equal(shiftReminder("2026-10-19T21:30", "2026-10-20", "2026-11-02"), "2026-11-01T21:30");
  assert.equal(shiftReminder("2026-12-31T08:00", "2026-12-31", "2027-01-01"), "2027-01-01T08:00");
  assert.equal(shiftReminder("2026-03-01T08:00", "2026-03-01", "2026-02-28"), "2026-02-28T08:00");
  assert.equal(shiftReminder("2026-10-20T09:00", "2026-10-20", "2026-10-20"), "2026-10-20T09:00");
});

test("zhDateTime 月日不补零", () => {
  assert.equal(zhDateTime("2026-10-20T09:00"), "10月20日 09:00");
  assert.equal(zhDateTime("2026-03-05T18:30"), "3月5日 18:30");
});

test("reminderLabel：当天只显示时刻，其他日期用紧凑写法", () => {
  assert.deepEqual(reminderLabel("2026-10-20T09:00", "2026-10-20"), {
    label: "09:00",
    full: "提醒 10月20日 09:00",
  });
  assert.deepEqual(reminderLabel("2026-10-19T09:00", "2026-10-20"), {
    label: "10/19 09:00",
    full: "提醒 10月19日 09:00",
  });
});

test("sameRemindAt：undefined 等于 null，只比较到分钟", () => {
  assert.equal(sameRemindAt(undefined, null), true);
  assert.equal(sameRemindAt(null, null), true);
  assert.equal(sameRemindAt("2026-10-20T09:00:00+08:00", "2026-10-20T09:00"), true);
  assert.equal(sameRemindAt("2026-10-20T09:00:30+08:00", "2026-10-20T09:00:00+09:00"), true);
  assert.equal(sameRemindAt("2026-10-20T09:00", null), false);
  assert.equal(sameRemindAt("2026-10-20T09:00", "2026-10-20T09:01"), false);
});

test("reminderDirty：没有提醒且没动过不算改动，不完整算改动", () => {
  assert.equal(reminderDirty(null, null), false);
  assert.equal(reminderDirty({ date: "2026-10-20", time: "" }, null), true);
  assert.equal(reminderDirty({ date: "2026-10-20", time: "09:00" }, "2026-10-20T09:00"), false);
  assert.equal(reminderDirty({ date: "2026-10-20", time: "10:00" }, "2026-10-20T09:00"), true);
  assert.equal(reminderDirty(null, "2026-10-20T09:00"), true);
});

const draft = (over = {}) => ({
  date: "2026-10-20",
  reminder: { date: "2026-10-20", time: "09:00" },
  remindTouched: false,
  title: "交周报",
  ...over,
});

test("applyReminderEdit：改时间、改日期都标记为改过", () => {
  const t = applyReminderEdit(draft(), { type: "time", value: "10:30" });
  assert.deepEqual(t.reminder, { date: "2026-10-20", time: "10:30" });
  assert.equal(t.remindTouched, true);
  assert.equal(t.title, "交周报");
  const d = applyReminderEdit(draft(), { type: "date", value: "2026-10-19" });
  assert.deepEqual(d.reminder, { date: "2026-10-19", time: "09:00" });
  assert.equal(d.remindTouched, true);
  const cleared = applyReminderEdit(draft(), { type: "time", value: "" });
  assert.equal(joinReminder(cleared.reminder), undefined);
});

test("applyReminderEdit：取消提醒后为 null 且标记为改过", () => {
  const c = applyReminderEdit(draft(), { type: "clear" });
  assert.equal(c.reminder, null);
  assert.equal(c.remindTouched, true);
});

test("applyReminderEdit：enable 填入默认值并标记为改过", () => {
  const e = applyReminderEdit(draft({ reminder: null, date: "2026-10-25" }), {
    type: "enable",
    timeZone: SH,
    now: AFTERNOON,
  });
  assert.deepEqual(e.reminder, { date: "2026-10-25", time: "09:00" });
  assert.equal(e.remindTouched, true);
});

test("applyReminderEdit：改事项日期时平移提醒，不改 remindTouched", () => {
  const s = applyReminderEdit(draft(), { type: "itemDate", value: "2026-10-22" });
  assert.equal(s.date, "2026-10-22");
  assert.deepEqual(s.reminder, { date: "2026-10-22", time: "09:00" });
  assert.equal(s.remindTouched, false);
  const kept = applyReminderEdit(draft({ remindTouched: true }), { type: "itemDate", value: "2026-10-21" });
  assert.equal(kept.remindTouched, true);
  assert.deepEqual(kept.reminder, { date: "2026-10-21", time: "09:00" });
  const none = applyReminderEdit(draft({ reminder: null }), { type: "itemDate", value: "2026-10-21" });
  assert.equal(none.reminder, null);
  assert.equal(none.remindTouched, false);
  // 不完整的提醒和被清空的日期都不平移。
  const partial = { date: "2026-10-20", time: "" };
  assert.deepEqual(
    applyReminderEdit(draft({ reminder: partial }), { type: "itemDate", value: "2026-10-21" }).reminder,
    partial,
  );
  const blank = applyReminderEdit(draft(), { type: "itemDate", value: "" });
  assert.equal(blank.date, "");
  assert.deepEqual(blank.reminder, { date: "2026-10-20", time: "09:00" });
});

test("applyReminderEdit：事项日期经过空值时，仍按最近一次有效日期平移", () => {
  const itemDate = (d, value) => applyReminderEdit(d, { type: "itemDate", value });
  // A → "" → B：平移 B − A = 5 天，仍不算用户改过。
  const blank = itemDate(draft(), "");
  const b = itemDate(blank, "2026-10-25");
  assert.equal(b.date, "2026-10-25");
  assert.deepEqual(b.reminder, { date: "2026-10-25", time: "09:00" });
  assert.equal(b.remindTouched, false);
  // 连续两次空值也一样。
  const twice = itemDate(itemDate(itemDate(draft(), ""), ""), "2026-10-25");
  assert.deepEqual(twice.reminder, { date: "2026-10-25", time: "09:00" });
  // 提醒和事项不在同一天：保持相对天数。
  const offset = itemDate(itemDate(draft({ reminder: { date: "2026-10-19", time: "21:30" } }), ""), "2026-11-02");
  assert.deepEqual(offset.reminder, { date: "2026-11-01", time: "21:30" });
  // 直接改「提醒」一栏仍然标记为改过，之后改事项日期照常平移。
  const timed = applyReminderEdit(blank, { type: "time", value: "10:00" });
  assert.equal(timed.remindTouched, true);
  const moved = itemDate(timed, "2026-10-22");
  assert.deepEqual(moved.reminder, { date: "2026-10-22", time: "10:00" });
  assert.equal(moved.remindTouched, true);
  const dated = applyReminderEdit(b, { type: "date", value: "2026-10-24" });
  assert.deepEqual(dated.reminder, { date: "2026-10-24", time: "09:00" });
  assert.equal(dated.remindTouched, true);
});
