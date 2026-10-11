import { cx } from "../lib/util";
import { Icon, LogoMark, type IconName } from "./icons";

export type ViewId = "calendar" | "history" | "assistants" | "settings";

const NAV: { id: ViewId; label: string; icon: IconName; key: string }[] = [
  { id: "calendar", label: "日历", icon: "calendar", key: "1" },
  { id: "history", label: "修改记录", icon: "history", key: "2" },
  { id: "assistants", label: "AI 助手", icon: "plug", key: "3" },
  { id: "settings", label: "设置", icon: "sliders", key: "4" },
];

export function Sidebar({
  view,
  onView,
  pending,
  onSearch,
}: {
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

      <button className="searchbox" onClick={onSearch} aria-label="搜索与命令（⌘K）">
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
                <span className="badge-count" aria-label={`${pending} 个待确认`}>
                  {pending}
                </span>
              )}
            </button>
          </li>
        ))}
      </ul>
    </nav>
  );
}
