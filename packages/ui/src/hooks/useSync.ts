import { useCallback, useEffect, useRef, useState } from "react";
import { call, type SyncConflict, type SyncStatus } from "../api";
import { useNotify } from "./useToasts";

/**
 * 协同状态。数据都在本机服务里：这里只读 `desktop_sync_status`（几个整数 + 在线成员），
 * 由脉冲里的 `sync_sequence`（状态或成员变化）和 `change_sequence`（本地待同步数量变化）驱动刷新。
 * 连接中（异步建立）时再加一个短轮询，直到有结果。
 */
export function useSync({
  syncSequence,
  changeSequence,
  onOpenConflicts,
}: {
  syncSequence: number | undefined;
  changeSequence: number | undefined;
  onOpenConflicts: () => void;
}) {
  const notify = useNotify();
  const [status, setStatus] = useState<SyncStatus>();
  const [conflicts, setConflicts] = useState<SyncConflict[]>([]);
  const knownOpen = useRef<number>();
  const seen = useRef<Set<number>>();
  const openRef = useRef(onOpenConflicts);
  openRef.current = onOpenConflicts;

  const loadConflicts = useCallback(async () => {
    try {
      const list = (await call<{ conflicts: SyncConflict[] }>("desktop_sync_conflicts")).conflicts;
      setConflicts(list);
      return list;
    } catch {
      // 下一次状态变化会重试。
      return undefined;
    }
  }, []);

  const refresh = useCallback(async () => {
    try {
      const s = await call<SyncStatus>("desktop_sync_status");
      setStatus((old) => (old && JSON.stringify(old) === JSON.stringify(s) ? old : s));
      const before = knownOpen.current;
      knownOpen.current = s.open_conflicts;
      if (before === undefined || s.open_conflicts !== before) {
        const list = await loadConflicts();
        // 按原因分别提示：不同原因对用户意味着完全不同的事，不能一句话糊弄。
        const known = seen.current;
        seen.current = new Set((list ?? []).map((c) => c.id));
        if (known && list) {
          const fresh = list.filter((c) => c.state === "open" && !known.has(c.id));
          const count = (reason: string) => new Set(fresh.filter((c) => c.reason === reason).map((c) => c.item_id + c.field)).size;
          const messages = [
            [count("concurrent_edit"), (n: number) => `有 ${n} 处修改和同事同时改了同一处，已保留较新的一份，你的版本可以找回。`],
            [count("item_deleted"), (n: number) => `你修改的 ${n} 处内容所在的事项已被同事删除，你的修改已保存到冲突列表，可以找回。`],
            [count("invalid"), (n: number) => `有 ${n} 处修改服务器没有接受，已恢复成服务器上的内容，你写的内容保存在冲突列表里。`],
            [count("read_only"), (n: number) => `你在团队里变成了只读成员，${n} 处没来得及上传的修改已被放弃（可在冲突列表里查看）。`],
          ] as const;
          for (const [n, text] of messages)
            if (n > 0)
              notify(text(n), { action: { label: "查看", run: () => openRef.current() }, ms: 10_000 });
        }
      }
      return s;
    } catch {
      return undefined;
    }
  }, [loadConflicts, notify]);

  useEffect(() => void refresh(), [refresh, syncSequence, changeSequence]);

  const transient = status?.state === "connecting";
  useEffect(() => {
    if (!transient) return;
    const timer = setInterval(() => void refresh(), 1000);
    return () => clearInterval(timer);
  }, [transient, refresh]);

  return { status, conflicts, refresh };
}
export type Sync = ReturnType<typeof useSync>;
