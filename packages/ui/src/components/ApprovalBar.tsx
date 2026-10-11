import { useState } from "react";
import type { Proposal } from "../api";
import { cx } from "../lib/util";
import { Icon } from "./icons";
import { KIND, ProposalBody, expiryText, useNow } from "./ProposalCard";

/**
 * 有助手请求待确认时，在页面顶部显示最早的一条（一行），可直接批准或拒绝；
 * 点「详情」展开修改前后的对比。不占用日历的垂直空间。
 */
export function ApprovalBar({
  pending,
  today,
  onDecide,
  onSeeAll,
}: {
  pending: Proposal[];
  today: string;
  onDecide: (id: string, decision: "approve" | "reject") => Promise<void>;
  onSeeAll: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState(false);
  const now = useNow();
  if (!pending.length) return null;
  // 列表里 pending 按新到旧排，最早提交的在最后，先处理它。
  const first = pending[pending.length - 1];
  const kind = KIND[first.preview?.kind] ?? { label: "操作", tone: "info" };
  const decide = async (decision: "approve" | "reject") => {
    setBusy(true);
    try {
      await onDecide(first.id, decision);
      setOpen(false);
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className="approval-bar" aria-label="待确认的助手请求" role="region">
      <div className="approval-line">
        <Icon name="shield" size={18} />
        <span className={cx("badge", kind.tone)}>{kind.label}</span>
        <strong className="approval-summary" title={first.summary}>
          {first.summary}
        </strong>
        <span className="muted approval-meta">
          {first.connection_label ?? "助手"} 请求 · {expiryText(first, now)}
        </span>
        <button className="link" onClick={() => setOpen((v) => !v)} aria-expanded={open}>
          {open ? "收起" : "详情"}
        </button>
        {pending.length > 1 && (
          <button className="link" onClick={onSeeAll}>
            还有 {pending.length - 1} 个
          </button>
        )}
        <span className="actions">
          <button className="btn" disabled={busy} onClick={() => void decide("reject")}>
            拒绝
          </button>
          <button
            className={cx("btn", first.preview?.kind === "delete" ? "danger-solid" : "primary")}
            disabled={busy}
            onClick={() => void decide("approve")}
          >
            <Icon name="check" size={16} />
            {first.preview?.kind === "delete" ? "批准删除" : "批准"}
          </button>
        </span>
      </div>
      {open && <ProposalBody proposal={first} today={today} />}
    </section>
  );
}
