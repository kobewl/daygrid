import { useCallback, useEffect, useState } from "react";
import type { WeekStart } from "./dates";

export type Theme = "system" | "light" | "dark";
export type Prefs = { theme: Theme; weekStart: WeekStart; lastColor: string };

const KEY = "daygrid.prefs.v1";
const defaults: Prefs = { theme: "system", weekStart: 1, lastColor: "blue" };

function load(): Prefs {
  try {
    const raw = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    return {
      theme: ["system", "light", "dark"].includes(raw.theme)
        ? raw.theme
        : defaults.theme,
      weekStart: raw.weekStart === 0 ? 0 : 1,
      lastColor:
        typeof raw.lastColor === "string" ? raw.lastColor : defaults.lastColor,
    };
  } catch {
    return defaults;
  }
}

export function applyTheme(theme: Theme) {
  const root = document.documentElement;
  if (theme === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", theme);
}

/** 本机偏好（外观、周起始日、上次选的颜色）。只存在这台机器的 WebView 里。 */
export function usePrefs() {
  const [prefs, setPrefs] = useState<Prefs>(load);
  useEffect(() => applyTheme(prefs.theme), [prefs.theme]);
  const update = useCallback((patch: Partial<Prefs>) => {
    setPrefs((p) => {
      const next = { ...p, ...patch };
      try {
        localStorage.setItem(KEY, JSON.stringify(next));
      } catch {
        // 存储不可用（隐私模式等）时仍然在内存中生效。
      }
      return next;
    });
  }, []);
  return [prefs, update] as const;
}
