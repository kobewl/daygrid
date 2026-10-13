import { useCallback, useEffect, useState } from "react";
import { call, type DayMark } from "../api";
import { DayMarkCache } from "../lib/dayMarkCache";
import type { DayKind, MarkValue } from "../lib/workday";
import { useNotify } from "./useToasts";

/**
 * 手动标记的休息日 / 工作日（调休）。
 *
 * 读：随月历范围加载，已看过的日期留在内存里。
 * 写：先改界面（乐观更新），再串行写入服务；失败回滚并提示。
 * 这些标记只有桌面界面能改，所以不需要像事项那样轮询外部变化。
 */
export function useDayMarks(from: string, to: string) {
  const notify = useNotify();
  const [marks, setMarks] = useState<ReadonlyMap<string, DayKind>>(new Map());
  const [cache] = useState(
    () =>
      new DayMarkCache(
        {
          list: async (from, to) => {
            const r = await call<{ marks: DayMark[] }>(
              "desktop_list_day_marks",
              {
                from_date: from,
                to_date: to,
              },
            );
            return r.marks;
          },
          set: (date, value) =>
            call("desktop_set_day_mark", { date, kind: value }),
        },
        setMarks,
      ),
  );

  useEffect(() => {
    if (!from) return;
    let cancelled = false;
    cache.load(from, to).catch((e: Error) => {
      if (!cancelled)
        notify(`读取休息日标记失败：${e.message}`, { tone: "error" });
    });
    return () => {
      cancelled = true;
    };
  }, [cache, from, to, notify]);

  /** 把一天标为休息日 / 工作日；`default` 清除标记，回到按周末与法定假日推算。 */
  const setDayMark = useCallback(
    (date: string, value: MarkValue) => {
      return cache.set(date, value).catch((e: Error) => {
        notify(`标记失败：${e.message}`, { tone: "error" });
      });
    },
    [cache, notify],
  );

  return { dayMarks: marks, setDayMark };
}
