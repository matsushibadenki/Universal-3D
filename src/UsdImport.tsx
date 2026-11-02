import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Download, ExternalLink, FolderOpen, X } from "lucide-react";
import { IconButton, InfoTip } from "./EditorChrome";
import type { Key } from "./i18n";

type Report = {
  objects: number;
  triangles: number;
  source: { origin: string; revision: string; license: string };
  warnings: string[];
};
export function UsdImport({
  t,
  close,
}: {
  t: (key: Key) => string;
  close: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [report, setReport] = useState<Report | null>(null);
  useEffect(() => {
    dialog.current?.showModal();
    return () => dialog.current?.close();
  }, []);
  async function importAsset(source: string | null) {
    setBusy(true);
    setError("");
    setReport(null);
    try {
      setReport(await invoke<Report | null>("import_usd", { source }));
    } catch (e) {
      const message = String(e);
      setError(
        message.includes("USD_RUNTIME_MISSING")
          ? t("usdRuntime")
          : message.includes("USD_IMPORT_CANCELLED")
            ? t("usdCancelled")
            : `${t("usdFailed")}\n${message}`,
      );
    } finally {
      setBusy(false);
    }
  }
  async function cancel() {
    try {
      await invoke("cancel_usd_import");
    } catch (e) {
      setError(String(e));
    }
  }
  return (
    <dialog
      ref={dialog}
      className="usd-dialog"
      onKeyDown={(e) => e.stopPropagation()}
      onCancel={(e) => {
        e.preventDefault();
        if (busy) void cancel();
        else close();
      }}
    >
      <header>
        <strong>{t("usdImport")}</strong>
        <InfoTip label={t("info")} text={t("usdHelp")} />
        <IconButton
          icon={X}
          label={t("usdClose")}
          disabled={busy}
          onClick={close}
        />
      </header>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (!busy) void importAsset(url.trim());
        }}
        aria-busy={busy}
      >
        <label htmlFor="usd-url">{t("usdUrl")}</label>
        <input
          id="usd-url"
          type="url"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          disabled={busy}
          placeholder="https://huggingface.co/datasets/…/com.nvidia.simready.packaging.json"
          required
        />
        <div className="usd-actions">
          <button
            type="button"
            disabled={busy}
            onClick={() => void importAsset(null)}
          >
            <FolderOpen size={15} />
            {t("usdLocal")}
          </button>
          <button type="submit" disabled={busy || !url.trim()}>
            <Download size={15} />
            {t("usdDownload")}
          </button>
          <button
            type="button"
            onClick={() =>
              void invoke("browse_simready").catch((e) => setError(String(e)))
            }
          >
            <ExternalLink size={15} />
            SimReady Central
          </button>
        </div>
        {busy && (
          <div className="usd-actions" role="status">
            <span>{t("usdLoading")}</span>
            <button type="button" onClick={() => void cancel()}>
              {t("usdCancel")}
            </button>
          </div>
        )}
        {error && (
          <p className="usd-error" role="alert">
            {error}
          </p>
        )}
        {report && (
          <div className="usd-result" role="status">
            <strong>{t("usdComplete")}</strong>
            <p>
              {report.objects.toLocaleString()} {t("objects")} ·{" "}
              {report.triangles.toLocaleString()} {t("usdTriangles")}
            </p>
            <p>{report.source.license}</p>
            {report.source.revision && <code>{report.source.revision}</code>}
            <p>{t("usdLimitations")}</p>
            {report.warnings.includes("unsupported_geometry") && (
              <p>{t("usdUnsupported")}</p>
            )}
            <details>
              <summary>{t("usdDetails")}</summary>
              <p>{report.source.origin}</p>
              <code>{report.warnings.join(" · ")}</code>
            </details>
          </div>
        )}
      </form>
    </dialog>
  );
}
