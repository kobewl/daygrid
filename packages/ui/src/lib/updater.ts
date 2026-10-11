import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { preview } from "../api";

export type UpdateInfo = {
  currentVersion: string;
  version: string;
  notes: string;
};
export type UpdateProgress = {
  phase: "downloading" | "installing";
  downloaded: number;
  total: number | null;
};
const demoScenario = () =>
  new URLSearchParams(location.search).get("update-preview");

export async function checkUpdate(): Promise<UpdateInfo | null> {
  if (!preview) return invoke("check_update");
  await new Promise((resolve) => setTimeout(resolve, 250));
  if (demoScenario() === "error")
    throw new Error("演示：暂时无法连接更新服务器，请稍后重试。");
  if (!demoScenario() || demoScenario() === "latest") return null;
  return {
    currentVersion: "演示版本",
    version: "下一版本（演示）",
    notes: "改进日历体验与稳定性。\n保留本机的事项、备注和 MCP 连接。",
  };
}

export async function installUpdate(
  version: string,
  onProgress: (value: UpdateProgress) => void,
): Promise<void> {
  if (preview) {
    for (let step = 1; step <= 5; step++) {
      await new Promise((resolve) => setTimeout(resolve, 300));
      onProgress({
        phase: "downloading",
        downloaded: step * 200,
        total: demoScenario() === "unknown-size" ? null : 1000,
      });
      if (demoScenario() === "download-error" && step === 3)
        throw new Error("演示：下载中断，可以重新检查更新后重试。");
    }
    onProgress({ phase: "installing", downloaded: 0, total: null });
    await new Promise((resolve) => setTimeout(resolve, 300));
    return;
  }
  // 先订阅，再开始安装，避免漏掉下载开始的进度；结束后始终释放订阅。
  const unlisten = await listen<UpdateProgress>("update:progress", (event) =>
    onProgress(event.payload),
  );
  try {
    await invoke("install_update", { version });
  } finally {
    unlisten();
  }
}

export async function openReleasePage(): Promise<void> {
  if (preview)
    window.open(
      "https://github.com/kobewl/daygrid/releases/latest",
      "_blank",
      "noopener,noreferrer",
    );
  else await invoke("open_release_page");
}

export async function restartAfterUpdate(): Promise<void> {
  if (!preview) await invoke("restart_after_update");
}
