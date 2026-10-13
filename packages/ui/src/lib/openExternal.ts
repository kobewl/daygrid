import { invoke } from "@tauri-apps/api/core";
import { preview } from "../api";

const ALLOWED = /^(https?:\/\/|mailto:)/i;

/** 只放行 http、https、mailto；其他协议（javascript:、file: 等）一律不当作链接。 */
export function safeUrl(href: string | undefined | null): string | undefined {
  const url = href?.trim();
  return url && ALLOWED.test(url) && !/[\s\u0000-\u001f]/.test(url)
    ? url
    : undefined;
}

/** 用系统默认的浏览器（或邮件客户端）打开；桌面应用里由 Rust 端校验后调用 `open`。 */
export async function openExternal(url: string) {
  const safe = safeUrl(url);
  if (!safe) return;
  if (preview) window.open(safe, "_blank", "noopener,noreferrer");
  else await invoke("open_external", { url: safe });
}

/**
 * 兜底：任何没被处理的 http(s)/mailto 链接点击，都改走系统浏览器，
 * 避免 WebView 自己跳转到网页、把日历界面替换掉。
 */
export function installLinkGuard() {
  const handler = (e: MouseEvent) => {
    if (e.defaultPrevented) return;
    const anchor = (e.target as Element | null)?.closest?.("a[href]");
    if (!anchor) return;
    const url = safeUrl(anchor.getAttribute("href"));
    if (!url) return;
    e.preventDefault();
    void openExternal(url).catch(() => undefined);
  };
  document.addEventListener("click", handler);
  document.addEventListener("auxclick", handler);
  return () => {
    document.removeEventListener("click", handler);
    document.removeEventListener("auxclick", handler);
  };
}
