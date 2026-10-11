import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { Icon } from "../components/icons";

export type Toast = {
  id: number;
  text: string;
  tone: "info" | "error";
  action?: { label: string; run: () => void };
};
type Notify = (
  text: string,
  options?: { tone?: Toast["tone"]; action?: Toast["action"]; ms?: number },
) => void;

const Ctx = createContext<Notify>(() => undefined);
export const useNotify = () => useContext(Ctx);

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const next = useRef(1);
  const dismiss = useCallback(
    (id: number) => setToasts((t) => t.filter((x) => x.id !== id)),
    [],
  );
  const notify = useCallback<Notify>(
    (text, options = {}) => {
      const id = next.current++;
      const tone = options.tone ?? "info";
      setToasts((t) => [...t.slice(-3), { id, text, tone, action: options.action }]);
      // 带「撤销」的提示多留一会儿；错误不自动消失太快。
      const ms = options.ms ?? (options.action ? 8000 : tone === "error" ? 7000 : 3500);
      setTimeout(() => dismiss(id), ms);
    },
    [dismiss],
  );
  const value = useMemo(() => notify, [notify]);
  return (
    <Ctx.Provider value={value}>
      {children}
      <div className="toasts" role="region" aria-label="通知" aria-live="polite">
        {toasts.map((t) => (
          <div key={t.id} className={`toast ${t.tone}`} role={t.tone === "error" ? "alert" : "status"}>
            <span>{t.text}</span>
            {t.action && (
              <button
                className="toast-action"
                onClick={() => {
                  t.action!.run();
                  dismiss(t.id);
                }}
              >
                {t.action.label}
              </button>
            )}
            <button className="toast-close" aria-label="关闭通知" onClick={() => dismiss(t.id)}>
              <Icon name="x" size={14} />
            </button>
          </div>
        ))}
      </div>
    </Ctx.Provider>
  );
}
