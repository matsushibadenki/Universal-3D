import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Monitor, Scan, RotateCcw, Magnet } from "lucide-react";
import { desktop } from "./store";
import { IconButton, InfoTip } from "./EditorChrome";
import type { PanelProps } from "./panels";
import type { Command } from "./types";

type Rect = {
  x: number;
  y: number;
  width: number;
  height: number;
  dpr: number;
  viewportHeight: number;
  visible: boolean;
};
// One native layout request in flight; scrolling/resizing keeps only the newest bounds.
let pendingLayout: {
  rect: Rect;
  waiters: { resolve: () => void; reject: (e: unknown) => void }[];
} | null = null;
let layingOut = false;
async function drainLayout() {
  if (layingOut) return;
  layingOut = true;
  while (pendingLayout) {
    const job = pendingLayout;
    pendingLayout = null;
    try {
      await invoke("set_viewport_rect", { rect: job.rect });
      job.waiters.forEach((w) => w.resolve());
    } catch (error) {
      job.waiters.forEach((w) => w.reject(error));
    }
  }
  layingOut = false;
}
function sendLayout(rect: Rect) {
  return new Promise<void>((resolve, reject) => {
    if (pendingLayout) {
      pendingLayout.rect = rect;
      pendingLayout.waiters.push({ resolve, reject });
    } else pendingLayout = { rect, waiters: [{ resolve, reject }] };
    void drainLayout();
  });
}

