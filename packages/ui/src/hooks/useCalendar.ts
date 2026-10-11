import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ApiError,
  call,
  key,
  requestAttention,
  type Context,
  type Item,
  type Proposal,
  type Pulse,
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
import { useNotify } from "./useToasts";

export type ItemInput = Pick<Item, "date" | "title" | "notes" | "color">;
export type ItemPatch = Partial<ItemInput>;

const PULSE_MS = 2000;
const CONTEXT_EVERY = 30; // 每 30 次脉冲顺带刷新一次上下文（跨午夜时 today 会变）

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

  const grid = useMemo(() => (month ? monthGrid(month, weekStart) : []), [month, weekStart]);
  const from = grid.length ? clampDate(grid[0]) : "";
  const to = grid.length ? clampDate(grid[grid.length - 1]) : "";

  // 异步回调里要读最新值，用 ref 避免闭包过期。
  const ctxRef = useRef<Context>();
  const itemsRef = useRef<Item[]>([]);
  const rangeRef = useRef({ from, to, month: month ?? "", weekStart });
  const epoch = useRef(0);
  const queue = useRef<Promise<void>>(Promise.resolve());
  const inflight = useRef(0);
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

  const refreshContext = useCallback(async () => {
    try {
      const c = await call<Context>("daygrid_get_context");
      setContext(c);
      return c;
    } catch (e) {
      return undefined;
    }
  }, []);

  const loadRange = useCallback(
    async (rangeFrom: string, rangeTo: string) => {
      const e = epoch.current;
      const fetched = await fetchRange(rangeFrom, rangeTo);
      // 请求期间发生过本地写入：这份数据可能已过时，丢弃（写入结束后会再校对）。
      if (e !== epoch.current) return false;
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

  const refresh = useCallback(async () => {
    const { from: f, to: t } = rangeRef.current;
    if (!f) return;
    try {
      await loadRange(f, t);
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
      setContext(c);
      setMonth((m) => m ?? monthKey(c.today));
      setSelected((s) => s ?? c.today);
      const p = await call<Pulse>("desktop_pulse");
      setPulse(p);
      void refreshProposals();
    } catch (e) {
      setFatal((e as Error).message);
    }
  }, [refreshProposals]);
  useEffect(() => void boot(), [boot]);

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

  // 脉冲轮询：几个整数，变了才拉数据。窗口不可见时暂停，回到前台立即检查。
  const lastPulse = useRef<Pulse>();
  const lastPending = useRef(0);
  useEffect(() => {
    if (!context) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let ticks = 0;
    const tick = async () => {
      if (stopped) return;
      if (!document.hidden) {
        try {
          const p = await call<Pulse>("desktop_pulse");
          const prev = lastPulse.current;
          lastPulse.current = p;
          setPulse((old) => (old && JSON.stringify(old) === JSON.stringify(p) ? old : p));
          if (prev && p.change_sequence !== prev.change_sequence && inflight.current === 0) {
            // 外部修改：只保留当前范围的缓存，重新校对。
            const { from: f, to: t } = rangeRef.current;
            commit((all) => all.filter((i) => i.date >= f && i.date <= t));
            void refresh().then(prefetch);
          }
          if (prev && (p.proposal_rev !== prev.proposal_rev || p.pending_proposals !== prev.pending_proposals)) {
            void refreshProposals();
          }
          if (p.pending_proposals > lastPending.current && prev) {
            if (!document.hasFocus()) requestAttention();
          }
          lastPending.current = p.pending_proposals;
          if (++ticks % CONTEXT_EVERY === 0) void refreshContext();
        } catch {
          // 服务暂时不可用：保持现状，下一轮再试。
        }
      }
      timer = setTimeout(tick, PULSE_MS);
    };
    const onVisible = () => {
      if (!document.hidden) {
        clearTimeout(timer);
        void tick();
      }
    };
    document.addEventListener("visibilitychange", onVisible);
    window.addEventListener("focus", onVisible);
    timer = setTimeout(tick, PULSE_MS);
    return () => {
      stopped = true;
      clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisible);
      window.removeEventListener("focus", onVisible);
    };
  }, [context === undefined, commit, refresh, prefetch, refreshProposals, refreshContext]); // eslint-disable-line react-hooks/exhaustive-deps

  // ───────────── 导航 ─────────────
  const goDate = useCallback((date: string) => {
    const d = clampDate(date);
    setSelected(d);
    setMonth(monthKey(d));
  }, []);
  const goToday = useCallback(() => {
    if (ctxRef.current) goDate(ctxRef.current.today);
  }, [goDate]);
  const shiftMonth = useCallback(
    (delta: number) => {
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

  const decide = useCallback(
    async (proposalId: string, decision: "approve" | "reject") => {
      const r = await call<{ proposal: Proposal }>("desktop_decide_proposal", {
        proposal_id: proposalId,
        decision,
      });
      epoch.current++;
      await Promise.all([refreshProposals(), refresh()]);
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
    setSelected,
    goDate,
    goToday,
    shiftMonth,
    items,
    itemsByDate,
    loading,
    proposals,
    pending,
    pulse,
    refresh,
    refreshContext,
    refreshProposals,
    createItem,
    updateItem,
    setStatus,
    removeItem,
    restoreItem,
    decide,
  };
}
export type Calendar = ReturnType<typeof useCalendar>;
