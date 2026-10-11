import { useCallback, useEffect, useState, type ReactNode } from "react";
import {
  call,
  preview,
  type AppInfo,
  type ApprovalMode,
  type Diagnostics,
} from "../api";
import { Icon } from "../components/icons";
import { Segmented } from "../components/Segmented";
import { SoftwareUpdate } from "../components/SoftwareUpdate";
import type { Updater } from "../hooks/useUpdater";
import { useNotify } from "../hooks/useToasts";
import { relativeTime, type WeekStart } from "../lib/dates";
import type { Prefs, Theme } from "../lib/prefs";
import { copyText, formatBytes, formatDuration } from "../lib/util";

function Row({
  title,
  desc,
  children,
}: {
  title: string;
  desc?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="setting">
      <div className="setting-text">
        <strong>{title}</strong>
        {desc && <p className="muted">{desc}</p>}
      </div>
      <div className="setting-control">{children}</div>
    </div>
  );
}

export function SettingsView({
  updater,
  prefs,
  onPrefs,
  changeSequence,
}: {
  updater: Updater;
  prefs: Prefs;
  onPrefs: (patch: Partial<Prefs>) => void;
  changeSequence: number;
}) {
  const notify = useNotify();
  const [mode, setMode] = useState<ApprovalMode>();
  const [diag, setDiag] = useState<Diagnostics>();
  const [info, setInfo] = useState<AppInfo>();
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      const [s, d, i] = await Promise.all([
        call<{ approval_mode: ApprovalMode }>("desktop_get_settings"),
        call<Diagnostics>("desktop_diagnostics"),
        call<AppInfo>("desktop_app_info"),
      ]);
      setMode(s.approval_mode);
      setDiag(d);
      setInfo(i);
    } catch (e) {
      notify((e as Error).message, { tone: "error" });
    }
  }, [notify]);
  useEffect(() => void load(), [load, changeSequence]);

  const changeMode = async (next: ApprovalMode) => {
    const previous = mode;
    setMode(next);
    try {
      await call("desktop_set_settings", { approval_mode: next });
      notify(next === "all_writes" ? "现在助手的所有写入都需要你确认" : "现在只有删除需要你确认");
    } catch (e) {
      setMode(previous);
      notify((e as Error).message, { tone: "error" });
    }
  };
  const backup = async () => {
    setBusy(true);
    try {
      await call("desktop_backup");
      notify("已创建备份");
      await load();
    } catch (e) {
      notify((e as Error).message, { tone: "error" });
    } finally {
      setBusy(false);
    }
  };
  const reveal = (target: "data" | "logs" | "backups") =>
    call("desktop_reveal", { target }).catch((e: Error) => notify(e.message, { tone: "error" }));

  const mismatch = info && diag && !preview && info.app_version !== diag.service_version;

  return (
    <div className="page narrow">
      <header className="view-head">
        <div>
          <h1>设置</h1>
          <p className="muted">外观与周起始日只保存在这台电脑上；权限设置控制 MCP 客户端如何访问日历。</p>
        </div>
      </header>

      <SoftwareUpdate updater={updater} />

      <section className="card">
        <h2>外观</h2>
        <Row title="主题" desc="跟随系统会随 macOS 的浅色 / 深色模式自动切换。">
          <Segmented<Theme>
            label="主题"
            value={prefs.theme}
            onChange={(theme) => onPrefs({ theme })}
            options={[
              { value: "system", label: "跟随系统" },
              { value: "light", label: <><Icon name="sun" size={15} />浅色</> },
              { value: "dark", label: <><Icon name="moon" size={15} />深色</> },
            ]}
          />
        </Row>
        <Row title="每周从哪天开始">
          <Segmented<WeekStart>
            label="周起始日"
            value={prefs.weekStart}
            onChange={(weekStart) => onPrefs({ weekStart })}
            options={[
              { value: 1, label: "周一" },
              { value: 0, label: "周日" },
            ]}
          />
        </Row>
      </section>

      <section className="card">
        <h2>MCP 连接权限</h2>
        <Row
          title="需要你确认的操作"
          desc="删除事项永远需要确认；你也可以让助手的所有写入（新建、修改）都先经过你。"
        >
          {mode && (
            <Segmented<ApprovalMode>
              label="确认策略"
              value={mode}
              onChange={changeMode}
              options={[
                { value: "deletes", label: "仅删除" },
                { value: "all_writes", label: "所有写入" },
              ]}
            />
          )}
        </Row>
      </section>

      <section className="card">
        <h2>数据与备份</h2>
        {diag && (
          <dl className="stats">
            <div><dt>事项</dt><dd>{diag.items_active}</dd></div>
            <div><dt>已删除</dt><dd>{diag.items_deleted}</dd></div>
            <div><dt>修改记录</dt><dd>{diag.history_entries}</dd></div>
            <div><dt>数据库</dt><dd>{formatBytes(diag.database_bytes)}</dd></div>
          </dl>
        )}
        <Row
          title="备份"
          desc={
            diag?.backups.length
              ? `最近一次：${relativeTime(diag.backups[0].modified_at)}（${diag.backups[0].name}）。服务每天首次启动时会自动备份，保留最近几份。`
              : "还没有备份。服务每天首次启动时会自动备份。"
          }
        >
          <button className="btn" onClick={backup} disabled={busy}>
            <Icon name="download" size={16} />
            立即备份
          </button>
        </Row>
        <Row title="打开文件夹" desc="在访达中查看数据、日志和备份。">
          <div className="btn-row">
            <button className="btn" onClick={() => reveal("data")}>
              <Icon name="folder" size={16} />数据
            </button>
            <button className="btn" onClick={() => reveal("logs")}>
              <Icon name="folder" size={16} />日志
            </button>
            <button className="btn" onClick={() => reveal("backups")}>
              <Icon name="folder" size={16} />备份
            </button>
          </div>
        </Row>
        <p className="muted small">
          日志只记录操作类型与耗时，不会写入任何事项标题或备注。备份是完整的数据库副本，目前需要手动替换以恢复。
        </p>
      </section>

      <section className="card">
        <h2>关于</h2>
        {mismatch && (
          <p className="callout warn">
            <Icon name="alert" />
            <span>
              应用版本（{info!.app_version}）与后台服务（{diag!.service_version}）不一致，请完全退出并重新打开日格。
            </span>
          </p>
        )}
        <dl className="about">
          <dt>应用版本</dt><dd>{info?.app_version ?? "—"}</dd>
          <dt>后台服务</dt>
          <dd>
            {diag ? `${diag.service_version} · 已运行 ${formatDuration(diag.uptime_seconds)}` : "—"}
          </dd>
          <dt>数据结构</dt><dd>{diag ? `v${diag.schema_version}` : "—"}</dd>
          <dt>日历时区</dt><dd>{diag?.timezone ?? "—"}</dd>
          <dt>系统</dt><dd>{info ? `${info.platform} / ${info.arch}` : "—"}</dd>
          <dt>数据目录</dt><dd className="mono">{diag?.data_dir ?? "—"}</dd>
          <dt>MCP 程序</dt>
          <dd className="mono">
            {info?.mcp_path ?? "—"}
            {info && (
              <button
                className="btn ghost icon-btn"
                aria-label="复制 MCP 程序路径"
                onClick={async () => notify((await copyText(info.mcp_path)) ? "已复制" : "复制失败")}
              >
                <Icon name="copy" size={15} />
              </button>
            )}
          </dd>
        </dl>
        <h3 className="shortcuts-title">快捷键</h3>
        <ul className="shortcuts">
          <li><kbd>⌘</kbd><kbd>K</kbd> 搜索与命令</li>
          <li><kbd>← → ↑ ↓</kbd> 移动选中日期</li>
          <li><kbd>[</kbd> <kbd>]</kbd> 上 / 下个月</li>
          <li><kbd>T</kbd> 回到今天</li>
          <li><kbd>N</kbd> 添加事项</li>
          <li><kbd>⌘</kbd><kbd>1</kbd>–<kbd>4</kbd> 切换页面</li>
          <li><kbd>⌘</kbd><kbd>↵</kbd> 保存对话框</li>
        </ul>
      </section>
    </div>
  );
}