export function Viewport({ p }: { p: PanelProps }) {
  const canvas = useRef<HTMLDivElement>(null);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState("");
  const [attempt, setAttempt] = useState(0);
  const [dragging, setDragging] = useState(false);
  const [snapHeld, setSnapHeld] = useState(false);
  const snapKey = useRef(false);
  const snapKeyRef = useRef<(held: boolean) => void>(() => {});
  const settings = p.s.transform_settings;
  const tool = p.s.tool === "select" ? "move" : p.s.tool;
  const space = settings[`${tool}_space`];
  const step = settings[`${tool}_step`];
  const steps =
    tool === "rotate"
      ? [1, 5, 15, 30, 45, 90]
      : tool === "scale"
        ? [0.01, 0.05, 0.1, 0.25, 0.5]
        : [0.1, 0.25, 0.5, 1, 5, 10];
  const requestLayout = useRef<() => void>(() => {});
  const gesture = useRef<{
    id: number;
    x: number;
    y: number;
    token?: string;
    left: number;
    top: number;
    travel: number;
    mode?: "orbit" | "pan" | "dolly";
  } | null>(null);
  const activeToken = useRef<string | null>(null);
  const cancelRef = useRef<() => void>(() => {});
  const commands = useRef<Command[]>([]);
  const sending = useRef(false);
  const mounted = useRef(true);
  const embeddedPlatform = /Mac/.test(navigator.userAgent);

  async function drain() {
    if (sending.current) return;
    sending.current = true;
    while (commands.current.length) {
      const command = commands.current.shift()!;
      try {
        await invoke("execute", { command });
        if (
          command.type === "transform_end" &&
          activeToken.current === command.token
        )
          activeToken.current = null;
      } catch (e) {
        if (mounted.current) setError(String(e));
        commands.current = [];
        const token = activeToken.current;
        activeToken.current = null;
        if (token)
          await invoke("execute", {
            command: { type: "transform_end", token, cancel: true },
          }).catch(() => {});
        gesture.current = null;
        if (mounted.current) setDragging(false);
      }
    }
    sending.current = false;
  }
  function send(command: Command) {
    if (!ready) return;
    const last = commands.current.at(-1);
    if (
      command.type === "navigate" &&
      last?.type === "navigate" &&
      last.mode === command.mode &&
      last.height === command.height
    ) {
      last.dx = Number(last.dx) + Number(command.dx);
      last.dy = Number(last.dy) + Number(command.dy);
    } else if (
      command.type === "transform_update" &&
      last?.type === "transform_update" &&
      last.token === command.token &&
      last.snap_toggle === command.snap_toggle &&
      (last.points as number[][]).length +
        (command.points as number[][]).length <=
        256
    ) {
      // Rotations need every angular crossing; keep the path while batching IPC.
      (last.points as number[][]).push(...(command.points as number[][]));
    } else commands.current.push(command);
    void drain();
  }
  snapKeyRef.current = (held) => {
    if (snapKey.current === held) return;
    snapKey.current = held;
    setSnapHeld(held);
    const g = gesture.current;
    if (g?.token && g.travel >= 4)
      send({
        type: "transform_update",
        token: g.token,
        points: [[g.x - g.left, g.y - g.top]],
        snap_toggle: held,
      });
  };
  function cancelGesture() {
    const token = activeToken.current;
    if (token) send({ type: "transform_end", token, cancel: true });
    gesture.current = null;
    setDragging(false);
  }
  cancelRef.current = cancelGesture;
  function frameSelection() {
    const bounds = canvas.current?.getBoundingClientRect();
    if (bounds && bounds.height > 0)
      send({ type: "frame_selection", aspect: bounds.width / bounds.height });
  }
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      const token = activeToken.current;
      commands.current = token
        ? [{ type: "transform_end", token, cancel: true }]
        : [];
      void drain();
    };
  }, []);
  useEffect(() => {
    const cancel = () => {
      snapKey.current = false;
      setSnapHeld(false);
      cancelRef.current();
    };
    const keyDown = (e: KeyboardEvent) => {
      if (
        e.key.toLowerCase() !== "j" ||
        e.repeat ||
        e.isComposing ||
        e.altKey ||
        e.ctrlKey ||
        e.metaKey ||
        (e.target as HTMLElement).closest(
          "input,select,textarea,[contenteditable=true]",
        )
      )
        return;
      e.preventDefault();
      snapKeyRef.current(true);
    };
    const keyUp = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() === "j") snapKeyRef.current(false);
    };
    const hidden = () => {
      if (document.hidden) cancel();
    };
    window.addEventListener("keydown", keyDown);
    window.addEventListener("keyup", keyUp);
    window.addEventListener("blur", cancel);
    document.addEventListener("visibilitychange", hidden);
    return () => {
      window.removeEventListener("keydown", keyDown);
      window.removeEventListener("keyup", keyUp);
      window.removeEventListener("blur", cancel);
      document.removeEventListener("visibilitychange", hidden);
    };
  }, []);
  useEffect(() => {
    if (!desktop || !embeddedPlatform) return;
    let disposed = false;
    let last = "";
    const node = canvas.current!;
    const schedule = () => {
      // Native windows also resize while WKWebView suspends animation frames in the background.
      // ResizeObserver already batches layout; the IPC queue coalesces overlapping requests.
      if (disposed) return;
      const r = node.getBoundingClientRect();
      const rect: Rect = {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
        dpr: devicePixelRatio,
        viewportHeight: innerHeight,
        visible: !document.hidden && r.width > 0 && r.height > 0,
      };
      const signature = JSON.stringify(rect);
      if (signature === last) return;
      if (last && activeToken.current) cancelRef.current();
      last = signature;
      void sendLayout(rect)
        .then(() => {
          if (disposed) return;
          document.documentElement.classList.add("gpu-embedded");
          setReady(true);
          setError("");
        })
        .catch((e) => {
          if (!disposed) {
            last = "";
            setError(String(e));
            setReady(false);
            document.documentElement.classList.remove("gpu-embedded");
          }
        });
    };
    requestLayout.current = schedule;
    const observer = new ResizeObserver(schedule);
    observer.observe(node);
    observer.observe(document.documentElement);
    window.addEventListener("resize", schedule);
    window.addEventListener("scroll", schedule, true);
    document.addEventListener("visibilitychange", schedule);
    window.visualViewport?.addEventListener("resize", schedule);
    window.visualViewport?.addEventListener("scroll", schedule);
    // OS DPI changes and panel-focus requests also invalidate the last CSS bounds.
    const listeners = [
      listen("viewport-layout-needed", () => {
        if (activeToken.current) cancelRef.current();
        last = "";
        requestLayout.current();
      }),
      listen("viewport-focus", () => {
        node.focus();
        requestLayout.current();
      }),
    ];
    schedule();
    return () => {
      disposed = true;
      observer.disconnect();
      window.removeEventListener("resize", schedule);
      window.removeEventListener("scroll", schedule, true);
      document.removeEventListener("visibilitychange", schedule);
      window.visualViewport?.removeEventListener("resize", schedule);
      window.visualViewport?.removeEventListener("scroll", schedule);
      listeners.forEach(
        (promise) =>
          void promise.then((unlisten) => unlisten()).catch(() => {}),
      );
      document.documentElement.classList.remove("gpu-embedded");
      void sendLayout({
        x: 0,
        y: 0,
        width: 0,
        height: 0,
        dpr: 1,
        viewportHeight: innerHeight,
        visible: false,
      }).catch(() => {});
    };
  }, [attempt, embeddedPlatform]);
  // Passive wheel handlers cannot prevent page scrolling while dollying the camera.
  useEffect(() => {
    const node = canvas.current!;
    const wheel = (e: WheelEvent) => {
      if (!ready) return;
      e.preventDefault();
      if (activeToken.current) return;
      node.focus();
      const unit =
        e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? node.clientHeight : 1;
      send({
        type: "navigate",
        mode: "dolly",
        dx: 0,
        dy: Math.max(-200, Math.min(200, e.deltaY * unit)) * 0.25,
        height: node.clientHeight,
      });
    };
    node.addEventListener("wheel", wheel, { passive: false });
    return () => node.removeEventListener("wheel", wheel);
  }, [ready]);
  useEffect(() => {
    requestLayout.current();
  });
  return (
    <>
      <div className="viewport-toolbar">
        <IconButton
          icon={Scan}
          label={`${p.t("frameSelected")} (F)`}
          disabled={!ready || p.s.document.selected === null}
          onClick={frameSelection}
        />
        <IconButton
          icon={RotateCcw}
          label={p.t("reset")}
          disabled={!ready}
          onClick={() => send({ type: "camera_reset" })}
        />
        <select
          className="transform-setting"
          aria-label={p.t("coordinateSpace")}
          title={p.t("coordinateSpace")}
          value={space}
          disabled={!ready || p.s.dragging}
          onChange={(e) =>
            void p.execute({ type: "set_space", space: e.target.value })
          }
        >
          <option value="world">{p.t("worldSpace")}</option>
          <option value="local">{p.t("localSpace")}</option>
        </select>
        <IconButton
          icon={Magnet}
          label={`${p.t("stepSnap")} (J)`}
          aria-pressed={settings.snap !== snapHeld}
          disabled={!ready || p.s.dragging}
          onClick={() =>
            void p.execute({ type: "set_snap", enabled: !settings.snap })
          }
        />
        <select
          className="transform-setting"
          aria-label={p.t("snapStep")}
          title={p.t("snapStep")}
          value={step}
          disabled={!ready || p.s.dragging}
          onChange={(e) =>
            void p.execute({
              type: "set_snap_step",
              tool,
              step: Number(e.target.value),
            })
          }
        >
          {(steps.includes(step)
            ? steps
            : [...steps, step].sort((a, b) => a - b)
          ).map((value) => (
            <option key={value} value={value}>
              {tool === "scale"
                ? `${Math.round(value * 100)}%`
                : tool === "rotate"
                  ? `${value}°`
                  : value}
            </option>
          ))}
        </select>
        <InfoTip label={p.t("snapInfo")} text={p.t("snapHelp")} />
        <span>{p.t(ready ? "embedded" : "notConnected")}</span>
        <InfoTip label={p.t("info")} text={p.t("viewportHelp")} />
      </div>
      <div
        className={`gpu-canvas ${ready ? "ready" : ""} ${dragging ? "dragging" : ""}`}
        ref={canvas}
        tabIndex={0}
        role="region"
        aria-label={p.t("viewport")}
        aria-keyshortcuts="Q W E R J F Escape Shift+Space"
        onContextMenu={(e) => {
          if (ready) e.preventDefault();
        }}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            cancelGesture();
            return;
          }
          if (activeToken.current) return;
          if (
            e.key.toLowerCase() === "f" &&
            !e.altKey &&
            !e.metaKey &&
            !e.ctrlKey &&
            !e.nativeEvent.isComposing
          ) {
            e.preventDefault();
            frameSelection();
          }
        }}
        onPointerDown={(e) => {
          if (!ready || gesture.current || (!e.altKey && e.button !== 0))
            return;
          e.preventDefault();
          e.currentTarget.focus();
          e.currentTarget.setPointerCapture(e.pointerId);
          const mode = e.altKey
            ? e.button === 0
              ? "orbit"
              : e.button === 1
                ? "pan"
                : e.button === 2
                  ? "dolly"
                  : undefined
            : undefined;
          const bounds = e.currentTarget.getBoundingClientRect();
          const token =
            !mode && p.s.tool !== "select" ? crypto.randomUUID() : undefined;
          if (token) {
            activeToken.current = token;
            send({
              type: "transform_begin",
              token,
              x: e.clientX - bounds.left,
              y: e.clientY - bounds.top,
              width: bounds.width,
              height: bounds.height,
            });
          }
          gesture.current = {
            id: e.pointerId,
            x: e.clientX,
            y: e.clientY,
            token,
            left: bounds.left,
            top: bounds.top,
            travel: 0,
            mode,
          };
          setDragging(Boolean(mode || token));
        }}
        onPointerMove={(e) => {
          const g = gesture.current;
          if (!g || g.id !== e.pointerId) return;
          const dx = e.clientX - g.x,
            dy = e.clientY - g.y;
          g.x = e.clientX;
          g.y = e.clientY;
          g.travel += Math.hypot(dx, dy);
          if (g.token && g.travel >= 4 && (dx || dy))
            send({
              type: "transform_update",
              token: g.token,
              points: [[e.clientX - g.left, e.clientY - g.top]],
              snap_toggle: snapKey.current,
            });
          if (g.mode && (dx || dy))
            send({
              type: "navigate",
              mode: g.mode,
              dx,
              dy,
              height: e.currentTarget.clientHeight,
            });
        }}
        onPointerUp={(e) => {
          const g = gesture.current;
          if (!g || g.id !== e.pointerId) return;
          if (g.token) {
            if (g.travel >= 4)
              send({
                type: "transform_update",
                token: g.token,
                points: [[e.clientX - g.left, e.clientY - g.top]],
                snap_toggle: snapKey.current,
              });
            send({ type: "transform_end", token: g.token, cancel: false });
          }
          if (!g.token && !g.mode && g.travel < 4) {
            const r = e.currentTarget.getBoundingClientRect();
            send({
              type: "viewport_pick",
              x: ((e.clientX - r.left) / r.width) * 2 - 1,
              y: 1 - ((e.clientY - r.top) / r.height) * 2,
              aspect: r.width / r.height,
            });
          }
          gesture.current = null;
          setDragging(false);
          e.currentTarget.releasePointerCapture(e.pointerId);
        }}
        onLostPointerCapture={() => {
          if (gesture.current) cancelGesture();
        }}
        onPointerCancel={cancelGesture}
      >
        {!ready && (
          <div className="native-message">
            <Monitor size={42} strokeWidth={1} />
            <span>
              {p.t(
                !desktop
                  ? "browser"
                  : embeddedPlatform
                    ? "startingViewport"
                    : "platformViewport",
              )}
            </span>
            {desktop && (
              <button
                onClick={() => {
                  if (embeddedPlatform) setAttempt((v) => v + 1);
                  else void p.call("open_viewport");
                }}
              >
                {p.t(embeddedPlatform ? "retry" : "launch")}
              </button>
            )}
          </div>
        )}
        {error && (
          <div role="alert" className="viewport-error">
            {p.t("viewportError")}
            <InfoTip label={p.t("info")} text={error} />
          </div>
        )}
      </div>
    </>
  );
}
