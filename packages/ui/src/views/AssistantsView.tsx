import { useCallback, useEffect, useState } from "react";
import {
  call,
  preview,
  type Activity,
  type ClientStatus,
  type Connection,
  type Proposal,
} from "../api";
import { Icon } from "../components/icons";
import { ProposalCard, useNow } from "../components/ProposalCard";
import { Segmented } from "../components/Segmented";
import { useNotify } from "../hooks/useToasts";
import { relativeTime } from "../lib/dates";
import { TOOL_LABEL } from "../lib/labels";
import { copyText, cx } from "../lib/util";

const STATE: Record<Connection["state"], string> = {
  active: "已启用",
  paused: "已暂停",
  revoked: "已撤销",
};

function ClientSetup({ connection }: { connection: Connection }) {
  const notify = useNotify();
  const [clients, setClients] = useState<ClientStatus[]>();
  const [busy, setBusy] = useState("");

  const load = useCallback(async () => {
    try {
      setClients(
        (await call<{ clients: ClientStatus[] }>("desktop_client_status", { connection_id: connection.id }))
          .clients,
      );
    } catch (e) {
      notify((e as Error).message, { tone: "error" });
    }
  }, [connection.id, notify]);
  useEffect(() => void load(), [load]);

  const act = async (client: ClientStatus, kind: "install" | "uninstall") => {
    setBusy(client.id + kind);
    try {
      const r = await call<{ restart_hint?: string }>(`desktop_client_${kind}`, {
        client: client.id,
        connection_id: connection.id,
      });
      notify(kind === "install" ? (r.restart_hint ?? "已写入") : `已从 ${client.name} 移除`, { ms: 7000 });
      await load();
    } catch (e) {
      notify((e as Error).message, { tone: "error" });
    } finally {
      setBusy("");
    }
  };
  const copy = async (text: string, what: string) =>
    notify((await copyText(text)) ? `已复制${what}` : "复制失败，请手动选择文本", {});

  return (
    <div className="setup">
      <h4>接入你的 AI 客户端</h4>
      <ul className="clients">
        {(clients ?? []).map((c) => (
          <li key={c.id}>
            <div className="client-main">
              <strong>{c.name}</strong>
              <small className="muted mono">{c.config_path}</small>
            </div>
            <div className="client-state">
              {c.installed ? (
                <span className="badge ok">已接入</span>
              ) : c.configured_for_other_connection ? (
                <span className="badge warn" title="该客户端里已有一个 daygrid，指向另一个连接">
                  已配置其他连接
                </span>
              ) : !c.detected ? (
                <span className="badge muted">未检测到</span>
              ) : null}
            </div>
            <div className="client-actions">
              {c.kind === "file" ? (
                <>
                  <button
                    className="btn"
                    disabled={busy !== "" || connection.state === "revoked"}
                    onClick={() => act(c, "install")}
                  >
                    {busy === c.id + "install" ? "写入中…" : c.installed ? "重新写入" : "一键写入"}
                  </button>
                  {c.installed && (
                    <button className="btn ghost" disabled={busy !== ""} onClick={() => act(c, "uninstall")}>
                      移除
                    </button>
                  )}
                </>
              ) : (
                c.manual_command && (
                  <button className="btn" onClick={() => copy(c.manual_command!, `${c.name} 命令`)}>
                    <Icon name="terminal" size={16} />
                    复制命令
                  </button>
                )
              )}
            </div>
            {c.kind === "command" && c.manual_command && <code className="cmd">{c.manual_command}</code>}
          </li>
        ))}
      </ul>
      <p className="muted small">
        「一键写入」只会在客户端的 <span className="mono">mcpServers</span>{" "}
        里增加或更新 <span className="mono">daygrid</span>，其余配置保持原样；首次修改前会先备份原文件。
      </p>
      <details>
        <summary>其他客户端：复制 JSON 配置</summary>
        <pre className="config">{JSON.stringify(connection.config, null, 2)}</pre>
        <button className="btn" onClick={() => copy(JSON.stringify(connection.config, null, 2), "配置")}>
          <Icon name="copy" size={16} />
          复制配置
        </button>
      </details>
    </div>
  );
}

