import { useEffect, useState, useRef, type ReactNode } from "react";
import {
  Box,
  Circle,
  Square,
  Eye,
  EyeOff,
  ExternalLink,
  PanelLeftClose,
  SlidersHorizontal,
  Layers,
  Film,
  Trash2,
  Search,
  ChevronRight,
  SkipBack,
  SkipForward,
  ChevronLeft,
} from "lucide-react";
import { IconButton, InfoTip } from "./EditorChrome";
import { desktop } from "./store";
import type { Snapshot, SceneObject, Command } from "./types";
import type { Key } from "./i18n";
export type PanelProps = {
  s: Snapshot;
  t: (k: Key) => string;
  execute: (c: Command) => Promise<void>;
  call: (n: string, a?: Record<string, unknown>) => Promise<void>;
  disabled: boolean;
  standalone?: boolean;
};
const icons: Record<string, typeof Box> = {
  cube: Box,
  sphere: Circle,
  plane: Square,
};
export function Panel({
  id,
  title,
  icon: Icon,
  children,
  p,
}: {
  id: string;
  title: string;
  icon: typeof Box;
  children: ReactNode;
  p: PanelProps;
}) {
  return (
    <section className={`panel ${id}`}>
      <header className="panel-heading">
        <Icon size={16} />
        <h2>{title}</h2>
        <button
          className="icon-button"
          title={p.t(p.standalone ? "dock" : "float")}
          aria-label={p.t(p.standalone ? "dock" : "float")}
          disabled={!desktop || p.disabled}
          onClick={() =>
            void p.call(p.standalone ? "dock_panel" : "float_panel", {
              panel: id,
            })
          }
        >
          {p.standalone ? (
            <PanelLeftClose size={15} />
          ) : (
            <ExternalLink size={15} />
          )}
        </button>
      </header>
      {children}
    </section>
  );
}
export function Scene(p: PanelProps) {
  const [query, setQuery] = useState("");
  const objects = p.s.document.objects.filter((o) =>
    o.name.toLocaleLowerCase().includes(query.toLocaleLowerCase()),
  );
  return (
    <Panel id="scene" title={p.t("scene")} icon={Layers} p={p}>
      <label className="scene-search">
        <Search size={14} />
        <input
          type="search"
          placeholder={p.t("search")}
          aria-label={p.t("search")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
      </label>
      <div className="scene-list">
        {objects.map((o) => {
          const Icon = icons[o.kind] ?? Box;
          return (
            <div
              key={o.id}
              className={`scene-row ${o.id === p.s.document.selected ? "selected" : ""}`}
            >
              <button
                disabled={p.disabled}
                aria-pressed={o.id === p.s.document.selected}
                onClick={() => void p.execute({ type: "select", id: o.id })}
              >
                <ChevronRight size={11} className="row-branch" />
                <Icon size={15} />
                <span>{o.name}</span>
              </button>
              <button
                className="icon-button"
                disabled={p.disabled}
                title={p.t("visible")}
                aria-label={`${p.t("visible")}: ${o.name}`}
                onClick={() =>
                  void p.execute({
                    type: "update",
                    object: { ...o, visible: !o.visible },
                  })
                }
              >
                {o.visible ? <Eye size={15} /> : <EyeOff size={15} />}
              </button>
            </div>
          );
        })}
        {!objects.length && (
          <p className="muted inset">
            {p.t(query ? "noResults" : "noObjects")}
          </p>
        )}
      </div>
      <div className="panel-foot">
        {objects.length} / {p.s.document.objects.length} {p.t("objects")}
      </div>
    </Panel>
  );
}
function NumberField({
  value,
  label,
  commit,
  disabled,
}: {
  value: number;
  label: string;
  commit: (n: number) => void;
  disabled: boolean;
}) {
  const [draft, setDraft] = useState(String(value));
  const cancel = useRef(false);
  return (
    <input
      aria-label={label}
      type="number"
      value={draft}
      disabled={disabled}
      step="0.1"
      onChange={(e) => setDraft(e.target.value)}
      onBlur={() => {
        const n = Number(draft);
        setDraft(String(value));
        if (
          !cancel.current &&
          draft.trim() !== "" &&
          Number.isFinite(n) &&
          n !== value
        )
          commit(n);
        cancel.current = false;
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") e.currentTarget.blur();
        if (e.key === "Escape") {
          cancel.current = true;
          setDraft(String(value));
          e.currentTarget.blur();
        }
      }}
    />
  );
}
export function Inspector(p: PanelProps) {
  const o = p.s.document.objects.find((o) => o.id === p.s.document.selected);
  const update = (next: SceneObject) =>
    void p.execute({ type: "update", object: next });
  return (
    <Panel
      id="inspector"
      title={p.t("inspector")}
      icon={SlidersHorizontal}
      p={p}
    >
      {o ? (
        <div className="inspector-body">
          <div className="object-name">
            <Box size={16} />
            <input
              key={`${o.id}-${o.name}`}
              aria-label={p.t("name")}
              defaultValue={o.name}
              disabled={p.disabled}
              maxLength={256}
              onBlur={(e) => {
                if (e.target.value !== o.name)
                  update({ ...o, name: e.target.value });
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
              }}
            />
          </div>
          <label className="check">
            <input
              type="checkbox"
              checked={o.visible}
              disabled={p.disabled}
              onChange={(e) => update({ ...o, visible: e.target.checked })}
            />
            {p.t("visible")}
          </label>
          <div className="section-heading">{p.t("transform")}</div>
          {(["position", "rotation", "scale"] as const).map((field) => (
            <div className="transform" key={field} data-channel={field}>
              <div className="axis-fields">
                {["X", "Y", "Z"].map((axis, i) => (
                  <label key={axis}>
                    <span>
                      {p.t(field)}{" "}
                      <b className={`axis-${axis.toLowerCase()}`}>{axis}</b>
                    </span>
                    <NumberField
                      key={`${o.id}-${field}-${i}-${o[field][i]}`}
                      label={`${p.t(field)} ${axis}`}
                      value={o[field][i]}
                      disabled={p.disabled}
                      commit={(n) => {
                        const v = [...o[field]];
                        v[i] = n;
                        update({ ...o, [field]: v });
                      }}
                    />
                  </label>
                ))}
              </div>
            </div>
          ))}
          <div className="section-heading">{p.t("modifiers")}</div>
          <div className="modifier">
            <Box size={17} />
            <span>
              {p.t(
                o.kind === "sphere"
                  ? "sphereLabel"
                  : o.kind === "plane"
                    ? "planeLabel"
                    : "cubeLabel",
              )}
            </span>
          </div>
          <label className="check">
            <input
              type="checkbox"
              checked={o.mirror}
              disabled={p.disabled}
              onChange={(e) => update({ ...o, mirror: e.target.checked })}
            />
            {p.t("mirror")}
          </label>
          <button
            className="delete-button"
            disabled={p.disabled}
            onClick={() => void p.execute({ type: "delete" })}
          >
            <Trash2 size={15} />
            {p.t("delete")}
          </button>
        </div>
      ) : (
        <p className="muted inset">{p.t("empty")}</p>
      )}
    </Panel>
  );
}
export function Timeline(p: PanelProps) {
  const [frame, setDraftFrame] = useState(p.s.frame);
  useEffect(() => setDraftFrame(p.s.frame), [p.s.frame]);
  const setFrame = (frame: number) =>
    void p.execute({ type: "set_frame", frame });
  return (
    <Panel id="timeline" title={p.t("timeline")} icon={Film} p={p}>
      <div className="timeline-ruler">
        {[1, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120].map((n) => (
          <span key={n} style={{ left: `${((n - 1) / 119) * 100}%` }}>
            {n}
          </span>
        ))}
      </div>
      <div className="track">
        <input
          aria-label={p.t("frame")}
          type="range"
          disabled={p.disabled}
          min="1"
          max="120"
          value={frame}
          onChange={(e) => setDraftFrame(+e.target.value)}
          onPointerUp={() => setFrame(frame)}
          onKeyUp={() => setFrame(frame)}
        />
      </div>
      <div className="timeline-footer">
        <div className="frame-controls">
          <IconButton
            icon={SkipBack}
            label={p.t("firstFrame")}
            disabled={p.disabled || p.s.frame === 1}
            onClick={() => setFrame(1)}
          />
          <IconButton
            icon={ChevronLeft}
            label={p.t("previousFrame")}
            disabled={p.disabled || p.s.frame === 1}
            onClick={() => setFrame(p.s.frame - 1)}
          />
          <label>
            <span>{p.t("frame")}</span>
            <NumberField
              key={p.s.frame}
              label={p.t("frame")}
              value={p.s.frame}
              disabled={p.disabled}
              commit={(n) =>
                setFrame(Math.max(1, Math.min(120, Math.round(n))))
              }
            />
          </label>
          <IconButton
            icon={ChevronRight}
            label={p.t("nextFrame")}
            disabled={p.disabled || p.s.frame === 120}
            onClick={() => setFrame(p.s.frame + 1)}
          />
          <IconButton
            icon={SkipForward}
            label={p.t("lastFrame")}
            disabled={p.disabled || p.s.frame === 120}
            onClick={() => setFrame(120)}
          />
        </div>
        <div className="frame-range">
          <span>1 — 120</span>
          <InfoTip label={p.t("info")} text={p.t("timelineNote")} />
        </div>
      </div>
    </Panel>
  );
}
