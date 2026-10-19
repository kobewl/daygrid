import { useState } from "react";
import { call, type SyncConflict, type SyncInvite, type SyncRole, type SyncState, type SyncStatus } from "../api";
import type { Sync } from "../hooks/useSync";
import { useNotify } from "../hooks/useToasts";
import { relativeTime } from "../lib/dates";
import { conflictFieldLabel, conflictValue } from "../lib/labels";
import { copyText } from "../lib/util";
import { Avatars } from "./Avatars";
import { Icon } from "./icons";
import { Segmented } from "./Segmented";

export const STATE_LABEL: Record<SyncState, string> = {
  off: "未连接",
  connecting: "正在连接…",
  syncing: "同步中…",
  online: "已同步",
  offline: "离线（恢复网络后自动同步）",
  error: "需要处理",
};
export const ROLE_LABEL: Record<SyncRole, string> = {
  owner: "所有者",
  editor: "可编辑",
  viewer: "只读",
};

/** 与后端 `insecure_url` 同一规则：http 只对本机地址放行，其余提示明文风险。 */
export const isInsecure = (url: string) =>
  /^http:\/\//i.test(url.trim()) && !/^http:\/\/(localhost|127\.0\.0\.1|\[::1\])(:|\/|$)/i.test(url.trim());

