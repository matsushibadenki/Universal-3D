import { useEffect, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  preview,
  type Snapshot,
  type Command,
  type EditorChange,
} from "./types";
import { applyChange } from "./sync";
export const desktop = isTauri();
export function useEditor() {
  const [snapshot, setSnapshot] = useState<Snapshot>(preview);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!desktop) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    let current: Snapshot | null = null;
    let pending: EditorChange[] = [];
    let recovering = false;
    function publish(value: Snapshot) {
      current = value;
      if (!disposed) setSnapshot(value);
    }
    async function recover() {
      if (disposed || recovering) return;
      recovering = true;
      try {
        const value = await invoke<Snapshot>("get_snapshot");
        if (disposed) return;
        if (!current || value.revision >= current.revision) publish(value);
        const events = pending.sort((a, b) => a.revision - b.revision);
        pending = [];
        for (const event of events) {
          const next = applyChange(current, event);
          if (next) publish(next);
          else pending.push(event);
        }
      } catch (e) {
        if (!disposed) setError(String(e));
        pending = [];
      } finally {
        recovering = false;
      }
      if (pending.length && !disposed) void recover();
    }
    // Listen first; late snapshots and duplicate/old events cannot roll state back.
    void listen<EditorChange>("editor-changed", (event) => {
      if (disposed) return;
      const next = applyChange(current, event.payload);
      if (next) publish(next);
      else {
        pending.push(event.payload);
        void recover();
      }
    })
      .then((f) => {
        if (disposed) {
          f();
          return;
        }
        unlisten = f;
        void recover();
      })
      .catch((e) => {
        if (!disposed) setError(String(e));
      });
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
