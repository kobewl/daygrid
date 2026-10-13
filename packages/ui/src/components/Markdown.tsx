import { useMemo, type ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkBreaks from "remark-breaks";
import remarkGfm from "remark-gfm";
import { linkifyBareUrls } from "../lib/linkify";
import { openExternal, safeUrl } from "../lib/openExternal";
import { cx } from "../lib/util";

/** 备注里的链接：点击交给系统浏览器，不在日格窗口里跳转。 */
function ExternalLink({
  href,
  children,
}: {
  href?: string;
  children?: ReactNode;
}) {
  const url = safeUrl(href);
  // 不是 http / https / mailto 的链接（javascript:、file: 等）只显示文字，不可点击。
  if (!url) return <>{children}</>;
  return (
    <a
      className="md-link"
      href={url}
      title={url}
      rel="noreferrer noopener"
      onClick={(e) => {
        e.preventDefault();
        e.stopPropagation();
        void openExternal(url);
      }}
    >
      {children}
    </a>
  );
}

const components: Components = {
  a: ExternalLink,
  // 日历是本地应用，CSP 也不允许加载远程图片；图片退化成可点击的链接。
  img: ({ src, alt }) => (
    <ExternalLink href={typeof src === "string" ? src : undefined}>
      {alt || src}
    </ExternalLink>
  ),
  // 任务列表的复选框只用来展示，不能在这里勾选（不会保存）。
  input: ({ checked }) => (
    <input
      type="checkbox"
      checked={!!checked}
      readOnly
      disabled
      aria-hidden="true"
    />
  ),
};

const inlineComponents: Components = {
  ...components,
  p: ({ children }) => <span>{children}</span>,
};
const INLINE_ELEMENTS = ["p", "a", "strong", "em", "del", "code"];

const remarkPlugins = [remarkGfm, remarkBreaks];
// 链接只认 http / https / mailto；其余协议返回空串，由 ExternalLink 当作普通文字。
const urlTransform = (url: string) => safeUrl(url) ?? "";

/** 备注里值得做摘要的第一行：跳过空行与代码围栏标记。 */
export function firstNoteLine(notes: string) {
  return (
    notes
      .split("\n")
      .find((line) => line.trim() && !/^\s*(```|~~~)/.test(line)) ?? ""
  );
}

/**
 * 渲染备注（Markdown）。不解析原始 HTML，链接只放行 http / https / mailto。
 * `inline`：只保留行内格式（加粗、斜体、代码、链接），用于单行摘要。
 */
export function Markdown({
  source,
  inline = false,
  className,
}: {
  source: string;
  inline?: boolean;
  className?: string;
}) {
  const text = useMemo(() => linkifyBareUrls(source), [source]);
  return (
    <div className={cx("md", inline && "md-inline", className)}>
      <ReactMarkdown
        remarkPlugins={remarkPlugins}
        components={inline ? inlineComponents : components}
        urlTransform={urlTransform}
        skipHtml
        {...(inline
          ? { allowedElements: INLINE_ELEMENTS, unwrapDisallowed: true }
          : {})}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
}
