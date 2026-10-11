import { useEffect, useState } from "react";
import { colorName, type Item, type Proposal } from "../api";
import { relativeTime, shortDate } from "../lib/dates";
import { diffItems } from "../lib/labels";
import { cx } from "../lib/util";
import { Icon } from "./icons";

export const KIND: Record<string, { label: string; tone: string }> = {
  delete: { label: "删除", tone: "danger" },
  update: { label: "修改", tone: "info" },
  status: { label: "完成状态", tone: "info" },
  create: { label: "创建", tone: "ok" },
};
const STATE: Record<Proposal["state"], string> = {
  pending: "待确认",
  approved: "已批准",
  rejected: "已拒绝",
  expired: "已过期",
  failed: "执行失败",
};

export function useNow(intervalMs = 15000) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(t);
  }, [intervalMs]);
  return now;
}

function ItemLine({ item, today }: { item: Item; today: string }) {
  return (
    <div className={cx("pitem", `c-${item.color}`)}>
      <b>{item.title}</b>
      <span>
        {shortDate(item.date, today)} · {colorName(item.color)}
        {item.status === "done" && " · 已完成"}
      </span>
      {item.notes && <em>{item.notes.split("\n")[0]}</em>}
    </div>
  );
}

export function expiryText(p: Proposal, now: number) {
  const left = Math.max(0, Math.round((Date.parse(p.expires_at) - now) / 60000));
  return left <= 0 ? "即将过期" : `${left} 分钟后过期`;
}

export function ProposalBody({ proposal: p, today }: { proposal: Proposal; today: string }) {
  const { before, after } = p.preview ?? { before: null, after: null };
  const diff = before && after ? diffItems(before, after) : [];
  return (
    <div className="proposal-body">
      {p.preview?.kind === "create" && after && <ItemLine item={after} today={today} />}
      {p.preview?.kind === "delete" && before && <ItemLine item={before} today={today} />}
      {(p.preview?.kind === "update" || p.preview?.kind === "status") && (
        <ul className="diff">
          {diff.map((d) => (
            <li key={d.label}>
              <b>{d.label}</b> <s>{d.from}</s> → <span>{d.to}</span>
            </li>
          ))}
          {!diff.length && <li>没有实际变化</li>}
        </ul>
      )}
      {p.preview?.reason && <p className="reason">理由：{p.preview.reason}</p>}
      {p.error && <p className="form-error">执行失败：{p.error.message}</p>}
    </div>
  );
}

export function ProposalCard({
  proposal: p,
  today,
  busy,
  onDecide,
  compact,
}: {
  proposal: Proposal;
  today: string;
  busy?: boolean;
  onDecide?: (decision: "approve" | "reject") => void;
  compact?: boolean;
}) {
  const now = useNow();
  const kind = KIND[p.preview?.kind] ?? { label: "操作", tone: "info" };
  const pending = p.state === "pending";

  return (
    <article className={cx("proposal", pending ? "pending" : p.state, compact && "compact")}>
      <header>
        <span className={cx("badge", kind.tone)}>{kind.label}</span>
        <h3>{p.summary}</h3>
        <span className={cx("state", p.state)}>{STATE[p.state]}</span>
      </header>
      <ProposalBody proposal={p} today={today} />
      <footer>
        <span className="muted">
          {p.connection_label ?? "已移除的连接"} · {relativeTime(p.created_at, now)}
          {pending && ` · ${expiryText(p, now)}`}
        </span>
        {pending && onDecide && (
          <span className="actions">
            <button className="btn" disabled={busy} onClick={() => onDecide("reject")}>
              拒绝
            </button>
            <button
              className={cx("btn", p.preview?.kind === "delete" ? "danger-solid" : "primary")}
              disabled={busy}
              onClick={() => onDecide("approve")}
            >
              <Icon name="check" size={16} />
              {p.preview?.kind === "delete" ? "批准删除" : "批准"}
            </button>
          </span>
        )}
      </footer>
    </article>
  );
}
