import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import ts from "typescript";

const source = readFileSync(
  new URL("../src/lib/dayMarkCache.ts", import.meta.url),
  "utf8",
);
const js = ts.transpileModule(source, {
  compilerOptions: {
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.ES2022,
  },
}).outputText;
const { DayMarkCache } = await import(
  `data:text/javascript;base64,${Buffer.from(js).toString("base64")}`
);

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

const date = "2026-10-13";
const other = "2026-10-14";
const from = "2026-10-01";
const to = "2026-10-31";

test("在月份读取期间标记一天，保留其余已存标记与最新选择", async () => {
  const read = deferred();
  const write = deferred();
  let displayed;
  const cache = new DayMarkCache(
    { list: () => read.promise, set: () => write.promise },
    (marks) => (displayed = marks),
  );
  const loading = cache.load(from, to);
  const saving = cache.set(date, "work");
  assert.equal(displayed.get(date), "work");
  read.resolve([
    { date, kind: "rest" },
    { date: other, kind: "rest" },
  ]);
  await loading;
  assert.equal(displayed.get(other), "rest");
  assert.equal(displayed.get(date), "work");
  write.resolve();
  await saving;
  assert.equal(displayed.get(date), "work");
  assert.equal(displayed.get(other), "rest");
});

for (const firstOk of [false, true]) {
  for (const secondOk of [false, true]) {
    test(`同日连续选择：第一笔${firstOk ? "成功" : "失败"}，第二笔${secondOk ? "成功" : "失败"}`, async () => {
      const writes = [deferred(), deferred()];
      const persisted = new Map();
      let displayed;
      let index = 0;
      const cache = new DayMarkCache(
        {
          list: async () => [],
          set: async (date, kind) => {
            await writes[index++].promise;
            persisted.set(date, kind);
          },
        },
        (marks) => (displayed = marks),
      );
      await cache.load(from, to);
      const first = cache.set(date, "rest");
      const firstSettled = first.catch(() => undefined);
      const second = cache.set(date, "work");
      const secondSettled = second.catch(() => undefined);
      assert.equal(displayed.get(date), "work");
      if (firstOk) writes[0].resolve();
      else writes[0].reject(new Error("first failed"));
      await firstSettled;
      assert.equal(displayed.get(date), "work", "早先结果不能覆盖后续选择");
      if (secondOk) writes[1].resolve();
      else writes[1].reject(new Error("second failed"));
      await secondSettled;
      assert.equal(displayed.get(date), persisted.get(date));
      assert.equal(
        displayed.get(date),
        secondOk ? "work" : firstOk ? "rest" : undefined,
      );
    });
  }
}

test("读取期间的清除失败时恢复已保存的标记", async () => {
  const read = deferred();
  let displayed;
  const cache = new DayMarkCache(
    {
      list: () => read.promise,
      set: async () => {
        throw new Error("save failed");
      },
    },
    (marks) => (displayed = marks),
  );
  const loading = cache.load(from, to);
  const saving = cache.set(date, "default");
  const failed = assert.rejects(saving, /save failed/);
  read.resolve([{ date, kind: "rest" }]);
  await loading;
  await failed;
  assert.equal(displayed.get(date), "rest");
});

test("读取排在写入后时使用最新保存结果，并保留其他月份缓存", async () => {
  const persisted = new Map([
    [date, "rest"],
    ["2026-11-02", "work"],
  ]);
  let displayed;
  const cache = new DayMarkCache(
    {
      list: async (from, to) =>
        [...persisted]
          .filter(([date]) => date >= from && date <= to)
          .map(([date, kind]) => ({ date, kind })),
      set: async (date, kind) => {
        if (kind === "default") persisted.delete(date);
        else persisted.set(date, kind);
      },
    },
    (marks) => (displayed = marks),
  );
  await cache.load("2026-11-01", "2026-11-30");
  const saving = cache.set(date, "default");
  const loading = cache.load(from, to);
  await Promise.all([saving, loading]);
  assert.equal(displayed.has(date), false);
  assert.equal(displayed.get("2026-11-02"), "work");
});

test("读取失败不会阻塞随后保存", async () => {
  let displayed;
  const cache = new DayMarkCache(
    {
      list: async () => {
        throw new Error("read failed");
      },
      set: async () => undefined,
    },
    (marks) => (displayed = marks),
  );
  const failed = assert.rejects(cache.load(from, to), /read failed/);
  const saving = cache.set(date, "rest");
  await Promise.all([failed, saving]);
  assert.equal(displayed.get(date), "rest");
});
