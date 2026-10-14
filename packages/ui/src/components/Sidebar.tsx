import type { SyncStatus } from "../api";
import { cx } from "../lib/util";
import { Avatars } from "./Avatars";
import { Icon, LogoMark, type IconName } from "./icons";

export type ViewId = "calendar" | "history" | "assistants" | "settings";

const NAV: { id: ViewId; label: string; icon: IconName; key: string }[] = [
  { id: "calendar", label: "日历", icon: "calendar", key: "1" },
  { id: "history", label: "修改记录", icon: "history", key: "2" },
  { id: "assistants", label: "MCP 连接", icon: "plug", key: "3" },
  { id: "settings", label: "设置", icon: "sliders", key: "4" },
];

export function Sidebar({
  view,
  onView,
  pending,
  onSearch,
  sync,
  onSync,
}: {
  sync?: SyncStatus;
  onSync: () => void;
  view: ViewId;
  onView: (v: ViewId) => void;
  pending: number;
  onSearch: () => void;
}) {
  return (
    <nav className="sidebar" aria-label="主导航">
      <div className="brand">
        <LogoMark />
        <div>
          <strong>日格</strong>
          <small>Daygrid</small>
        </div>
      </div>

      <button
        className="searchbox"
        onClick={onSearch}
        aria-label="搜索与命令（⌘K）"
      >
        <Icon name="search" size={16} />
        <span>搜索或跳转…</span>
        <kbd>⌘K</kbd>
      </button>

      <ul>
        {NAV.map((n) => (
          <li key={n.id}>
            <button
              className={cx("nav", view === n.id && "on")}
              aria-current={view === n.id ? "page" : undefined}
              onClick={() => onView(n.id)}
              title={`${n.label}（⌘${n.key}）`}
            >
              <Icon name={n.icon} />
              <span>{n.label}</span>
              {n.id === "assistants" && pending > 0 && (
                <span
                  className="badge-count"
                  aria-label={`${pending} 个待确认`}
                >
                  {pending}
                </span>
              )}
            </button>
          </li>
        ))}
      </ul>
      {sync?.linked && sync.config && (
        <button className="sync-chip" onClick={onSync} title="多人协作设置">
          <span className={cx("dot", sync.state)} aria-hidden="true" />
          <span className="sync-chip-text">
            <strong>{sync.config.workspace_name}</strong>
            <small>
              {sync.state === "online" || sync.state === "syncing"
                ? sync.pending > 0
                  ? `${sync.pending} 项待上传`
                  : "已同步"
                : sync.state === "offline"
                  ? sync.pending > 0
                    ? `离线 · ${sync.pending} 项待上传`
                    : "离线"
                  : sync.state === "error"
                    ? "需要处理"
                    : "连接中…"}
              {sync.config.role === "viewer" && " · 只读"}
            </small>
          </span>
          <Avatars peers={sync.peers} max={3} />
          {sync.open_conflicts > 0 && (
            <span className="badge-count" aria-label={`${sync.open_conflicts} 处冲突`}>
              {sync.open_conflicts}
            </span>
          )}
        </button>
      )}
      <div className="sidebar-foot">
        <span className="sidebar-foot-mark">
          <Icon name="calendar" size={15} /> 把每一天，安排清楚。
        </span>
        <small>
          <kbd>N</kbd> 添加事项 <span>·</span> <kbd>T</kbd> 回到今天
        </small>
      </div>
    </nav>
  );
}
