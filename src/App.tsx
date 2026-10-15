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
  FlipHorizontal2,
  Trash2,
  MousePointer2,
  Move,
  Rotate3D,
  Scaling,
  PanelLeft,
  PanelRight,
  Film,
  Maximize2,
  Minimize2,
} from "lucide-react";
import { useEditor, desktop } from "./store";
import { dictionaries, type Language, type Key } from "./i18n";
import { Scene, Inspector, Timeline, type PanelProps } from "./panels";
import { IconButton, InfoTip, Menu } from "./EditorChrome";
import { Viewport } from "./Viewport";
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
    ["yaw", "orbit", 0, 6.2832],
    ["pitch", "elevation", -1.4, 1.4],
    ["distance", "distance", 1.5, 30],
  ] as const;
  return (
    <div className="camera-controls">
      {fields.map(([key, label, min, max]) => (
        <label key={key}>
          <span>{p.t(label)}</span>
          <input
            type="range"
            min={Math.min(min, camera[key])}
            max={Math.max(max, camera[key])}
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
        title={p.t("reset")}
        aria-label={p.t("reset")}
        disabled={p.disabled}
        onClick={() => {
          const c = { yaw: 0.7, pitch: 0.45, distance: 7 };
          setCamera(c);
          void p.execute({ type: "camera_reset" });
        }}
      >
        <RotateCcw size={14} />
      </button>
    </div>
  );
}
export default function App() {
  const { snapshot: s, error, busy, execute, call } = useEditor();
  const [language, setLanguage] = useState<Language>(() => {
    const saved = localStorage.getItem("u3d-language");
    return saved === "en" || saved === "zh" ? saved : "ja";
  });
  const [shelf, setShelf] = useState<"polygon" | "view">("polygon");
  const [hidden, setHidden] = useState<string[]>([]);
  const [focused, setFocused] = useState(false);
  const t = (k: Key) => dictionaries[language][k];
  const p: PanelProps = { s, t, execute, call, disabled: !desktop || busy };
  const standalone = new URLSearchParams(location.search).get("panel");
  const selected = s.document.objects.find((o) => o.id === s.document.selected);
  const toggle = (panel: string) =>
    setHidden((value) =>
      value.includes(panel)
        ? value.filter((v) => v !== panel)
        : [...value, panel],
    );
  const shown = (panel: string) =>
    !focused && !hidden.includes(panel) && !s.floating.includes(panel);
  function focusChannel(field?: string) {
    setFocused(false);
    const panel = field ? "inspector" : "scene";
    setHidden((value) => value.filter((v) => v !== panel));
    if (s.floating.includes(panel)) {
      void call("float_panel", { panel });
      return;
    }
    requestAnimationFrame(() => {
      document
        .querySelector<HTMLInputElement>(
          field ? `[data-channel="${field}"] input` : ".scene-search input",
        )
        ?.focus();
    });
  }
  useEffect(() => {
    document.documentElement.lang = language;
    localStorage.setItem("u3d-language", language);
  }, [language]);
  useEffect(() => {
    const f = (e: KeyboardEvent) => {
      if (
        e.isComposing ||
        (e.target as HTMLElement).closest(
          "input,select,textarea,[contenteditable=true]",
        )
      )
        return;
      if (e.shiftKey && e.code === "Space" && !standalone) {
        e.preventDefault();
        setFocused((v) => !v);
        return;
      }
      if (!desktop || busy) return;
      if (e.metaKey || e.ctrlKey) {
        const key = e.key.toLowerCase();
        if (key === "z") {
          e.preventDefault();
          void execute({ type: e.shiftKey ? "redo" : "undo" });
        }
        if (key === "s") {
          e.preventDefault();
          void call("save_document");
        }
        if (key === "o") {
          e.preventDefault();
          void call("open_document");
        }
      }
    };
    window.addEventListener("keydown", f);
    return () => window.removeEventListener("keydown", f);
  }, [busy, execute, call, standalone]);
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
  const primitives = [
    { icon: Box, kind: "cube", label: "cubeLabel" },
    { icon: Circle, kind: "sphere", label: "sphereLabel" },
    { icon: Square, kind: "plane", label: "planeLabel" },
  ] as const;
  const layoutControls = (
    <>
      <IconButton
        icon={PanelLeft}
        label={t("toggleScene")}
        aria-pressed={shown("scene")}
        disabled={s.floating.includes("scene")}
        onClick={() => {
          setFocused(false);
          toggle("scene");
        }}
      />
      <IconButton
        icon={PanelRight}
        label={t("toggleInspector")}
        aria-pressed={shown("inspector")}
        disabled={s.floating.includes("inspector")}
        onClick={() => {
          setFocused(false);
          toggle("inspector");
        }}
      />
      <IconButton
        icon={Film}
        label={t("toggleTimeline")}
        aria-pressed={shown("timeline")}
        disabled={s.floating.includes("timeline")}
        onClick={() => {
          setFocused(false);
          toggle("timeline");
        }}
      />
    </>
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
    <div className="app" aria-busy={busy}>
      <header className="app-header">
        <strong className="brand">Universal 3D</strong>
        <nav aria-label={t("modeling")}>
          <Menu label={t("file")}>
            <button
              disabled={p.disabled}
              onClick={() => void call("open_document")}
            >
              <FolderOpen size={15} />
              {t("open")}
              <kbd>⌘/Ctrl O</kbd>
            </button>
            <button
              disabled={p.disabled}
              onClick={() => void call("save_document")}
            >
              <Save size={15} />
              {t("save")}
              <kbd>⌘/Ctrl S</kbd>
            </button>
          </Menu>
          <Menu label={t("edit")}>
            <button
              disabled={p.disabled || !s.can_undo}
              onClick={() => void execute({ type: "undo" })}
            >
              <Undo2 size={15} />
              {t("undo")}
            </button>
            <button
              disabled={p.disabled || !s.can_redo}
              onClick={() => void execute({ type: "redo" })}
            >
              <Redo2 size={15} />
              {t("redo")}
            </button>
            <button
              disabled={p.disabled || !selected}
              onClick={() => void execute({ type: "delete" })}
            >
              <Trash2 size={15} />
              {t("delete")}
            </button>
          </Menu>
          <Menu label={t("create")}>
            {primitives.map(({ icon: Icon, kind }) => (
              <button
                key={kind}
                disabled={p.disabled}
                onClick={() => void execute({ type: "add", kind })}
              >
                <Icon size={15} />
                {t(kind)}
              </button>
            ))}
          </Menu>
          <Menu label={t("window")}>
            {(["scene", "inspector", "timeline"] as const).map((panel) => (
              <button
                key={panel}
                onClick={() => {
                  if (s.floating.includes(panel))
                    void call("float_panel", { panel });
                  else {
                    setFocused(false);
                    toggle(panel);
                  }
                }}
              >
                {t(panel)}
                <span className="menu-state">{shown(panel) ? "✓" : ""}</span>
              </button>
            ))}
            <button onClick={() => setFocused((v) => !v)}>
              {t("maximize")}
              <kbd>⇧ Space</kbd>
            </button>
            <button
              disabled={p.disabled}
              onClick={() => void call("open_viewport")}
            >
              {t("launch")}
            </button>
          </Menu>
        </nav>
      </header>
      <div className="status-line">
        <span className="menu-set">{t("modeling")}</span>
        <div className="separator" />
        <IconButton
          icon={FolderOpen}
          label={t("open")}
          disabled={p.disabled}
          onClick={() => void call("open_document")}
        />
        <IconButton
          icon={Save}
          label={t("save")}
          disabled={p.disabled}
          onClick={() => void call("save_document")}
        />
        <div className="separator" />
        <IconButton
          icon={Undo2}
          label={t("undo")}
          disabled={p.disabled || !s.can_undo}
          onClick={() => void execute({ type: "undo" })}
        />
        <IconButton
          icon={Redo2}
          label={t("redo")}
          disabled={p.disabled || !s.can_redo}
          onClick={() => void execute({ type: "redo" })}
        />
        <span className="document-title" title={s.filename}>
          {s.dirty ? "● " : ""}
          {s.filename}
        </span>
        <div className="workspace-controls">
          {lang}
          <label>
            <span>{t("workspace")}</span>
            <select
              aria-label={t("workspace")}
              value={focused ? "focus" : "standard"}
              onChange={(e) => {
                setFocused(e.target.value === "focus");
                if (e.target.value === "standard") setHidden([]);
              }}
            >
              <option value="standard">{t("standard")}</option>
              <option value="focus">{t("focusView")}</option>
            </select>
          </label>
          {layoutControls}
        </div>
      </div>
      <section className="shelf" aria-label={t("create")}>
        <div className="shelf-tabs">
          <button
            aria-pressed={shelf === "polygon"}
            onClick={() => setShelf("polygon")}
          >
            {t("polygon")}
          </button>
          <button
            aria-pressed={shelf === "view"}
            onClick={() => setShelf("view")}
          >
            {t("view")}
          </button>
        </div>
        <div className="shelf-tools">
          {shelf === "polygon" ? (
            <>
              {primitives.map(({ icon: Icon, kind, label }) => (
                <button
                  className="shelf-button primitive"
                  key={kind}
                  title={t(kind)}
                  aria-label={t(kind)}
                  disabled={p.disabled}
                  onClick={() => void execute({ type: "add", kind })}
                >
                  <Icon size={25} strokeWidth={1.6} />
                  <span>{t(label)}</span>
                </button>
              ))}
              <div className="separator" />
              <button
                className="shelf-button"
                aria-pressed={selected?.mirror ?? false}
                title={t("mirror")}
                disabled={p.disabled || !selected}
                onClick={() =>
                  selected &&
                  void execute({
                    type: "update",
                    object: { ...selected, mirror: !selected.mirror },
                  })
                }
              >
                <FlipHorizontal2 size={25} strokeWidth={1.6} />
                <span>{t("mirrorLabel")}</span>
              </button>
              <button
                className="shelf-button danger"
                title={t("delete")}
                disabled={p.disabled || !selected}
                onClick={() => void execute({ type: "delete" })}
              >
                <Trash2 size={23} strokeWidth={1.6} />
                <span>{t("deleteLabel")}</span>
              </button>
            </>
          ) : (
            <>
              <IconButton
                icon={Monitor}
                label={t("launch")}
                disabled={p.disabled}
                onClick={() => void call("open_viewport")}
              />
              {layoutControls}
              <IconButton
                icon={RotateCcw}
                label={t("reset")}
                disabled={p.disabled}
                onClick={() =>
                  void execute({
                    type: "camera_reset",
                  })
                }
              />
            </>
          )}
        </div>
      </section>
      {error && (
        <div className="error" role="alert">
          {error}
        </div>
      )}
      <main
        className={`workspace ${!shown("scene") ? "scene-floating" : ""} ${!shown("inspector") ? "inspector-floating" : ""}`}
      >
        <aside className="toolbox" aria-label={t("transform")}>
          <IconButton
            icon={MousePointer2}
            label={t("selectObject")}
            onClick={() => focusChannel()}
          />
          <IconButton
            icon={Move}
            label={t("editPosition")}
            disabled={p.disabled || !selected}
            onClick={() => focusChannel("position")}
          />
          <IconButton
            icon={Rotate3D}
            label={t("editRotation")}
            disabled={p.disabled || !selected}
            onClick={() => focusChannel("rotation")}
          />
          <IconButton
            icon={Scaling}
            label={t("editScale")}
            disabled={p.disabled || !selected}
            onClick={() => focusChannel("scale")}
          />
          <div className="separator" />
          <IconButton
            icon={focused ? Minimize2 : Maximize2}
            label={t("maximize")}
            aria-pressed={focused}
            onClick={() => setFocused((v) => !v)}
          />
        </aside>
        <div className="workspace-panels">
          {shown("scene") && <Scene {...p} />}
          <section className="panel viewport">
            <header className="panel-heading">
              <h2>{t("viewport")}</h2>
              <span className="muted">{t("perspective")}</span>
              <IconButton
                icon={focused ? Minimize2 : Maximize2}
                label={t("maximize")}
                onClick={() => setFocused((v) => !v)}
              />
            </header>
            <Viewport p={p} />
            <Camera p={p} />
          </section>
          {shown("inspector") && <Inspector {...p} />}
        </div>
      </main>
      {shown("timeline") && <Timeline {...p} />}
      <footer className="statusbar">
        <span>
          {t(busy ? "busy" : desktop ? "ready" : "preview")}
          {!desktop && <InfoTip label={t("info")} text={t("browser")} />}
        </span>
        <span>
          {s.document.objects.length} {t("objects")}
          <span className="status-version">Universal 3D · 0.1.0</span>
        </span>
      </footer>
    </div>
  );
}
