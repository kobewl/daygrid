import { useCallback, useEffect, useMemo, useState } from "react";
import { preview, type Item } from "./api";
import { ApprovalBar } from "./components/ApprovalBar";
import { CommandPalette, type Command } from "./components/CommandPalette";
import { LogoMark } from "./components/icons";
import { ItemDialog, type DialogTarget } from "./components/ItemDialog";
import { Sidebar, type ViewId } from "./components/Sidebar";
import { useActions } from "./hooks/useActions";
import { useCalendar } from "./hooks/useCalendar";
import { ToastProvider } from "./hooks/useToasts";
import { usePrefs } from "./lib/prefs";
import { AssistantsView } from "./views/AssistantsView";
import { CalendarView } from "./views/CalendarView";
import { HistoryView } from "./views/HistoryView";
import { SettingsView } from "./views/SettingsView";

const VIEW_KEYS: Record<string, ViewId> = {
  "1": "calendar",
  "2": "history",
  "3": "assistants",
  "4": "settings",
  ",": "settings",
};

function Shell() {
  const [prefs, updatePrefs] = usePrefs();
  const cal = useCalendar(prefs.weekStart);
  const actions = useActions(cal);
  const [view, setView] = useState<ViewId>("calendar");
  const [dialog, setDialog] = useState<DialogTarget>();
  const [palette, setPalette] = useState(false);

  const today = cal.context?.today;
  const createAt = useCallback(
    (date: string) => setDialog({ mode: "create", date }),
    [],
  );
  const edit = useCallback(
    (item: Item) => setDialog({ mode: "edit", item }),
    [],
  );
  const openView = useCallback((v: ViewId) => setView(v), []);

  // 全局快捷键：⌘K 面板，⌘1–4 / ⌘, 切页。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || e.altKey) return;
      if (e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPalette((v) => !v);
      } else if (VIEW_KEYS[e.key]) {
        e.preventDefault();
        setView(VIEW_KEYS[e.key]);
        setDialog(undefined);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const commands = useMemo<Command[]>(() => {
    const nav = (
      id: ViewId,
      label: string,
      icon: Command["icon"],
      hint: string,
    ): Command => ({
      id: "view-" + id,
      label: `打开${label}`,
      hint,
      icon,
      keywords: id,
      run: () => setView(id),
    });
    const selected = cal.selected ?? today ?? "";
    return [
      {
        id: "new",
        label: "添加事项",
        hint: "N",
        icon: "plus",
        keywords: "新建 创建 add",
        run: () => {
          setView("calendar");
          createAt(selected);
        },
      },
      {
        id: "today",
        label: "回到今天",
        hint: "T",
        icon: "calendar",
        keywords: "today",
        run: () => {
          setView("calendar");
          cal.goToday();
        },
      },
      {
        id: "next",
        label: "下个月",
        hint: "]",
        icon: "right",
        run: () => {
          setView("calendar");
          cal.shiftMonth(1);
        },
      },
      {
        id: "prev",
        label: "上个月",
        hint: "[",
        icon: "left",
        run: () => {
          setView("calendar");
          cal.shiftMonth(-1);
        },
      },
      nav("calendar", "日历", "calendar", "⌘1"),
      nav("history", "修改记录", "history", "⌘2"),
      nav("assistants", "MCP 连接", "plug", "⌘3"),
      nav("settings", "设置", "sliders", "⌘4"),
      {
        id: "theme-light",
        label: "切换到浅色主题",
        icon: "sun",
        keywords: "theme light 外观",
        run: () => updatePrefs({ theme: "light" }),
      },
      {
        id: "theme-dark",
        label: "切换到深色主题",
        icon: "moon",
        keywords: "theme dark 外观",
        run: () => updatePrefs({ theme: "dark" }),
      },
      {
        id: "theme-system",
        label: "主题跟随系统",
        icon: "sliders",
        keywords: "theme system 外观",
        run: () => updatePrefs({ theme: "system" }),
      },
      {
        id: "week",
        label:
          prefs.weekStart === 1 ? "每周改为从周日开始" : "每周改为从周一开始",
        icon: "calendar",
        keywords: "week 周起始",
        run: () => updatePrefs({ weekStart: prefs.weekStart === 1 ? 0 : 1 }),
      },
    ];
  }, [
    cal.selected,
    cal.goToday,
    cal.shiftMonth,
    createAt,
    today,
    prefs.weekStart,
    updatePrefs,
  ]);

  if (cal.fatal)
    return (
      <div className="splash">
        <LogoMark size={52} />
        <h1>无法连接日历服务</h1>
        <p className="muted">{cal.fatal}</p>
        <button className="btn primary" onClick={() => void cal.retry()}>
          重试
        </button>
        <small className="muted">
          如果反复失败，请完全退出并重新打开日格；日志在「设置 → 打开文件夹 →
          日志」。
        </small>
      </div>
    );
  if (!cal.context || !today)
    return (
      <div className="splash" aria-busy="true">
        <LogoMark size={52} />
        <p className="muted">正在打开日历…</p>
      </div>
    );

  const pulseKey = cal.pulse
    ? `${cal.pulse.activity_sequence}-${cal.pulse.proposal_rev}-${cal.pulse.change_sequence}`
    : "";
  const overlay = !!dialog || palette;

  return (
    <div className="app">
      {preview && (
        <div className="preview-banner" role="note">
          浏览器预览 · 示例数据，刷新页面即重置；真实数据请使用桌面应用
        </div>
      )}
      <div className="layout">
        <Sidebar
          view={view}
          onView={openView}
          pending={cal.pending.length}
          onSearch={() => setPalette(true)}
        />
        <main className="content">
          <ApprovalBar
            pending={view === "assistants" ? [] : cal.pending}
            today={today}
            onDecide={actions.decide}
            onSeeAll={() => setView("assistants")}
          />
          {view === "calendar" && (
            <CalendarView
              cal={cal}
              weekStart={prefs.weekStart}
              keysEnabled={!overlay}
              onCreate={createAt}
              onEdit={edit}
              onToggle={actions.toggle}
              onRemove={actions.remove}
              onMove={actions.move}
              onQuickAdd={actions.quickAdd}
            />
          )}
          {view === "history" && (
            <HistoryView
              changeSequence={cal.pulse?.change_sequence ?? 0}
              today={today}
              onRestore={cal.restoreItem}
              onJump={(date) => {
                cal.goDate(date);
                setView("calendar");
              }}
            />
          )}
          {view === "assistants" && (
            <AssistantsView
              pending={cal.pending}
              proposals={cal.proposals}
              today={today}
              pulseKey={pulseKey}
              onDecide={actions.decide}
            />
          )}
          {view === "settings" && (
            <SettingsView
              prefs={prefs}
              onPrefs={updatePrefs}
              changeSequence={cal.pulse?.change_sequence ?? 0}
            />
          )}
        </main>
      </div>

      {dialog && (
        <ItemDialog
          key={dialog.mode === "edit" ? dialog.item.id : "new-" + dialog.date}
          target={dialog}
          onSave={actions.save}
          onToggle={actions.toggle}
          onDelete={actions.remove}
          onClose={() => setDialog(undefined)}
        />
      )}
      {palette && (
        <CommandPalette
          today={today}
          commands={commands}
          onGoDate={(date) => {
            setView("calendar");
            cal.goDate(date);
          }}
          onOpenItem={(item) => {
            setView("calendar");
            edit(item);
          }}
          onClose={() => setPalette(false)}
        />
      )}
    </div>
  );
}

export default function App() {
  return (
    <ToastProvider>
      <Shell />
    </ToastProvider>
  );
}
