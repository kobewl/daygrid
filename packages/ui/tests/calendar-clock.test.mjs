import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import ts from "typescript";

const source = readFileSync(
  new URL("../src/lib/calendarClock.ts", import.meta.url),
  "utf8",
);
const js = ts.transpileModule(source, {
  compilerOptions: {
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.ES2022,
  },
}).outputText;
const { dateInTimeZone, millisecondsUntilNextDay } = await import(
  `data:text/javascript;base64,${Buffer.from(js).toString("base64")}`
);

test("同一时刻按日历时区分别计算日期", () => {
  const now = Date.parse("2026-10-12T16:00:00Z");
  assert.equal(dateInTimeZone("Asia/Shanghai", now), "2026-10-13");
  assert.equal(dateInTimeZone("UTC", now), "2026-10-12");
  assert.equal(dateInTimeZone("America/Los_Angeles", now), "2026-10-12");
  assert.equal(dateInTimeZone("Pacific/Kiritimati", now), "2026-10-13");
});

for (const [label, zone, start, end] of [
  [
    "上海跨日",
    "Asia/Shanghai",
    "2026-10-12T15:59:59.999Z",
    "2026-10-12T16:00:00Z",
  ],
  ["跨月", "Asia/Shanghai", "2026-10-31T15:30:00Z", "2026-10-31T16:00:00Z"],
  ["跨年", "Asia/Shanghai", "2026-12-31T15:30:00Z", "2026-12-31T16:00:00Z"],
  [
    "夏令时23小时日",
    "America/New_York",
    "2026-03-08T05:00:00Z",
    "2026-03-09T04:00:00Z",
  ],
  [
    "夏令时25小时日",
    "America/New_York",
    "2026-11-01T04:00:00Z",
    "2026-11-02T05:00:00Z",
  ],
]) {
  test(label + "：计时器落在精确日期边界", () => {
    const now = Date.parse(start);
    const delay = millisecondsUntilNextDay(zone, now);
    assert.equal(delay, Date.parse(end) - now);
    assert.equal(
      dateInTimeZone(zone, now + delay - 1),
      dateInTimeZone(zone, now),
    );
    assert.notEqual(
      dateInTimeZone(zone, now + delay),
      dateInTimeZone(zone, now),
    );
  });
}
