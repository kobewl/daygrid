import { useEffect, useMemo, useRef, useState } from "react";
import { ApiError, colorName, type Item, type Visibility } from "../api";
import { MAX_DATE, MIN_DATE } from "../lib/dates";
import { automaticColor } from "../lib/itemColor";
import { diffItems, VISIBILITY_LABEL } from "../lib/labels";
import { rememberedVisibility } from "../lib/visibility";
import { cx } from "../lib/util";
import { Icon } from "./icons";
import { Markdown } from "./Markdown";
import { Modal } from "./Modal";
import { Segmented } from "./Segmented";

export type Draft = {
  id?: string;
  revision: number;
  date: string;
  title: string;
  notes: string;
  color: string;
  status?: Item["status"];
  /** 连接了团队日历时才有：新建时的初始选择 / 编辑时当前的可见范围。 */
  visibility?: Visibility;
  /** 编辑时打开对话框那一刻的可见范围，用来判断有没有改。 */
  originalVisibility?: Visibility;
};

export type DialogTarget =
  | { mode: "create"; date: string }
  | { mode: "edit"; item: Item };

const len = (s: string) => [...s].length;

export function ItemDialog({
  target,
  team,
  onSave,
  onToggle,
  onDelete,
  onClose,
}: {
  target: DialogTarget;
  /** 团队协同状态：只有连接了团队才显示「可见范围」。`viewer` 不能共享事项，只显示说明。 */
  team?: { role: "owner" | "editor" | "viewer" } | null;
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
            ...(team
              ? {
                  visibility: target.item.visibility,
                  originalVisibility: target.item.visibility,
                }
              : {}),
          }
        : {
            revision: 0,
            date: target.date,
            title: "",
            notes: "",
            color: "sage",
            ...(team && team.role !== "viewer" ? { visibility: rememberedVisibility() } : {}),
          },
    [target, team],
  );
  const [draft, setDraft] = useState<Draft>(original);
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const [conflict, setConflict] = useState<Item>();
  const [confirmDiscard, setConfirmDiscard] = useState(false);
  // 已有备注的事项先以阅读形式打开，这样里面的链接能直接点；点空白处或「编辑」再修改。
  const [notesMode, setNotesMode] = useState<"edit" | "preview">(
    target.mode === "edit" && target.item.notes.trim() ? "preview" : "edit",
  );
  const notesRef = useRef<HTMLTextAreaElement>(null);
  const focusNotes = useRef(false);
  useEffect(() => {
    if (notesMode === "edit" && focusNotes.current) {
      focusNotes.current = false;
      notesRef.current?.focus();
    }
  }, [notesMode]);
  const editNotes = () => {
    focusNotes.current = true;
    setNotesMode("edit");
  };

  const dirty =
    draft.title !== original.title ||
    draft.notes !== original.notes ||
    draft.date !== original.date ||
    draft.color !== original.color ||
    draft.visibility !== original.visibility;
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
                        ...(team
                          ? { visibility: conflict.visibility, originalVisibility: conflict.visibility }
                          : {}),
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

          <div className="field">
            <div className="notes-head">
              <span id="notes-label">
                备注 <span className="optional">可选 · 支持 Markdown</span>
                {len(draft.notes) > 8000 && (
                  <em
                    className={cx("counter", len(draft.notes) > 10000 && "over")}
                  >
                    {len(draft.notes)} / 10000
                  </em>
                )}
              </span>
              <Segmented<"edit" | "preview">
                label="备注视图"
                className="notes-tabs"
                value={notesMode}
                onChange={(mode) => (mode === "edit" ? editNotes() : setNotesMode(mode))}
                options={[
                  { value: "edit", label: "编辑" },
                  { value: "preview", label: "预览" },
                ]}
              />
            </div>
            {notesMode === "edit" ? (
              <textarea
                ref={notesRef}
                aria-labelledby="notes-label"
                value={draft.notes}
                rows={3}
                onChange={(e) => change("notes", e.target.value)}
                placeholder="补充细节、想法，或需要记住的事。支持 Markdown：**加粗**、- 列表、[文字](链接)，网址会自动变成可点击的链接"
              />
            ) : (
              <div
                className="notes-preview"
                role="group"
                aria-labelledby="notes-label"
                onClick={(e) => {
                  // 点链接是打开网址；点其他地方回到编辑。选中文字（复制）时不打断。
                  if ((e.target as Element).closest("a")) return;
                  if (window.getSelection()?.toString()) return;
                  editNotes();
                }}
              >
                {draft.notes.trim() ? (
                  <Markdown source={draft.notes} />
                ) : (
                  <p className="muted">没有备注，点这里开始写</p>
                )}
              </div>
            )}
          </div>

          {team && (
            <div className="field visibility-field">
              <span id="visibility-label">谁能看到这个事项</span>
              {team.role === "viewer" && !(edit && edit.visibility === "shared") ? (
                <p className="muted small">你是只读成员：这个事项只保存在你自己的电脑上，不会共享。</p>
              ) : team.role === "viewer" ? (
                <p className="muted small">这是团队共享的事项，只读成员不能修改。</p>
              ) : (
                <>
                  <Segmented<Visibility>
                    label="可见范围"
                    value={draft.visibility ?? "private"}
                    onChange={(v) => change("visibility", v)}
                    options={[
                      { value: "private", label: VISIBILITY_LABEL.private },
                      { value: "shared", label: VISIBILITY_LABEL.shared },
                    ]}
                  />
                  <small className="muted">
                    {(draft.visibility ?? "private") === "shared"
                      ? "团队日历里的所有成员都能看到，也能一起编辑。"
                      : edit?.visibility === "shared"
                        ? "保存后会从团队日历撤回，同事将不再看到它；你这里的内容原样保留。"
                        : "只保存在这台电脑上，不会上传，同事看不到。"}
                  </small>
                </>
              )}
            </div>
          )}

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
