import { Fragment, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { call, colorName, type Item } from "../api";
import { dayTitle, parseLooseDate, shortDate } from "../lib/dates";
import { cx, useDebounced } from "../lib/util";
import { Icon, type IconName } from "./icons";
import { Modal } from "./Modal";

export type Command = {
  id: string;
  label: string;
  hint?: string;
  icon: IconName;
  keywords?: string;
  run: () => void;
};

type Entry =
  | { kind: "date"; id: string; date: string }
  | { kind: "command"; id: string; command: Command }
  | { kind: "item"; id: string; item: Item; match: string };

function Highlight({ text, query }: { text: string; query: string }): ReactNode {
  const q = query.trim().toLowerCase();
  if (!q) return text;
  const at = text.toLowerCase().indexOf(q);
  if (at < 0) return text;
  return (
    <>
      {text.slice(0, at)}
      <mark>{text.slice(at, at + q.length)}</mark>
      {text.slice(at + q.length)}
    </>
  );
}

export function CommandPalette({
  today,
  commands,
  onGoDate,
  onOpenItem,
  onClose,
}: {
  today: string;
  commands: Command[];
  onGoDate: (date: string) => void;
  onOpenItem: (item: Item) => void;
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<{ item: Item; match: string }[]>([]);
  const [more, setMore] = useState(false);
  const [error, setError] = useState("");
  const [active, setActive] = useState(0);
  const debounced = useDebounced(query.trim(), 120);
  const seq = useRef(0);
  const listRef = useRef<HTMLUListElement>(null);

  useEffect(() => {
    const id = ++seq.current;
    if (!debounced) {
      setHits([]);
      setMore(false);
      setError("");
      return;
    }
    call<{ results: { item: Item; match: string }[]; has_more: boolean }>("desktop_search", {
      query: debounced,
      limit: 20,
    })
      .then((r) => {
        if (id !== seq.current) return; // 已有更新的输入，丢弃旧结果
        setHits(r.results);
        setMore(r.has_more);
        setError("");
      })
      .catch((e: Error) => id === seq.current && setError(e.message));
  }, [debounced]);

  const entries = useMemo<Entry[]>(() => {
    const q = query.trim().toLowerCase();
    const list: Entry[] = [];
    const date = q ? parseLooseDate(q, today) : undefined;
    if (date) list.push({ kind: "date", id: "date:" + date, date });
    for (const command of commands) {
      if (!q || (command.label + " " + (command.keywords ?? "")).toLowerCase().includes(q))
        list.push({ kind: "command", id: command.id, command });
    }
    for (const h of hits) list.push({ kind: "item", id: h.item.id, item: h.item, match: h.match });
    return list;
  }, [query, commands, hits, today]);

  useEffect(() => setActive(0), [query]);
  useEffect(() => {
    listRef.current?.querySelector<HTMLElement>(`[data-index="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }, [active]);

  const run = (entry: Entry | undefined) => {
    if (!entry) return;
    onClose();
    if (entry.kind === "date") onGoDate(entry.date);
    else if (entry.kind === "command") entry.command.run();
    else {
      onGoDate(entry.item.date);
      onOpenItem(entry.item);
    }
  };

  const heading = (i: number) => {
    const e = entries[i];
    if (i > 0 && entries[i - 1].kind === e.kind) return undefined;
    return e.kind === "date" ? "跳转到日期" : e.kind === "command" ? "命令" : "事项";
  };

  return (
    <Modal label="搜索与命令" onClose={onClose} className="palette" placement="top">
      <div className="palette-input">
        <Icon name="search" />
        <input
          data-autofocus
          role="combobox"
          aria-expanded="true"
          aria-controls="palette-list"
          aria-activedescendant={entries[active] ? `pal-${active}` : undefined}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="搜索事项，或输入 10-20、明天 跳转，或输入命令…"
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setActive((a) => Math.min(entries.length - 1, a + 1));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setActive((a) => Math.max(0, a - 1));
            } else if (e.key === "Enter") {
              e.preventDefault();
              run(entries[active]);
            }
          }}
        />
        <kbd>esc</kbd>
      </div>
      <ul className="palette-list" id="palette-list" role="listbox" ref={listRef}>
        {entries.map((entry, i) => (
          <Fragment key={entry.id}>
            {heading(i) && (
              <li className="palette-group" role="presentation">
                {heading(i)}
              </li>
            )}
            <li
              id={`pal-${i}`}
              data-index={i}
              role="option"
              aria-selected={i === active}
              className={cx("palette-item", i === active && "on")}
              onMouseMove={() => setActive(i)}
              onClick={() => run(entry)}
            >
              {entry.kind === "date" && (
                <>
                  <Icon name="calendar" />
                  <span className="grow">
                    跳转到 {entry.date}（{dayTitle(entry.date)}）
                  </span>
                </>
              )}
              {entry.kind === "command" && (
                <>
                  <Icon name={entry.command.icon} />
                  <span className="grow">{entry.command.label}</span>
                  {entry.command.hint && <kbd>{entry.command.hint}</kbd>}
                </>
              )}
              {entry.kind === "item" && (
                <>
                  <i className={cx("swatch", `c-${entry.item.color}`)} title={colorName(entry.item.color)} />
                  <span className="grow">
                    <Highlight text={entry.item.title} query={debounced} />
                    {entry.match === "notes" && (
                      <small className="muted"> · 备注中匹配</small>
                    )}
                  </span>
                  <small className="muted">
                    {shortDate(entry.item.date, today)}
                    {entry.item.status === "done" && " · 已完成"}
                  </small>
                </>
              )}
            </li>
          </Fragment>
        ))}
        {!entries.length && (
          <li className="palette-empty">{error ? error : debounced ? "没有找到匹配的事项或命令" : "输入内容开始搜索"}</li>
        )}
        {more && <li className="palette-empty">只显示最近的 20 条，请输入更具体的关键词。</li>}
      </ul>
    </Modal>
  );
}
