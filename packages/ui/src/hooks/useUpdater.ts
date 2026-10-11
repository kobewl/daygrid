import { useRef, useState } from "react";
import {
  checkUpdate,
  installUpdate,
  restartAfterUpdate,
  type UpdateInfo,
  type UpdateProgress,
} from "../lib/updater";

export type UpdateState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "latest" }
  | { kind: "installed" }
  | { kind: "available"; info: UpdateInfo }
  | { kind: "installing"; progress: UpdateProgress }
  | { kind: "error"; message: string };

/** 由应用根组件持有，切换页面不会丢失下载状态或重复发起安装。 */
export function useUpdater() {
  const [state, setState] = useState<UpdateState>({ kind: "idle" });
  const busy = useRef(false);
  const check = async () => {
    if (busy.current) return;
    busy.current = true;
    setState({ kind: "checking" });
    try {
      const info = await checkUpdate();
      setState(info ? { kind: "available", info } : { kind: "latest" });
    } catch (e) {
      setState({
        kind: "error",
        message: String(e instanceof Error ? e.message : e),
      });
    } finally {
      busy.current = false;
    }
  };
  const install = async () => {
    if (busy.current || state.kind !== "available") return;
    busy.current = true;
    const version = state.info.version;
    setState({
      kind: "installing",
      progress: { phase: "downloading", downloaded: 0, total: null },
    });
    try {
      await installUpdate(version, (progress) =>
        setState({ kind: "installing", progress }),
      );
      // 桌面端通常已经重启；若仍返回，则提供明确的手动重启入口。
      setState({ kind: "installed" });
    } catch (e) {
      setState({
        kind: "error",
        message: String(e instanceof Error ? e.message : e),
      });
    } finally {
      busy.current = false;
    }
  };
  const restart = async () => {
    try {
      await restartAfterUpdate();
    } catch (e) {
      setState({
        kind: "error",
        message: String(e instanceof Error ? e.message : e),
      });
    }
  };
  return { state, check, install, restart };
}
export type Updater = ReturnType<typeof useUpdater>;
