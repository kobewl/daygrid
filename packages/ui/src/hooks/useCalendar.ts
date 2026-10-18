import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  ApiError,
  call,
  key,
  preview,
  requestAttention,
  type Context,
  type Item,
  type Proposal,
  type Pulse,
  type Visibility,
} from "../api";
import {
  MAX_DATE,
  MIN_DATE,
  addMonths,
  clampDate,
  monthGrid,
  monthKey,
  type WeekStart,
} from "../lib/dates";
import { useDayMarks } from "./useDayMarks";
import { useNotify } from "./useToasts";
import { dateInTimeZone, millisecondsUntilNextDay } from "../lib/calendarClock";

export type ItemInput = Pick<Item, "date" | "title" | "notes" | "color"> & {
  /** 只有连接了团队日历时才会传；缺省时后端按「仅自己可见」创建。 */
  visibility?: Visibility;
  /** 本地串 YYYY-MM-DDTHH:MM（服务端按日历时区规范化）；null 清除提醒。 */
  remind_at?: string | null;
};
export type ItemPatch = Partial<Omit<ItemInput, "visibility">>;

const PULSE_MS = 2000;
const CONTEXT_EVERY = 30; // 定期核对设置；日期边界与回到前台另外立即核对。
const FRESH_MS = 1400; // 外部写入到达的高亮保留多久（需大于 CSS 的 arrive 动画时长）

const sortItems = (list: Item[]) =>
  list.sort(
    (a, b) =>
      Number(a.status === "done") - Number(b.status === "done") ||
      a.created_at.localeCompare(b.created_at) ||
      a.id.localeCompare(b.id),
  );

async function fetchRange(from: string, to: string): Promise<Item[]> {
  // 游标绑定在某个版本的日历上；翻页期间日历变了会得到 CURSOR_STALE，整页重来一次。
  for (let attempt = 0; ; attempt++) {
    try {
      const out: Item[] = [];
      let cursor: string | undefined;
      do {
        const r = await call<{ items: Item[]; next_cursor: string | null }>(
          "daygrid_list_items",
          { from_date: from, to_date: to, limit: 100, ...(cursor ? { cursor } : {}) },
        );
        out.push(...r.items);
        cursor = r.next_cursor ?? undefined;
      } while (cursor);
      return out;
    } catch (e) {
      if (attempt === 0 && e instanceof ApiError && e.code === "CURSOR_STALE") continue;
      throw e;
    }
  }
}

/**
 * 日历的全部数据与写操作。
 *
 * 读：只拉当前 6 周网格范围，并在空闲时预取前后各一个月；已看过的月份留在内存里，
 *     翻月即时显示，随后静默校对。界面靠 `desktop_pulse`（几个整数）判断是否有外部
 *     变化，而不是反复拉全量数据。
 * 写：先改本地（乐观更新），再进入一个串行队列调用服务；这样同一事项连续操作时
 *     版本号永远是最新的。失败会回滚并报错，队列清空后静默校对一次。
 */
