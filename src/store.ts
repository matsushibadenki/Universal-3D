import { useEffect, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { preview, type Snapshot, type Command } from "./types";
export const desktop = isTauri();
export function useEditor() {
  const [snapshot, setSnapshot] = useState<Snapshot>(preview);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!desktop) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    // Subscribe before loading the snapshot so changes are never missed.
    void listen<Snapshot>("editor-changed", (e) => {
      if (!disposed) setSnapshot(e.payload);
    })
      .then(async (f) => {
        if (disposed) {
          f();
          return;
        }
        unlisten = f;
        const s = await invoke<Snapshot>("get_snapshot");
        if (!disposed) setSnapshot(s);
      })
      .catch((e) => setError(String(e)));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
  async function call(name: string, args?: Record<string, unknown>) {
    if (!desktop) return;
    setBusy(true);
    setError("");
    try {
      await invoke(name, args);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  const execute = (command: Command) => call("execute", { command });
  return { snapshot, error, busy, call, execute };
}