function ConnectionCard({
  c,
  open,
  onToggle,
  onState,
}: {
  c: Connection;
  open: boolean;
  onToggle: () => void;
  onState: (state: Connection["state"]) => void;
}) {
  const now = useNow(30000);
  const [confirmRevoke, setConfirmRevoke] = useState(false);
  useEffect(() => {
    if (!confirmRevoke) return;
    const t = setTimeout(() => setConfirmRevoke(false), 4000);
    return () => clearTimeout(t);
  }, [confirmRevoke]);
  const revoked = c.state === "revoked";
  return (
    <article className={cx("conn", c.state)}>
      <header>
        <span className={cx("status-dot", c.state)} aria-hidden="true" />
        <div className="conn-main">
          <h3>{c.label}</h3>
          <p className="muted">
            {c.permission === "write" ? "可读写" : "只读"} · {STATE[c.state]} · 最近活动{" "}
            {relativeTime(c.last_seen_at, now)} · 调用 {c.calls} 次
          </p>
        </div>
        <div className="actions">
          {!revoked && (
            <button className="btn" onClick={onToggle} aria-expanded={open}>
              {open ? "收起" : "接入 / 配置"}
            </button>
          )}
          {c.state === "active" && (
            <button className="btn ghost" onClick={() => onState("paused")}>
              暂停
            </button>
          )}
          {c.state === "paused" && (
            <button className="btn" onClick={() => onState("active")}>
              恢复
            </button>
          )}
          {!revoked &&
            (confirmRevoke ? (
              <button className="btn danger-solid" onClick={() => onState("revoked")}>
                确认撤销（不可恢复）
              </button>
            ) : (
              <button className="btn ghost danger-text" onClick={() => setConfirmRevoke(true)}>
                撤销
              </button>
            ))}
        </div>
      </header>
      {open && !revoked && <ClientSetup connection={c} />}
    </article>
  );
}

