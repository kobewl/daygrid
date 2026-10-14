import type { SyncStatus, Visibility } from "../api";

const KEY = "daygrid.newItemVisibility.v1";

/** 新事项默认的可见范围：记住用户上一次的选择；从没选过就是更保守的「仅自己可见」。 */
export function rememberedVisibility(): Visibility {
  try {
    return localStorage.getItem(KEY) === "shared" ? "shared" : "private";
  } catch {
    return "private";
  }
}
export function rememberVisibility(v: Visibility) {
  try {
    localStorage.setItem(KEY, v);
  } catch {
    // 存储不可用时只在这一次有效。
  }
}

/** 这个团队状态下，用户能不能把事项共享出去（连接了团队，且不是只读成员）。 */
export const canShare = (status: SyncStatus | undefined) =>
  !!status?.linked && status.config?.role !== "viewer";

/** 新建事项（含快速添加）时要传给后端的可见范围；没连接团队时不传（后端一律私有）。 */
export const newItemVisibility = (status: SyncStatus | undefined): Visibility | undefined =>
  canShare(status) ? rememberedVisibility() : undefined;