function ConnectForm({ status, sync }: { status?: SyncStatus; sync: Sync }) {
  const notify = useNotify();
  const [url, setUrl] = useState("");
  const [how, setHow] = useState<"invite" | "token">("invite");
  const [secret, setSecret] = useState("");
  const [name, setName] = useState("");
  const [team, setTeam] = useState("");
  const [busy, setBusy] = useState(false);
  // 连接前先问清楚：本机已有的事项要不要共享给团队（默认不共享）。
  const [asking, setAsking] = useState(false);
  const [shareExisting, setShareExisting] = useState(false);
  const connecting = busy || status?.state === "connecting";
  const local = status?.local_items ?? 0;

  const submit = async () => {
    setBusy(true);
    try {
      await call("desktop_sync_connect", {
        url,
        ...(how === "invite" ? { invite: secret, display_name: name } : { token: secret }),
        ...(how === "token" && team.trim() ? { workspace: team.trim() } : {}),
        share_existing: shareExisting,
      });
      setSecret("");
      setAsking(false);
      setShareExisting(false);
      await sync.refresh();
    } catch (e) {
      notify((e as Error).message, { tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      className="collab-form"
      onSubmit={(e) => {
        e.preventDefault();
        if (local > 0 && !asking) setAsking(true);
        else void submit();
      }}
    >
      <p className="muted small">
        多人共用同一份日历：每个人的修改先保存在自己电脑上（断网也能用），联网后自动合并。服务器由团队自己部署（
        <code>daygrid-server</code>），详见 README。
      </p>
      <label className="field">
        <span>服务器地址</span>
        <input
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="https://calendar.example.com 或 http://127.0.0.1:7878"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          required
        />
      </label>
      {isInsecure(url) && (
        <p className="callout warn">
          <Icon name="alert" />
          <span>这是 http 地址：访问令牌和日历内容会在网络上明文传输。只建议在可信的内网或本机使用。</span>
        </p>
      )}
      <Segmented<"invite" | "token">
        label="加入方式"
        value={how}
        onChange={(v) => {
          setHow(v);
          setSecret("");
        }}
        options={[
          { value: "invite", label: "我有邀请码" },
          { value: "token", label: "我有访问令牌" },
        ]}
      />
      {how === "invite" && (
        <label className="field">
          <span>你的显示名（同事看到的名字）</span>
          <input value={name} onChange={(e) => setName(e.target.value)} maxLength={40} required />
        </label>
      )}
      {how === "token" && (
        <label className="field">
          <span>
            团队名称 <span className="optional">可选 · 令牌加入了多个团队时填写</span>
          </span>
          <input value={team} onChange={(e) => setTeam(e.target.value)} maxLength={40} />
        </label>
      )}
      <label className="field">
        <span>{how === "invite" ? "邀请码" : "访问令牌"}</span>
        <input
          type="password"
          value={secret}
          onChange={(e) => setSecret(e.target.value)}
          autoComplete="off"
          spellCheck={false}
          required
        />
      </label>
      {status?.state === "error" && status.last_error && <p className="form-error">{status.last_error}</p>}
      {asking && (
        <div className="callout" role="group" aria-label="是否共享现有事项">
          <Icon name="users" />
          <div>
            <strong>你的日历里已有 {local} 个事项，要共享给团队吗？</strong>
            <label className="check-line">
              <input type="radio" name="share" checked={!shareExisting} onChange={() => setShareExisting(false)} />
              <span>
                不共享（推荐）<small className="muted">——它们保持「仅自己可见」，只留在这台电脑上，之后可以逐个改成共享。</small>
              </span>
            </label>
            <label className="check-line">
              <input type="radio" name="share" checked={shareExisting} onChange={() => setShareExisting(true)} />
              <span>
                全部共享<small className="muted">——所有同事都会看到这 {local} 个事项，也能修改它们。</small>
              </span>
            </label>
          </div>
        </div>
      )}
      <div className="actions">
        <button className="btn primary" type="submit" disabled={connecting}>
          {connecting ? "正在连接…" : asking ? (shareExisting ? "共享并连接" : "连接（不共享）") : "连接"}
        </button>
        {asking && !connecting && (
          <button type="button" className="btn ghost" onClick={() => setAsking(false)}>
            返回
          </button>
        )}
        <small className="muted">
          令牌只保存在本机一个受保护的文件里，不会进入数据库、备份或日志。
        </small>
      </div>
    </form>
  );
}

function Invite({ role }: { role: SyncRole }) {
  const notify = useNotify();
  const [as, setAs] = useState<"editor" | "viewer">("editor");
  const [invite, setInvite] = useState<SyncInvite>();
  const [busy, setBusy] = useState(false);
  if (role !== "owner") return null;

  const create = async () => {
    setBusy(true);
    try {
      setInvite(await call<SyncInvite>("desktop_sync_invite", { role: as, ttl_hours: 72, max_uses: 1 }));
    } catch (e) {
      notify((e as Error).message, { tone: "error" });
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="setting">
      <div className="setting-text">
        <strong>邀请同事</strong>
        <p className="muted">生成一次性邀请码（72 小时内有效），发给同事，让他在这里填入即可加入。</p>
        {invite && (
          <p className="invite-code">
            <code className="mono">{invite.code}</code>
            <button
              type="button"
              className="btn ghost icon-btn"
              aria-label="复制邀请码"
              onClick={async () => notify((await copyText(invite.code)) ? "已复制" : "复制失败")}
            >
              <Icon name="copy" size={15} />
            </button>
            <small className="muted">
              {ROLE_LABEL[invite.role]} · 有效至{" "}
              {new Date(invite.expires_at).toLocaleString("zh-CN", {
                month: "numeric",
                day: "numeric",
                hour: "2-digit",
                minute: "2-digit",
              })}
            </small>
          </p>
        )}
      </div>
      <div className="setting-control btn-row">
        <Segmented<"editor" | "viewer">
          label="邀请角色"
          value={as}
          onChange={setAs}
          options={[
            { value: "editor", label: "可编辑" },
            { value: "viewer", label: "只读" },
          ]}
        />
        <button className="btn" onClick={create} disabled={busy}>
          生成邀请码
        </button>
      </div>
    </div>
  );
}

const REASON: Record<string, string> = {
  concurrent_edit: "同时改了同一处",
  item_deleted: "这个事项在你修改的同时被删除了",
  invalid: "服务器没有接受这次修改",
  read_only: "你变成了只读成员，这次修改没有上传",
};

function ConflictRow({ c, onDone }: { c: SyncConflict; onDone: () => void }) {
  const notify = useNotify();
  const [busy, setBusy] = useState(false);
  const resolve = async (action: "restore" | "dismiss") => {
    setBusy(true);
    try {
      await call("desktop_sync_resolve_conflict", { id: c.id, action });
      notify(action === "restore" ? "已把你的版本写回" : "已忽略");
      onDone();
    } catch (e) {
      notify((e as Error).message, { tone: "error" });
    } finally {
      setBusy(false);
    }
  };
  const deleted = c.reason === "item_deleted";
  // 没有"对方的版本"：是被服务器拒绝 / 因只读没上传的修改，只能看你当时写了什么。
  const unsent = c.reason === "invalid" || c.reason === "read_only";
  const whole = c.field === "*";
  return (
    <li className="conflict">
      <div className="conflict-head">
        <strong>{c.item_title ?? "（已删除的事项）"}</strong>
        <span className="muted small">
          {conflictFieldLabel(c.field)} · {REASON[c.reason] ?? c.reason} · {relativeTime(c.occurred_at)}
        </span>
      </div>
      {c.message && <p className="muted small conflict-message">{c.message}</p>}
      {!whole && (
        <dl className="conflict-diff">
          <div>
            <dt>{unsent ? "现在的内容" : c.kept_by ? `${c.kept_by} 的版本（已保留）` : "已保留"}</dt>
            <dd>{deleted ? "事项已删除" : conflictValue(c.field, c.kept_value)}</dd>
          </div>
          <div>
            <dt>{unsent ? "你的修改（没有生效）" : "你的版本（被覆盖）"}</dt>
            <dd>{conflictValue(c.field, c.lost_value)}</dd>
          </div>
        </dl>
      )}
      <div className="btn-row">
        {!whole && (
          <button className="btn" disabled={busy} onClick={() => resolve("restore")}>
            <Icon name="undo" size={15} />
            {deleted ? "恢复事项并写回我的修改" : unsent ? "重新应用我的修改" : "用我的版本覆盖"}
          </button>
        )}
        <button className="btn ghost" disabled={busy} onClick={() => resolve("dismiss")}>
          忽略
        </button>
      </div>
    </li>
  );
}

export function CollabSettings({ sync }: { sync: Sync }) {
  const notify = useNotify();
  const { status, conflicts } = sync;
  const [confirmLeave, setConfirmLeave] = useState(false);
  const open = conflicts.filter((c) => c.state === "open");
  const cfg = status?.linked ? status.config : null;

  const leave = async () => {
    try {
      await call("desktop_sync_disconnect");
      setConfirmLeave(false);
      notify("已断开协同，本机事项原样保留");
      await sync.refresh();
    } catch (e) {
      notify((e as Error).message, { tone: "error" });
    }
  };

  return (
    <section className="card" id="collab">
      <h2>多人协作</h2>
      {!cfg || !status ? (
        <ConnectForm status={status} sync={sync} />
      ) : (
        <>
          {cfg.insecure && (
            <p className="callout warn">
              <Icon name="alert" />
              <span>当前连接使用 http：令牌和日历内容在网络上明文传输。</span>
            </p>
          )}
          <dl className="about">
            <dt>团队日历</dt>
            <dd>
              {cfg.workspace_name} · 你是「{cfg.user_name}」（{ROLE_LABEL[cfg.role]}）
            </dd>
            <dt>状态</dt>
            <dd>
              <span className={`dot ${status.state}`} aria-hidden="true" /> {STATE_LABEL[status.state]}
              {status.pending > 0 && ` · ${status.pending} 项修改等待上传`}
              {status.last_sync_at && ` · 上次同步 ${relativeTime(status.last_sync_at)}`}
            </dd>
            {status.state === "error" && status.last_error && (
              <>
                <dt>问题</dt>
                <dd className="form-error">{status.last_error}</dd>
              </>
            )}
            <dt>服务器</dt>
            <dd className="mono">{cfg.url}</dd>
            <dt>在线成员</dt>
            <dd>
              {status.peers.length ? (
                <>
                  <Avatars peers={status.peers} max={8} />{" "}
                  <span className="muted">{[...new Set(status.peers.map((p) => p.user))].join("、")}</span>
                </>
              ) : (
                <span className="muted">只有你</span>
              )}
            </dd>
          </dl>
          {cfg.role === "viewer" && (
            <p className="callout info-inline">
              <Icon name="shield" />
              <span>你是只读成员：可以查看和实时接收同事的修改，不能修改团队共享的事项；你自己的私有事项照常可用。</span>
            </p>
          )}
          <Invite role={cfg.role} />
          <div className="setting">
            <div className="setting-text">
              <strong>断开协同</strong>
              <p className="muted">
                {status.pending > 0
                  ? `还有 ${status.pending} 项修改尚未上传，断开后它们只留在这台电脑上。`
                  : "断开后这台电脑上的事项原样保留，之后不再与团队同步。"}
              </p>
            </div>
            <div className="setting-control btn-row">
              {confirmLeave ? (
                <>
                  <button className="btn danger" onClick={leave}>
                    确认断开
                  </button>
                  <button className="btn ghost" onClick={() => setConfirmLeave(false)}>
                    取消
                  </button>
                </>
              ) : (
                <button className="btn" onClick={() => setConfirmLeave(true)}>
                  断开
                </button>
              )}
            </div>
          </div>
          <h3 className="conflict-title">
            需要你看一眼的冲突{open.length > 0 && <span className="badge warn">{open.length}</span>}
          </h3>
          {open.length === 0 ? (
            <p className="muted small">
              没有。两个人同时改同一事项的不同字段，会自动合并；改到同一处时保留较新的一份，被覆盖的会出现在这里，随时可以找回。
            </p>
          ) : (
            <ul className="conflicts">
              {open.map((c) => (
                <ConflictRow key={c.id} c={c} onDone={() => void sync.refresh()} />
              ))}
            </ul>
          )}
        </>
      )}
    </section>
  );
}
