import { useCallback } from "react";
import type { Item, Visibility } from "../api";
import type { Draft } from "../components/ItemDialog";
import { dayTitle } from "../lib/dates";
import { automaticColor } from "../lib/itemColor";
import { joinReminder } from "../lib/reminder";
import { rememberVisibility } from "../lib/visibility";
import type { Calendar } from "./useCalendar";
import { useNotify } from "./useToasts";

/** 面向用户的操作：在数据层之上加上提示与「撤销」，界面各处共用。 */
export function useActions(cal: Calendar) {
  const notify = useNotify();
  const fail = useCallback(
    (e: unknown) => notify((e as Error).message, { tone: "error" }),
    [notify],
  );

  const toggle = useCallback(
    (item: Item) => {
      cal
        .setStatus(item.id, item.status === "done" ? "planned" : "done")
        .catch(fail);
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
              run: () =>
                cal.restoreItem(deleted.id, deleted.revision).catch(fail),
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
            action: {
              label: "撤销",
              run: () => cal.updateItem(id, { date: previous }).catch(fail),
            },
          }),
        )
        .catch(fail);
    },
    [cal.items, cal.updateItem, notify, fail], // eslint-disable-line react-hooks/exhaustive-deps
  );

  const quickAdd = useCallback(
    async (date: string, title: string, visibility?: Visibility) => {
      try {
        await cal.createItem({
          date,
          title,
          notes: "",
          color: automaticColor(title),
          ...(visibility ? { visibility } : {}),
        });
        return true;
      } catch (e) {
        fail(e);
        return false;
      }
    },
    [cal.createItem, fail], // eslint-disable-line react-hooks/exhaustive-deps
  );

  /** 对话框保存：失败时抛出，让对话框处理版本冲突并保留草稿。 */
  const save = useCallback(
    async (draft: Draft) => {
      // 对话框已拦下不完整的提醒，这里只会是本地串或 null。
      const remindAt = joinReminder(draft.reminder) ?? null;
      // 只读成员编辑共享事项：请求里只带 remind_at（服务端只对这种修改放行）。
      if (draft.id && draft.reminderOnly) {
        if (!draft.remindTouched) return;
        await cal.updateItem(draft.id, { remind_at: remindAt }, { revision: draft.revision });
        notify("已保存");
        return;
      }
      const fields = {
        date: draft.date,
        title: draft.title,
        notes: draft.notes,
        color: draft.id ? draft.color : automaticColor(draft.title),
      };
      if (draft.visibility) rememberVisibility(draft.visibility);
      if (draft.id) {
        // 没动过提醒就不传：只改日期时由服务端按同样的天数平移。
        await cal.updateItem(
          draft.id,
          { ...fields, ...(draft.remindTouched ? { remind_at: remindAt } : {}) },
          { revision: draft.revision },
        );
        // 可见范围是单独的写入口（它还要通知同步引擎撤回 / 上传）；改了才调用。
        if (draft.visibility && draft.visibility !== draft.originalVisibility)
          await cal.setVisibility(draft.id, draft.visibility);
      } else {
        await cal.createItem({
          ...fields,
          ...(draft.visibility ? { visibility: draft.visibility } : {}),
          ...(remindAt ? { remind_at: remindAt } : {}),
        });
      }
      cal.goDate(draft.date);
      notify(
        draft.visibility === "private" && draft.originalVisibility === "shared"
          ? "已保存，并从团队日历撤回（同事将不再看到它）"
          : "已保存",
      );
    },
    [cal.updateItem, cal.createItem, cal.setVisibility, cal.goDate, notify], // eslint-disable-line react-hooks/exhaustive-deps
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