export function AssistantsView({
  pending,
  proposals,
  today,
  pulseKey,
  onDecide,
}: {
  pending: Proposal[];
  proposals: Proposal[];
  today: string;
  pulseKey: string;
  onDecide: (id: string, decision: "approve" | "reject") => Promise<void>;
}) {
  const notify = useNotify();
  const [connections, setConnections] = useState<Connection[]>();
  const [activity, setActivity] = useState<Activity[]>([]);
  const [openId, setOpenId] = useState<string>();
  const [creating, setCreating] = useState(false);
  const [label, setLabel] = useState("");
  const [permission, setPermission] = useState<"write" | "read">("write");
  const [filter, setFilter] = useState("all");
  const [busy, setBusy] = useState(false);
  const now = useNow(30000);

  const load = useCallback(async () => {
    try {
      const [c, a] = await Promise.all([
        call<{ connections: Connection[] }>("desktop_connections"),
        call<{ activity: Activity[] }>("desktop_activity", { limit: 100 }),
      ]);
      setConnections(c.connections);
      setActivity(a.activity);
    } catch (e) {
      notify((e as Error).message, { tone: "error" });
    }
  }, [notify]);
  // pulseKey 变化（有新活动 / 新请求 / 日历变化）时静默刷新。
  useEffect(() => void load(), [load, pulseKey]);

  const create = async () => {
    const name = label.trim();
    if (!name || busy) return;
    setBusy(true);
    try {
      const r = await call<{ id: string }>("desktop_create_connection", { label: name, permission });
      setLabel("");
      setCreating(false);
      await load();
      setOpenId(r.id);
      notify("连接已建立，现在把它接入你的 AI 客户端");
    } catch (e) {
      notify((e as Error).message, { tone: "error" });
    } finally {
      setBusy(false);
    }
  };
  const setState = async (c: Connection, state: Connection["state"]) => {
    try {
      await call("desktop_set_connection", { id: c.id, state });
      notify(state === "revoked" ? `已撤销「${c.label}」` : state === "paused" ? `已暂停「${c.label}」` : `已恢复「${c.label}」`);
      await load();
    } catch (e) {
      notify((e as Error).message, { tone: "error" });
    }
  };

  const recent = proposals.filter((p) => p.state !== "pending").slice(0, 5);
  const shown = connections?.filter((c) => c.state !== "revoked") ?? [];
  const revoked = connections?.filter((c) => c.state === "revoked") ?? [];
  const feed = activity.filter((a) => filter === "all" || a.connection_id === filter);

  return (
    <div className="page">
      <header className="view-head">
        <div>
          <h1>MCP 连接</h1>
          <p className="muted">
            让 Cursor、Claude 等客户端通过 MCP 访问你的日历。每个客户端使用独立连接，可以随时暂停或撤销。
          </p>
        </div>
        <button className="btn primary" onClick={() => setCreating((v) => !v)}>
          <Icon name="plus" size={16} />
          新建连接
        </button>
      </header>
      {preview && (
        <p className="callout info-inline">
          这是浏览器预览：连接与确认流程使用示例数据，「一键写入」不会修改任何真实文件。
        </p>
      )}

      {creating && (
        <form
          className="card form-card"
          onSubmit={(e) => {
            e.preventDefault();
            void create();
          }}
        >
          <label className="field">
            <span>连接名称</span>
            <input
              data-autofocus
              autoFocus
              value={label}
              maxLength={100}
              onChange={(e) => setLabel(e.target.value)}
              placeholder="例如：Cursor、Claude Desktop"
            />
          </label>
          <div className="field">
            <span>权限</span>
            <Segmented
              label="权限"
              value={permission}
              onChange={setPermission}
              options={[
                { value: "write", label: "可读写" },
                { value: "read", label: "只读" },
              ]}
            />
            <small className="muted">
              {permission === "write"
                ? "可以创建和修改事项；删除永远需要你确认。"
                : "只能查询，任何修改都会被拒绝。"}
            </small>
          </div>
          <div className="actions">
            <button type="button" className="btn ghost" onClick={() => setCreating(false)}>
              取消
            </button>
            <button type="submit" className="btn primary" disabled={!label.trim() || busy}>
              创建连接
            </button>
          </div>
        </form>
      )}

      <section className="group">
        <h2 className="group-title">
          待你确认
          {pending.length > 0 && <span className="badge-count">{pending.length}</span>}
        </h2>
        {pending.length === 0 ? (
          <p className="muted soft-box">没有待确认的请求。助手想删除事项时，会先停在这里等你批准。</p>
        ) : (
          <div className="stack">
            {pending.map((p) => (
              <ProposalCard key={p.id} proposal={p} today={today} onDecide={(d) => onDecide(p.id, d)} />
            ))}
          </div>
        )}
        {recent.length > 0 && (
          <details className="recent">
            <summary>最近处理的请求（{recent.length}）</summary>
            <div className="stack">
              {recent.map((p) => (
                <ProposalCard key={p.id} proposal={p} today={today} compact />
              ))}
            </div>
          </details>
        )}
      </section>

      <section className="group">
        <h2 className="group-title">连接</h2>
        {connections && shown.length === 0 && (
          <div className="empty big">
            <div className="empty-art" aria-hidden="true" />
            <p>还没有连接</p>
            <small>点右上角「新建连接」，几秒钟就能把日历接入 Cursor、Claude 或 Codex。</small>
          </div>
        )}
        <div className="stack">
          {shown.map((c) => (
            <ConnectionCard
              key={c.id}
              c={c}
              open={openId === c.id}
              onToggle={() => setOpenId(openId === c.id ? undefined : c.id)}
              onState={(s) => setState(c, s)}
            />
          ))}
        </div>
        {revoked.length > 0 && (
          <p className="muted small">另有 {revoked.length} 个已撤销的连接（仅保留历史记录）。</p>
        )}
      </section>

      <section className="group">
        <div className="group-row">
          <h2 className="group-title">最近活动</h2>
          <select
            className="select"
            aria-label="按连接筛选活动"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          >
            <option value="all">全部连接</option>
            {(connections ?? []).map((c) => (
              <option key={c.id} value={c.id}>
                {c.label}
              </option>
            ))}
          </select>
        </div>
        {feed.length === 0 ? (
          <p className="muted soft-box">助手的每一次调用都会记录在这里（只存在你的电脑上）。</p>
        ) : (
          <ul className="feed">
            {feed.map((a) => (
              <li key={a.sequence} className={a.outcome}>
                <span className={cx("status-dot", a.outcome === "ok" ? "active" : "error")} aria-hidden="true" />
                <div className="feed-main">
                  <span className="feed-summary">{a.summary}</span>
                  <small className="muted">
                    {a.connection_label ?? "已移除的连接"} · {TOOL_LABEL[a.tool] ?? a.tool} · {a.duration_ms} ms
                  </small>
                </div>
                <small className="muted nowrap">{relativeTime(a.occurred_at, now)}</small>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
