import { useEffect, useRef, type ReactNode } from "react";
import { cx } from "../lib/util";

const FOCUSABLE =
  'button:not([disabled]),[href],input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])';

/** 对话框：焦点陷阱、Esc 关闭、关闭后把焦点还给触发它的元素。 */
export function Modal({
  label,
  onClose,
  children,
  className,
  placement = "center",
}: {
  label: string;
  onClose: () => void;
  children: ReactNode;
  className?: string;
  placement?: "center" | "top";
}) {
  const ref = useRef<HTMLDivElement>(null);
  const close = useRef(onClose);
  close.current = onClose;

  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    const node = ref.current!;
    const focusable = () => Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE));
    (node.querySelector<HTMLElement>("[data-autofocus]") ?? focusable()[0] ?? node).focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        close.current();
      } else if (e.key === "Tab") {
        const list = focusable();
        if (!list.length) return;
        const first = list[0];
        const last = list[list.length - 1];
        if (e.shiftKey && document.activeElement === first) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && document.activeElement === last) {
          e.preventDefault();
          first.focus();
        }
      }
    };
    node.addEventListener("keydown", onKey);
    return () => {
      node.removeEventListener("keydown", onKey);
      previous?.focus?.();
    };
  }, []);

  return (
    <div
      className={cx("backdrop", placement === "top" && "top")}
      onMouseDown={(e) => e.target === e.currentTarget && onClose()}
    >
      <div
        ref={ref}
        className={cx("modal", className)}
        role="dialog"
        aria-modal="true"
        aria-label={label}
        tabIndex={-1}
      >
        {children}
      </div>
    </div>
  );
}
