import { useMemo, useState } from "react";
import { ApiError, colorName, type Item } from "../api";
import { MAX_DATE, MIN_DATE } from "../lib/dates";
import { automaticColor } from "../lib/itemColor";
import { diffItems } from "../lib/labels";
import { cx } from "../lib/util";
import { Icon } from "./icons";
import { Modal } from "./Modal";

export type Draft = {
  id?: string;
  revision: number;
  date: string;
  title: string;
  notes: string;
  color: string;
  status?: Item["status"];
};

export type DialogTarget =
  | { mode: "create"; date: string }
  | { mode: "edit"; item: Item };

const len = (s: string) => [...s].length;

export function ItemDialog({
  target,
  onSave,
  onToggle,
  onDelete,
  onClose,
}: {
  target: DialogTarget;
  /** 返回保存后的事项；失败时抛 ApiError。 */
  onSave: (draft: Draft) => Promise<void>;
  onToggle: (item: Item) => void;
  onDelete: (item: Item) => void;
  onClose: () => void;
}) {
  const original = useMemo<Draft>(
    () =>
      target.mode === "edit"
        ? {
            id: target.item.id,
            revision: target.item.revision,
            date: target.item.date,
            title: target.item.title,
            notes: target.item.notes,
            color: target.item.color,
            status: target.item.status,
          }
        : {
            revision: 0,
            date: target.date,
            title: "",
            notes: "",
            color: "sage",
          },
    [target],
  );
  const [draft, setDraft] = useState<Draft>(original);
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const [conflict, setConflict] = useState<Item>();
  const [confirmDiscard, setConfirmDiscard] = useState(false);

  const dirty =
    draft.title !== original.title ||
    draft.notes !== original.notes ||
    draft.date !== original.date ||
    draft.color !== original.color;
  const edit = target.mode === "edit" ? target.item : undefined;
  // 配色预示：新建时保存会按标题自动分配（见 useActions.save），编辑时保留原色。
  // 与保存时用的是同一个函数，所以这里看到的就是保存后的颜色。
  const hasTitle = draft.title.trim().length > 0;
  const previewColor = edit ? draft.color : hasTitle ? automaticColor(draft.title) : "";

  const requestClose = () => {
    if (dirty && !confirmDiscard) setConfirmDiscard(true);
    else onClose();
  };

  const save = async (next: Draft = draft) => {
    if (saving) return;
    const title = next.title.trim();
    if (!title || len(title) > 200 || len(next.notes) > 10000) {
      setError("标题需要 1–200 个字符，备注最多 10000 个字符。");
      return;
    }
    if (!next.date || next.date < MIN_DATE || next.date > MAX_DATE) {
      setError("日期需要在 1900–2100 年之间。");
      return;
    }
    setSaving(true);
    setError("");
    try {
      await onSave({ ...next, title });
      onClose();
    } catch (e) {
      const err = e as ApiError;
      if (err.code === "REVISION_CONFLICT") {
        setConflict(err.details.current_item as Item);
        setError("");
      } else setError(err.message);
    } finally {
      setSaving(false);
    }
  };

  const change = <K extends keyof Draft>(field: K, value: Draft[K]) => {
    setDraft((d) => ({ ...d, [field]: value }));
    setConfirmDiscard(false);
  };

  // 冲突时对比：「我的草稿」相对「服务里的最新版本」差在哪。
  const conflictDiff = conflict
    ? diffItems(conflict, {
        ...conflict,
        ...draft,
        status: conflict.status,
      } as Item)
    : [];

  return (
    <Modal
      label={edit ? "编辑事项" : "添加事项"}
      onClose={requestClose}
      className="dialog"
    >
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
            e.preventDefault();
            void save();
          }
        }}
      >
        <header className="dialog-head">
          <div>
            <p className="eyebrow">
              {edit ? "记录每一点进展" : "给重要的事，留一点时间"}
            </p>
            <h2>{edit ? "编辑事项" : "添加事项"}</h2>
          </div>
          <button
            type="button"
            className="btn ghost icon-btn"
            aria-label="关闭"
            onClick={requestClose}
          >
            <Icon name="x" />
          </button>
        </header>

        <div className="dialog-body">
          {conflict && (
            <div className="callout warn" role="alert">
              <Icon name="alert" />
              <div>
                <strong>这个事项刚刚被修改了</strong>
                <p>
                  可能是 AI
                  助手或另一处操作改过它。你的草稿还在，请选择如何处理：
                </p>
                {conflictDiff.length > 0 && (
                  <ul className="diff">
                    {conflictDiff.map((d) => (
                      <li key={d.label}>
                        <b>{d.label}</b> 最新：<span>{d.from}</span> →
                        你的草稿：<span>{d.to}</span>
                      </li>
                    ))}
                  </ul>
                )}
                <div className="actions">
                  <button
                    type="button"
                    className="btn"
                    onClick={() => {
                      setDraft({
                        id: conflict.id,
                        revision: conflict.revision,
                        date: conflict.date,
                        title: conflict.title,
                        notes: conflict.notes,
                        color: conflict.color,
                        status: conflict.status,
                      });
                      setConflict(undefined);
                    }}
                  >
                    载入最新版本
                  </button>
                  <button
                    type="button"
                    className="btn primary"
                    onClick={() => {
                      const next = { ...draft, revision: conflict.revision };
                      setDraft(next);
                      setConflict(undefined);
                      void save(next);
                    }}
                  >
                    用我的草稿覆盖
                  </button>
                </div>
              </div>
            </div>
          )}

          <label
            className={cx("field title-field", previewColor && `c-${previewColor}`)}
          >
            <span className="title-label">
              <span>标题</span>
              <em className="color-hint">
                {previewColor ? (
                  <>
                    <i className="swatch" aria-hidden="true" />
                    {colorName(previewColor)}
                    {edit ? " · 保留原色" : " · 自动配色"}
                  </>
                ) : (
                  "颜色将按标题自动分配"
                )}
              </em>
            </span>
            <input
              data-autofocus
              value={draft.title}
              maxLength={400}
              onChange={(e) => change("title", e.target.value)}
              placeholder="记下你想做的事…"
              required
            />
          </label>

          <label className="field date-field">
            <span>
              <Icon name="calendar" size={14} /> 日期
            </span>
            <input
              type="date"
              value={draft.date}
              min={MIN_DATE}
              max={MAX_DATE}
              onChange={(e) => change("date", e.target.value)}
              required
            />
          </label>

          <label className="field">
            <span>
              备注 <span className="optional">可选</span>
              {len(draft.notes) > 8000 && (
                <em
                  className={cx("counter", len(draft.notes) > 10000 && "over")}
                >
                  {len(draft.notes)} / 10000
                </em>
              )}
            </span>
            <textarea
              value={draft.notes}
              rows={3}
              onChange={(e) => change("notes", e.target.value)}
              placeholder="补充细节、想法，或需要记住的事"
            />
          </label>

          {error && (
            <p className="form-error" role="alert">
              {error}
            </p>
          )}
          {confirmDiscard && (
            <div className="callout" role="alert">
              <Icon name="alert" />
              <div>
                <strong>放弃未保存的修改？</strong>
                <div className="actions">
                  <button
                    type="button"
                    className="btn"
                    onClick={() => setConfirmDiscard(false)}
                  >
                    继续编辑
                  </button>
                  <button
                    type="button"
                    className="btn danger"
                    onClick={onClose}
                  >
                    放弃
                  </button>
                </div>
              </div>
            </div>
          )}
        </div>

        <footer className="dialog-foot">
          <div className="left">
            {edit && (
              <>
                <button
                  type="button"
                  className="btn ghost"
                  onClick={() => {
                    onToggle(edit);
                    onClose();
                  }}
                >
                  <Icon name="check" size={16} />
                  {edit.status === "done" ? "标为未完成" : "标为完成"}
                </button>
                <button
                  type="button"
                  className="btn ghost danger-text"
                  onClick={() => {
                    onDelete(edit);
                    onClose();
                  }}
                >
                  <Icon name="trash" size={16} />
                  删除
                </button>
              </>
            )}
          </div>
          <span className="hint">
            <kbd>⌘ ↵</kbd> 保存
          </span>
          <button
            type="submit"
            className="btn primary"
            disabled={saving || !draft.title.trim()}
          >
            {saving ? "保存中…" : edit ? "保存修改" : "添加事项"}
          </button>
        </footer>
      </form>
    </Modal>
  );
}