export function useCalendar(weekStart: WeekStart) {
  const notify = useNotify();
  const [context, setContext] = useState<Context>();
  const [fatal, setFatal] = useState<string>();
  const [month, setMonth] = useState<string>();
  const [selected, setSelected] = useState<string>();
  const [items, setItems] = useState<Item[]>([]);
  const [loading, setLoading] = useState(true);
  const [proposals, setProposals] = useState<Proposal[]>([]);
  const [pulse, setPulse] = useState<Pulse>();
  const [hidden, setHidden] = useState<ReadonlySet<string>>(new Set());
  // 刚由外部（助手、批准的请求）写入而到达界面的事项，用于一次性的到达动效。
  const [fresh, setFresh] = useState<ReadonlySet<string>>(new Set());
  const freshTimer = useRef<ReturnType<typeof setTimeout>>();
  useEffect(() => () => clearTimeout(freshTimer.current), []);

  const grid = useMemo(() => (month ? monthGrid(month, weekStart) : []), [month, weekStart]);
  const from = grid.length ? clampDate(grid[0]) : "";
  const to = grid.length ? clampDate(grid[grid.length - 1]) : "";
  const { dayMarks, setDayMark } = useDayMarks(from, to);

  // 异步回调里要读最新值，用 ref 避免闭包过期。
  const ctxRef = useRef<Context>();
  const followingToday = useRef(true);
  const navigation = useRef(0);
  const contextRequest = useRef<Promise<Context | undefined>>();
  const itemsRef = useRef<Item[]>([]);
  const rangeRef = useRef({ from, to, month: month ?? "", weekStart });
  const epoch = useRef(0);
  const queue = useRef<Promise<void>>(Promise.resolve());
  const inflight = useRef(0);
  // macOS 上关窗只是隐藏（外壳发 daygrid://window-hidden / shown）；WebKit 的 document.hidden
  // 不一定跟着变，所以两者任一成立都停止脉冲和跨日计时器，让服务能照常空闲退出。
  const appHidden = useRef(false);
  const suspended = useCallback(() => document.hidden || appHidden.current, []);
  ctxRef.current = context;
  itemsRef.current = items;
  rangeRef.current = { from, to, month: month ?? "", weekStart };

  const commit = useCallback((updater: (prev: Item[]) => Item[]) => {
    setItems((prev) => {
      const next = updater(prev);
      itemsRef.current = next;
      return next;
    });
  }, []);

  const applyContext = useCallback((c: Context) => {
    const previous = ctxRef.current;
    ctxRef.current = c;
    setContext(c);
    if (previous && previous.today !== c.today && followingToday.current) {
      setSelected(c.today);
      setMonth(monthKey(c.today));
    }
  }, []);

  const refreshContext = useCallback(() => {
    // focus、visibilitychange 和跨日计时器可能同时到达，只保留一个请求。
    if (contextRequest.current) return contextRequest.current;
    const request = call<Context>("daygrid_get_context")
      .then((c) => {
        applyContext(c);
        return c;
      })
      .catch(() => undefined)
      .finally(() => { contextRequest.current = undefined; });
    contextRequest.current = request;
    return request;
  }, [applyContext]);

  const loadRange = useCallback(
    async (rangeFrom: string, rangeTo: string, markFresh = false) => {
      const e = epoch.current;
      const fetched = await fetchRange(rangeFrom, rangeTo);
      // 请求期间发生过本地写入：这份数据可能已过时，丢弃（写入结束后会再校对）。
      if (e !== epoch.current) return false;
      if (markFresh) {
        // 只在「外部变更」路径标记：本地乐观写入完成后版本号与服务端一致，不会被误判。
        const known = new Map(itemsRef.current.map((i) => [i.id, i.revision]));
        const arrived = fetched
          .filter((i) => known.get(i.id) !== i.revision)
          .map((i) => i.id);
        if (arrived.length) {
          setFresh(new Set(arrived));
          clearTimeout(freshTimer.current);
          freshTimer.current = setTimeout(() => setFresh(new Set()), FRESH_MS);
        }
      }
      commit((prev) => [
        ...prev.filter((i) => i.date < rangeFrom || i.date > rangeTo),
        ...fetched,
      ]);
      return true;
    },
    [commit],
  );

  const prefetch = useCallback(() => {
    const { month: m, weekStart: w } = rangeRef.current;
    if (!m) return;
    for (const delta of [1, -1]) {
      const other = addMonths(m + "-01", delta).slice(0, 7);
      if (other < MIN_DATE.slice(0, 7) || other > MAX_DATE.slice(0, 7)) continue;
      const g = monthGrid(other, w);
      void loadRange(clampDate(g[0]), clampDate(g[g.length - 1])).catch(() => undefined);
    }
  }, [loadRange]);

  /** `markFresh`：这次刷新是为了拉取外部写入的结果，变化的事项会触发到达动效。 */
  const refresh = useCallback(async (markFresh = false) => {
    const { from: f, to: t } = rangeRef.current;
    if (!f) return;
    try {
      await loadRange(f, t, markFresh);
    } catch (e) {
      notify((e as Error).message, { tone: "error" });
    }
  }, [loadRange, notify]);

  const refreshProposals = useCallback(async () => {
    try {
      setProposals((await call<{ proposals: Proposal[] }>("desktop_proposals")).proposals);
    } catch {
      // 下一次脉冲会重试。
    }
  }, []);

  // 首次加载：上下文 → 以「今天」为中心。
  const boot = useCallback(async () => {
    setFatal(undefined);
    try {
      const c = await call<Context>("daygrid_get_context");
      applyContext(c);
      setMonth((m) => m ?? monthKey(c.today));
      setSelected((s) => s ?? c.today);
      const p = await call<Pulse>("desktop_pulse");
      setPulse(p);
      void refreshProposals();
    } catch (e) {
      setFatal((e as Error).message);
    }
  }, [applyContext, refreshProposals]);
  useEffect(() => void boot(), [boot]);

  // 常驻前台时在日历时区的午夜核对；后台休眠后由前台事件补上。
  useEffect(() => {
    if (!context) return;
    let delay: number;
    try {
      delay = millisecondsUntilNextDay(context.calendar_timezone);
    } catch {
      return; // 服务端仍是日期真值；不支持的本机时区交给定期刷新。
    }
    const timer = setTimeout(() => {
      // 挂起时跳过：恢复时的 tick(true) 会刷新上下文，不会漏掉跨日。
      if (!suspended()) void refreshContext();
    }, delay + 25);
    return () => clearTimeout(timer);
  }, [context?.today, context?.calendar_timezone, refreshContext, suspended]);

  // 范围变化（翻月 / 改周起始日）时加载，并在空闲时预取相邻月份。
  useEffect(() => {
    if (!from) return;
    let cancelled = false;
    const run = async () => {
      try {
        await loadRange(from, to);
      } catch (e) {
        if (!cancelled) notify((e as Error).message, { tone: "error" });
      } finally {
        if (!cancelled) setLoading(false);
      }
      if (!cancelled) {
        const idle = (window as any).requestIdleCallback as
          | ((cb: () => void) => void)
          | undefined;
        idle ? idle(prefetch) : setTimeout(prefetch, 200);
      }
    };
    void run();
    return () => {
      cancelled = true;
    };
  }, [from, to, loadRange, prefetch, notify]);

  // 脉冲轮询：几个整数，变了才拉数据。窗口不可见或被隐藏时完全停下（不发请求，也不挂计时器），
  // 回到前台立即检查。
  const lastPulse = useRef<Pulse>();
  const lastPending = useRef(0);
  useEffect(() => {
    if (!context) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let ticks = 0;
    let ticking = false;
    const tick = async (refreshClock = false) => {
      if (stopped) return;
      if (refreshClock) void refreshContext();
      if (ticking) return;
      ticking = true;
      clearTimeout(timer);
      if (!suspended()) {
        try {
          const c = ctxRef.current;
          let dateChanged = false;
          try {
            dateChanged = !!c && dateInTimeZone(c.calendar_timezone) !== c.today;
          } catch { /* 本机时区数据不支持时仍核对服务端。 */ }
          if (dateChanged || ++ticks % CONTEXT_EVERY === 0) await refreshContext();
          const p = await call<Pulse>("desktop_pulse");
          const prev = lastPulse.current;
          lastPulse.current = p;
          setPulse((old) => (old && JSON.stringify(old) === JSON.stringify(p) ? old : p));
          if (prev && p.change_sequence !== prev.change_sequence && inflight.current === 0) {
            // 外部修改：只保留当前范围的缓存，重新校对。
            const { from: f, to: t } = rangeRef.current;
            commit((all) => all.filter((i) => i.date >= f && i.date <= t));
            void refresh(true).then(prefetch);
          }
          if (prev && (p.proposal_rev !== prev.proposal_rev || p.pending_proposals !== prev.pending_proposals)) {
            void refreshProposals();
          }
          if (p.pending_proposals > lastPending.current && prev) {
            if (!document.hasFocus()) requestAttention();
          }
          lastPending.current = p.pending_proposals;
        } catch {
          // 服务暂时不可用：保持现状，下一轮再试。
        }
      }
      ticking = false;
      if (!stopped && !suspended()) timer = setTimeout(tick, PULSE_MS);
    };
    const onVisible = (e: Event) => {
      // 收到 focus 说明窗口已经可见。
      if (e.type === "focus") appHidden.current = false;
      if (!suspended()) {
        clearTimeout(timer);
        void tick(true);
      }
    };
    document.addEventListener("visibilitychange", onVisible);
    window.addEventListener("focus", onVisible);
    window.addEventListener("pageshow", onVisible);
    // 订阅失败（或浏览器预览）时只靠 document.hidden。
    let unlisten: (() => void)[] = [];
    if (!preview) {
      void Promise.allSettled([
        listen("daygrid://window-hidden", () => {
          appHidden.current = true;
          clearTimeout(timer);
        }),
        listen("daygrid://window-shown", () => {
          appHidden.current = false;
          clearTimeout(timer);
          void tick(true);
        }),
      ]).then((results) => {
        const fns = results.flatMap((r) => (r.status === "fulfilled" ? [r.value] : []));
        if (stopped) fns.forEach((f) => f());
        else unlisten = fns;
      });
    }
    timer = setTimeout(tick, PULSE_MS);
    return () => {
      stopped = true;
      unlisten.forEach((f) => f());
      clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisible);
      window.removeEventListener("focus", onVisible);
      window.removeEventListener("pageshow", onVisible);
    };
  }, [context === undefined, commit, refresh, prefetch, refreshProposals, refreshContext, suspended]); // eslint-disable-line react-hooks/exhaustive-deps

  // ───────────── 导航 ─────────────
  const selectDate = useCallback((date: string) => {
    const d = clampDate(date);
    navigation.current++;
    const c = ctxRef.current;
    try {
      followingToday.current = !!c && d === dateInTimeZone(c.calendar_timezone);
    } catch {
      followingToday.current = d === c?.today;
    }
    setSelected(d);
  }, []);
  const goDate = useCallback((date: string) => {
    const d = clampDate(date);
    selectDate(d);
    setMonth(monthKey(d));
  }, [selectDate]);
  const goToday = useCallback(async () => {
    const intent = ++navigation.current;
    const c = await refreshContext();
    // 请求途中用户又选了其他日期，保留最新的手动操作。
    if (navigation.current !== intent) return;
    if (c) {
      goDate(c.today);
      followingToday.current = true;
    } else notify("暂时无法获取今天的日期，请稍后再试。", { tone: "error" });
  }, [refreshContext, goDate, notify]);
  const shiftMonth = useCallback(
    (delta: number) => {
      navigation.current++;
      followingToday.current = false;
      setSelected((s) => {
        const base = s ?? ctxRef.current?.today ?? MIN_DATE;
        const next = clampDate(addMonths(base, delta));
        setMonth(monthKey(next));
        return next;
      });
    },
    [],
  );

  // ───────────── 写操作 ─────────────
  const enqueue = useCallback(<T,>(task: () => Promise<T>) => {
    inflight.current++;
    const p = queue.current.then(task);
    queue.current = p.then(
      () => undefined,
      () => undefined,
    );
    return p.finally(() => {
      if (--inflight.current === 0) void refresh();
    });
  }, [refresh]);

  const base = useCallback(() => {
    const c = ctxRef.current;
    if (!c) throw new ApiError("SERVICE_UNAVAILABLE", "日历尚未就绪。");
    return { calendar_generation: c.calendar_generation, idempotency_key: key() };
  }, []);

  const replace = useCallback(
    (item: Item) =>
      commit((prev) => prev.map((i) => (i.id === item.id ? item : i))),
    [commit],
  );

  const createItem = useCallback(
    (input: ItemInput) =>
      enqueue(async () => {
        const r = await call<{ item: Item }>("daygrid_create_item", { ...base(), ...input });
        epoch.current++;
        commit((prev) => [...prev, r.item]);
        return r.item;
      }),
    [enqueue, base, commit],
  );

  /** `revision` 显式给出时按它做冲突检测（编辑对话框：以打开时的版本为准）。 */
  const updateItem = useCallback(
    (id: string, patch: ItemPatch, options: { revision?: number } = {}) => {
      epoch.current++;
      const before = itemsRef.current.find((i) => i.id === id);
      if (before && options.revision === undefined)
        replace({ ...before, ...patch });
      return enqueue(async () => {
        const current = itemsRef.current.find((i) => i.id === id);
        const revision = options.revision ?? current?.revision;
        if (!revision) throw new ApiError("NOT_FOUND", "事项不存在。");
        const r = await call<{ item: Item }>("daygrid_update_item", {
          ...base(),
          item_id: id,
          expected_revision: revision,
          patch,
        });
        epoch.current++;
        commit((prev) =>
          prev.some((i) => i.id === id)
            ? prev.map((i) => (i.id === id ? r.item : i))
            : [...prev, r.item],
        );
        return r.item;
      });
    },
    [enqueue, base, commit, replace],
  );

  const setStatus = useCallback(
    (id: string, status: Item["status"]) => {
      epoch.current++;
      const before = itemsRef.current.find((i) => i.id === id);
      if (before) replace({ ...before, status });
      return enqueue(async () => {
        const current = itemsRef.current.find((i) => i.id === id);
        if (!current) throw new ApiError("NOT_FOUND", "事项不存在。");
        const r = await call<{ item: Item }>("daygrid_set_item_status", {
          ...base(),
          item_id: id,
          expected_revision: current.revision,
          status,
        });
        epoch.current++;
        replace(r.item);
        return r.item;
      });
    },
    [enqueue, base, replace],
  );

  /** 乐观删除：先从界面隐藏，服务确认后才真正移除；失败则自动恢复显示。 */
  const removeItem = useCallback(
    (id: string) => {
      epoch.current++;
      setHidden((h) => new Set(h).add(id));
      return enqueue(async () => {
        const current = itemsRef.current.find((i) => i.id === id);
        try {
          if (!current) throw new ApiError("NOT_FOUND", "事项不存在。");
          const r = await call<{ item: Item }>("desktop_delete", {
            ...base(),
            item_id: id,
            expected_revision: current.revision,
          });
          epoch.current++;
          commit((prev) => prev.filter((i) => i.id !== id));
          return r.item;
        } finally {
          setHidden((h) => {
            const next = new Set(h);
            next.delete(id);
            return next;
          });
        }
      });
    },
    [enqueue, base, commit],
  );

  const restoreItem = useCallback(
    (itemId: string, revision: number) =>
      enqueue(async () => {
        const r = await call<{ item: Item }>("desktop_restore", {
          ...base(),
          item_id: itemId,
          expected_revision: revision,
        });
        epoch.current++;
        commit((prev) => (prev.some((i) => i.id === itemId) ? prev : [...prev, r.item]));
        return r.item;
      }),
    [enqueue, base, commit],
  );

  /** 改变可见范围；`revision` 缺省时用本地最新版本。 */
  const setVisibility = useCallback(
    (id: string, visibility: Visibility, options: { revision?: number } = {}) =>
      enqueue(async () => {
        const current = itemsRef.current.find((i) => i.id === id);
        const revision = options.revision ?? current?.revision;
        if (!revision) throw new ApiError("NOT_FOUND", "事项不存在。");
        const r = await call<{ item: Item }>("desktop_set_visibility", {
          ...base(),
          item_id: id,
          expected_revision: revision,
          visibility,
        });
        epoch.current++;
        commit((prev) => prev.map((i) => (i.id === id ? r.item : i)));
        return r.item;
      }),
    [enqueue, base, commit],
  );

  const decide = useCallback(
    async (proposalId: string, decision: "approve" | "reject") => {
      const r = await call<{ proposal: Proposal }>("desktop_decide_proposal", {
        proposal_id: proposalId,
        decision,
      });
      epoch.current++;
      // 批准后服务端才真正执行，结果等同于一次外部写入，让它「到达」得看得见。
      await Promise.all([refreshProposals(), refresh(true)]);
      return r.proposal;
    },
    [refreshProposals, refresh],
  );

  const itemsByDate = useMemo(() => {
    const map = new Map<string, Item[]>();
    for (const item of items) {
      if (hidden.has(item.id)) continue;
      const list = map.get(item.date);
      if (list) list.push(item);
      else map.set(item.date, [item]);
    }
    for (const list of map.values()) sortItems(list);
    return map;
  }, [items, hidden]);

  const pending = useMemo(() => proposals.filter((p) => p.state === "pending"), [proposals]);

  return {
    context,
    fatal,
    retry: boot,
    month,
    grid,
    selected,
    setSelected: selectDate,
    goDate,
    goToday,
    shiftMonth,
    items,
    itemsByDate,
    fresh,
    loading,
    proposals,
    pending,
    pulse,
    refresh,
    refreshContext,
    refreshProposals,
    createItem,
    updateItem,
    setVisibility,
    setStatus,
    removeItem,
    restoreItem,
    decide,
    dayMarks,
    setDayMark,
  };
}
export type Calendar = ReturnType<typeof useCalendar>;
