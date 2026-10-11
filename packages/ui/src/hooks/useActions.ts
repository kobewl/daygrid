import { useCallback } from "react";
import type { Item } from "../api";
import type { Draft } from "../components/ItemDialog";
import { dayTitle } from "../lib/dates";
import type { Prefs } from "../lib/prefs";
import type { Calendar } from "./useCalendar";
import { useNotify } from "./useToasts";

/** 面向用户的操作：在数据层之上加上提示与「撤销」，界面各处共用。 */
export function useActions(
  cal: Calendar,
  prefs: Prefs,
  updatePrefs: (patch: Partial<Prefs>) => void,
) {
  const notify = useNotify();
  const fail = useCallback(
    (e: unknown) => notify((e as Error).message, { tone: "error" }),
    [notify],
  );

  const toggle = useCallback(
    (item: Item) => {
      cal.setStatus(item.id, item.status === "done" ? "planned" : "done").catch(fail);
    },
    [cal.setStatus, fail], // eslint-disable-line react-hooks/exhaustive-deps
  );

  const remove = useCallback(
    (item: Item) => {
      cal
        .removeItem(item.id)
        .then((deleted) =>
          notify(`已删除「${item.title}」`, {
            action: {
              label: "撤销",
              run: () => cal.restoreItem(deleted.id, deleted.revision).catch(fail),
            },
          }),
        )
        .catch(fail);
    },
    [cal.removeItem, cal.restoreItem, notify, fail], // eslint-disable-line react-hooks/exhaustive-deps
  );

  const move = useCallback(
    (id: string, date: string) => {
      const item = cal.items.find((i) => i.id === id);
      if (!item || item.date === date) return;
      const previous = item.date;
      cal
        .updateItem(id, { date })
        .then(() =>
          notify(`「${item.title}」已移到 ${dayTitle(date)}`, {
            action: { label: "撤销", run: () => cal.updateItem(id, { date: previous }).catch(fail) },
          }),
        )
        .catch(fail);
    },
    [cal.items, cal.updateItem, notify, fail], // eslint-disable-line react-hooks/exhaustive-deps
  );

  const quickAdd = useCallback(
    async (date: string, title: string) => {
      try {
        await cal.createItem({ date, title, notes: "", color: prefs.lastColor });
        return true;
      } catch (e) {
        fail(e);
        return false;
      }
    },
    [cal.createItem, prefs.lastColor, fail], // eslint-disable-line react-hooks/exhaustive-deps
  );

  /** 对话框保存：失败时抛出，让对话框处理版本冲突并保留草稿。 */
  const save = useCallback(
    async (draft: Draft) => {
      const fields = {
        date: draft.date,
        title: draft.title,
        notes: draft.notes,
        color: draft.color,
      };
      if (draft.id) await cal.updateItem(draft.id, fields, { revision: draft.revision });
      else await cal.createItem(fields);
      updatePrefs({ lastColor: draft.color });
      cal.goDate(draft.date);
      notify("已保存");
    },
    [cal.updateItem, cal.createItem, cal.goDate, updatePrefs, notify], // eslint-disable-line react-hooks/exhaustive-deps
  );

  const decide = useCallback(
    async (id: string, decision: "approve" | "reject") => {
      try {
        const p = await cal.decide(id, decision);
        if (decision === "reject") notify("已拒绝该请求");
        else if (p.state === "failed")
          notify(p.error?.message ?? "请求未能执行", { tone: "error" });
        else notify("已批准并执行");
      } catch (e) {
        fail(e);
        void cal.refreshProposals();
      }
    },
    [cal.decide, cal.refreshProposals, notify, fail], // eslint-disable-line react-hooks/exhaustive-deps
  );

  return { toggle, remove, move, quickAdd, save, decide, fail };
}
