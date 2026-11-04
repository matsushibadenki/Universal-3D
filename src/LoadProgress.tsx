import { useState, useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import { X } from "lucide-react";
import { desktop } from "./store";
import { IconButton } from "./EditorChrome";
import type { Key } from "./i18n";

type Kind = "open" | "recovery" | "usd";
type Progress = {
  id: string;
  kind: Kind;
  phase:
    | "read"
    | "convert"
    | "validate"
    | "vertices"
    | "surfaces"
    | "instances"
    | "commit";
  completed: number;
  total: number;
  cancelling: boolean;
  cancellable: boolean;
};
// A single small poll per WebView, shared by footer and modal subscribers.
let current: Progress | null = null;
const listeners = new Set<() => void>();
let generation = 0;
let timer: ReturnType<typeof setTimeout> | undefined;
async function refresh(version: number) {
  try {
    const next = await invoke<Progress | null>("get_load_progress");
    if (version !== generation) return;
    if (JSON.stringify(current) !== JSON.stringify(next)) {
      current = next;
      listeners.forEach((listener) => listener());
    }
  } catch {
    // A command failure is reported by the operation that initiated the load.
  } finally {
    if (version === generation && listeners.size)
      timer = setTimeout(() => void refresh(version), 250);
  }
}
function subscribe(listener: () => void) {
  if (!desktop) return () => {};
  listeners.add(listener);
  if (listeners.size === 1) void refresh(++generation);
  return () => {
    listeners.delete(listener);
    if (!listeners.size) {
      generation++;
      clearTimeout(timer);
      current = null;
    }
  };
}
const snapshot = () => current;
const empty = () => null;
export async function cancelCurrentLoad(kind: Kind) {
  // Capture the displayed ID now; an asynchronous refresh could target a later job.
  const progress = current;
  if (progress?.kind === kind && progress.cancellable)
    await invoke("cancel_load", { id: progress.id });
}
const phases: Record<Progress["phase"], Key> = {
  read: "loadRead",
  convert: "loadConvert",
  validate: "loadValidate",
  vertices: "loadVertices",
  surfaces: "loadSurfaces",
  instances: "loadInstances",
  commit: "loadCommit",
};
export function LoadProgress({
  t,
  kind,
}: {
  t: (key: Key) => string;
  kind?: Kind;
}) {
  const progress = useSyncExternalStore(subscribe, snapshot, empty);
  const [pending, setPending] = useState("");
  const [error, setError] = useState({ id: "", message: "" });
  if (!progress || (kind && progress.kind !== kind)) return null;
  const cancelling = progress.cancelling || pending === progress.id;
  const label = t(cancelling ? "loadCancelling" : phases[progress.phase]);
  return (
    <div className="load-progress" aria-busy="true">
      <span className="load-phase" role="status">
        {label}
      </span>
      <progress
        aria-label={label}
        aria-valuetext={
          progress.total
            ? `${progress.completed}/${progress.total} ${t(progress.phase === "instances" ? "objects" : "loadShapes")}`
            : undefined
        }
        max={progress.total || 1}
        value={progress.total ? progress.completed : undefined}
      />
      {progress.total > 0 && (
        <small
          title={t(progress.phase === "instances" ? "objects" : "loadShapes")}
        >
          {progress.completed}/{progress.total}
        </small>
      )}
      <IconButton
        icon={X}
        label={t("loadCancel")}
        disabled={!progress.cancellable || cancelling}
        onClick={() => {
          setPending(progress.id);
          setError({ id: progress.id, message: "" });
          void invoke("cancel_load", { id: progress.id })
            .catch((e) => setError({ id: progress.id, message: String(e) }))
            .finally(() =>
              setPending((previous) =>
                previous === progress.id ? "" : previous,
              ),
            );
        }}
      />
      {error.id === progress.id && error.message && (
        <span role="alert" className="usd-error">
          {error.message}
        </span>
      )}
    </div>
  );
}
