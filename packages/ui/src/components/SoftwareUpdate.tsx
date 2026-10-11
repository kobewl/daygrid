import { preview } from "../api";
import type { Updater } from "../hooks/useUpdater";
import { useNotify } from "../hooks/useToasts";
import { openReleasePage } from "../lib/updater";
import { formatBytes } from "../lib/util";
import { Icon } from "./icons";

export function SoftwareUpdate({ updater }: { updater: Updater }) {
  const { state, check, install, restart } = updater;
  const notify = useNotify();
  const progress = state.kind === "installing" ? state.progress : undefined;
  const percent =
    progress?.phase === "downloading" && progress.total
      ? Math.min(100, Math.floor((progress.downloaded / progress.total) * 100))
      : null;
  const description =
    state.kind === "idle"
      ? "手动检查新版本，安装完成后自动重启。"
      : state.kind === "checking"
        ? "正在检查新版本…"
        : state.kind === "latest"
          ? "已是最新版本。"
          : state.kind === "available"
            ? `发现新版本 ${state.info.version} · 当前 ${state.info.currentVersion}`
            : state.kind === "installing"
              ? progress?.phase === "installing"
                ? "正在校验并安装，完成后自动重启…"
                : `正在下载更新${percent === null ? "…" : ` · ${percent}%`}`
              : state.kind === "installed"
                ? preview
                  ? "演示完成，未下载或安装任何更新。"
                  : "更新已安装，请重新启动日格。"
                : state.message;
  const open = () =>
    void openReleasePage().catch((e) => notify(String(e), { tone: "error" }));

  return (
    <section className="card software-update" aria-label="软件更新">
      <div className="update-heading">
        <div className="update-symbol">
          <Icon name="download" size={21} />
        </div>
        <div className="grow">
          <h2>软件更新</h2>
          <p
            className="muted small"
            role={state.kind === "error" ? "alert" : "status"}
          >
            {description}
          </p>
        </div>
        {state.kind === "available" ? (
          <button className="btn primary" onClick={() => void install()}>
            立即更新
          </button>
        ) : state.kind === "installed" && !preview ? (
          <button className="btn primary" onClick={() => void restart()}>
            立即重启
          </button>
        ) : (
          <button
            className="btn"
            onClick={() => void check()}
            disabled={state.kind === "checking" || state.kind === "installing"}
          >
            {state.kind === "checking"
              ? "检查中…"
              : state.kind === "installing"
                ? "更新中…"
                : "检查更新"}
          </button>
        )}
      </div>
      {progress && (
        <div>
          <div
            className={`update-progress${percent === null ? " indeterminate" : ""}`}
            role="progressbar"
            aria-label="更新进度"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={percent ?? undefined}
          >
            <span
              style={percent === null ? undefined : { width: `${percent}%` }}
            />
          </div>
          {progress.phase === "downloading" && progress.downloaded > 0 && (
            <p className="muted small update-bytes">
              已下载 {formatBytes(progress.downloaded)}
              {progress.total ? ` / ${formatBytes(progress.total)}` : ""}
            </p>
          )}
        </div>
      )}
      {state.kind === "available" && (
        <>
          <p className="muted small">
            更新前会自动备份日历数据。事项、备注和 MCP 连接会保留。
          </p>
          {state.info.notes && (
            <details className="update-notes">
              <summary>查看更新内容</summary>
              <pre>{state.info.notes}</pre>
            </details>
          )}
        </>
      )}
      {(state.kind === "available" || state.kind === "error") && (
        <button className="link update-release" onClick={open}>
          {state.kind === "error" ? "前往发布页手动下载" : "查看发布页"}
        </button>
      )}
      {preview && (
        <p className="muted small">
          浏览器仅演示更新流程，不会安装更新或重启应用。
        </p>
      )}
    </section>
  );
}
