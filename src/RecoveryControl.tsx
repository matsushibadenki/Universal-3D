import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { History, ShieldCheck, TriangleAlert } from "lucide-react";
import { InfoTip } from "./EditorChrome";
import { desktop } from "./store";
import { LoadProgress, cancelCurrentLoad } from "./LoadProgress";
import type { Key } from "./i18n";

type RecoveryState = {
  candidates: { id: string; filename: string; saved_at: number }[];
  saved_at: number | null;
  error: string | null;
};
export function RecoveryControl({ t }: { t: (key: Key) => string }) {
  const [state, setState] = useState<RecoveryState>({
    candidates: [],
    saved_at: null,
    error: null,
  });
  const [open, setOpen] = useState(false);
  const [selected, setSelected] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const initialized = useRef(false);
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    if (!desktop) return;
    let disposed = false;
    let pending = false;
    async function refresh() {
      if (pending) return;
      pending = true;
      try {
        const next = await invoke<RecoveryState>("get_recovery_state");
        if (disposed) return;
        setState(next);
        if (!initialized.current) {
          initialized.current = true;
          if (next.candidates.length) {
            setSelected(next.candidates[0].id);
            setOpen(true);
          }
        }
      } catch (e) {
        if (!disposed)
          setState((previous) => ({ ...previous, error: String(e) }));
      } finally {
        pending = false;
      }
    }
    void refresh();
    const timer = window.setInterval(() => void refresh(), 5000);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, []);
  useEffect(() => {
    if (open) {
      dialog.current?.showModal();
      dialog.current
        ?.querySelector<HTMLElement>("select, .recovery-body button")
        ?.focus();
    } else dialog.current?.close();
  }, [open]);
  async function restore() {
    setBusy(true);
    setError("");
    try {
      await invoke("restore_recovery", { id: selected });
      setState(await invoke<RecoveryState>("get_recovery_state"));
      setOpen(false);
    } catch (e) {
      setError(
        String(e).includes("LOAD_CANCELLED") ||
          String(e).includes("USD_IMPORT_CANCELLED")
          ? t("loadCancelled")
          : String(e),
      );
    } finally {
      setBusy(false);
    }
  }
  if (!desktop) return null;
  const status = state.error
    ? t("recoveryFailed")
    : state.saved_at
      ? `${t("recoverySaved")} · ${new Date(state.saved_at * 1000).toLocaleTimeString()}`
      : t("recoveryWaiting");
  const candidate = state.candidates.find((item) => item.id === selected);
  const Icon = state.error
    ? TriangleAlert
    : state.candidates.length
      ? History
      : ShieldCheck;
  return (
    <>
      <button
        className={`recovery-status${state.error ? " has-error" : ""}`}
        title={status}
        aria-label={status}
        onClick={() => {
          setSelected(state.candidates[0]?.id ?? "");
          setOpen(true);
        }}
      >
        <Icon size={14} />
        {state.candidates.length > 0 && <span>{state.candidates.length}</span>}
      </button>
      <dialog
        ref={dialog}
        className="usd-dialog recovery-dialog"
        aria-labelledby="recovery-title"
        onKeyDown={(e) => e.stopPropagation()}
        onCancel={(e) => {
          e.preventDefault();
          if (!busy) setOpen(false);
          else
            void cancelCurrentLoad("recovery").catch((e) =>
              setError(String(e)),
            );
        }}
      >
        <header>
          <strong id="recovery-title">{t("recoveryTitle")}</strong>
          <InfoTip label={t("info")} text={t("recoveryHelp")} />
        </header>
        <div className="recovery-body">
          {state.candidates.length > 0 ? (
            <>
              <label htmlFor="recovery-file">{t("recoveryFile")}</label>
              <select
                id="recovery-file"
                value={selected}
                disabled={busy}
                onChange={(e) => setSelected(e.target.value)}
              >
                {state.candidates.map((candidate) => (
                  <option key={candidate.id} value={candidate.id}>
                    {candidate.filename} ·{" "}
                    {new Date(candidate.saved_at * 1000).toLocaleString()}
                  </option>
                ))}
              </select>
              {candidate && (
                <time
                  dateTime={new Date(candidate.saved_at * 1000).toISOString()}
                >
                  {new Date(candidate.saved_at * 1000).toLocaleString()}
                </time>
              )}
              <p>{t("recoveryCopy")}</p>
            </>
          ) : (
            <p>{status}</p>
          )}
          {busy && <LoadProgress t={t} kind="recovery" />}
          {(error || state.error) && (
            <p className="usd-error" role="alert">
              {error || state.error}
            </p>
          )}
          <div className="usd-actions">
            <button
              type="button"
              disabled={busy}
              onClick={() => setOpen(false)}
            >
              {t("recoveryLater")}
            </button>
            {state.candidates.length > 0 && (
              <button
                type="button"
                disabled={busy || !selected}
                onClick={() => void restore()}
              >
                {t(busy ? "busy" : "recoveryRestore")}
              </button>
            )}
          </div>
        </div>
      </dialog>
    </>
  );
}
