import { useState, useEffect } from "react";
import {
  Box,
  Circle,
  Square,
  Undo2,
  Redo2,
  Save,
  FolderOpen,
  Monitor,
  RotateCcw,
} from "lucide-react";
import { useEditor, desktop } from "./store";
import { dictionaries, type Language, type Key } from "./i18n";
import { Scene, Inspector, Timeline, type PanelProps } from "./panels";
function Camera({ p }: { p: PanelProps }) {
  const [camera, setCamera] = useState({
    yaw: p.s.camera[0],
    pitch: p.s.camera[1],
    distance: p.s.camera[2],
  });
  useEffect(
    () =>
      setCamera({
        yaw: p.s.camera[0],
        pitch: p.s.camera[1],
        distance: p.s.camera[2],
      }),
    [p.s.camera[0], p.s.camera[1], p.s.camera[2]],
  );
  const fields = [
    ["yaw", "orbit", -3.14, 3.14],
    ["pitch", "elevation", -1.3, 1.3],
    ["distance", "distance", 1.5, 30],
  ] as const;
  return (
    <div className="camera-controls">
      {fields.map(([key, label, min, max]) => (
        <label key={key}>
          <span>{p.t(label)}</span>
          <input
            type="range"
            min={min}
            max={max}
            step="0.05"
            value={camera[key]}
            disabled={p.disabled}
            onChange={(e) => {
              const c = { ...camera, [key]: +e.target.value };
              setCamera(c);
            }}
            onPointerUp={() => void p.execute({ type: "camera", ...camera })}
            onKeyUp={() => void p.execute({ type: "camera", ...camera })}
          />
        </label>
      ))}
      <button
        disabled={p.disabled}
        onClick={() => {
          const c = { yaw: 0.7, pitch: 0.45, distance: 7 };
          setCamera(c);
          void p.execute({ type: "camera", ...c });
        }}
      >
        <RotateCcw size={14} />
        {p.t("reset")}
      </button>
    </div>
  );
}
export default function App() {
  const { snapshot: s, error, busy, execute, call } = useEditor();
  const [language, setLanguage] = useState<Language>(
    () => (localStorage.getItem("u3d-language") as Language) || "ja",
  );
  const dict = dictionaries[language] ?? dictionaries.ja;
  const t = (k: Key) => dict[k];
  const p: PanelProps = { s, t, execute, call, disabled: !desktop || busy };
  const standalone = new URLSearchParams(location.search).get("panel");
  useEffect(() => {
    document.documentElement.lang = language;
    localStorage.setItem("u3d-language", language);
  }, [language]);
  useEffect(() => {
    const f = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement).matches("input,select,textarea") || busy)
        return;
      if (e.metaKey || e.ctrlKey) {
        if (e.key.toLowerCase() === "z") {
          e.preventDefault();
          void execute({ type: e.shiftKey ? "redo" : "undo" });
        }
        if (e.key.toLowerCase() === "s") {
          e.preventDefault();
          void call("save_document");
        }
      }
    };
    window.addEventListener("keydown", f);
    return () => window.removeEventListener("keydown", f);
  }, [busy]);
  const lang = (
    <select
      aria-label="Language"
      value={language}
      onChange={(e) => setLanguage(e.target.value as Language)}
    >
      <option value="en">English</option>
      <option value="ja">日本語</option>
      <option value="zh">简体中文</option>
    </select>
  );
  if (standalone)
    return (
      <div className="standalone">
        <div className="floating-toolbar">{lang}</div>
        {standalone === "scene" ? (
          <Scene {...p} standalone />
        ) : standalone === "inspector" ? (
          <Inspector {...p} standalone />
        ) : (
          <Timeline {...p} standalone />
        )}
        {error && (
          <div role="alert" className="error">
            {error}
          </div>
        )}
      </div>
    );
  return (
    <div className="app">
      <header className="app-header">
        <div className="brand">
          <Box size={24} />
          <strong>Universal 3D</strong>
        </div>
        <span className="workspace-tab">{t("modeling")}</span>
        <div className="header-actions">
          {lang}
          <button
            disabled={p.disabled}
            onClick={() => void call("open_document")}
          >
            <FolderOpen size={16} />
            {t("open")}
          </button>
          <button
            className="primary"
            disabled={p.disabled}
            onClick={() => void call("save_document")}
          >
            <Save size={16} />
            {t("save")}
          </button>
        </div>
      </header>
      <div className="toolbar">
        {[
          [Box, "cube"],
          [Circle, "sphere"],
          [Square, "plane"],
        ].map(([Icon, key]) => {
          const I = Icon as typeof Box;
          return (
            <button
              key={String(key)}
              disabled={p.disabled}
              onClick={() => void execute({ type: "add", kind: key })}
            >
              <I size={19} />
              {t(key as Key)}
            </button>
          );
        })}
        <div className="separator" />
        <button
          disabled={p.disabled || !s.can_undo}
          onClick={() => void execute({ type: "undo" })}
        >
          <Undo2 size={18} />
          {t("undo")}
        </button>
        <button
          disabled={p.disabled || !s.can_redo}
          onClick={() => void execute({ type: "redo" })}
        >
          <Redo2 size={18} />
          {t("redo")}
        </button>
        <span className="document-title">
          {s.dirty ? "● " : ""}
          {s.filename}
        </span>
      </div>
      {!desktop && <div className="preview-notice">{t("browser")}</div>}
      {error && (
        <div className="error" role="alert">
          {error}
        </div>
      )}
      <main
        className={`workspace ${s.floating.includes("scene") ? "scene-floating" : ""} ${s.floating.includes("inspector") ? "inspector-floating" : ""}`}
      >
        {!s.floating.includes("scene") && <Scene {...p} />}
        <section className="panel viewport">
          <header className="panel-heading">
            <Monitor size={16} />
            <h2>{t("viewport")}</h2>
            <span className="muted">wgpu</span>
          </header>
          <div className="viewport-body">
            <div className="native-message">
              <Monitor size={42} strokeWidth={1} />
              <h1>{t("native")}</h1>
              <p>{t("description")}</p>
              <button
                className="primary"
                disabled={p.disabled}
                onClick={() => void call("open_viewport")}
              >
                <ExternalIcon />
                {t("launch")}
              </button>
              <small>{t(s.viewport ? "connected" : "notConnected")}</small>
            </div>
            <Camera p={p} />
          </div>
        </section>
        {!s.floating.includes("inspector") && <Inspector {...p} />}
      </main>
      {!s.floating.includes("timeline") && <Timeline {...p} />}
      <footer className="statusbar">
        <span>{t("ready")}</span>
        <span>Universal 3D · 0.1.0</span>
      </footer>
    </div>
  );
}
function ExternalIcon() {
  return <Monitor size={16} />;
}
