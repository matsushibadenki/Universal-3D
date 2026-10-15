import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Monitor, Scan, RotateCcw } from "lucide-react";
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
  const requestLayout = useRef<() => void>(() => {});
  const gesture = useRef<{
    id: number;
    x: number;
    y: number;
    startX: number;
    startY: number;
    travel: number;
    mode?: "orbit" | "pan" | "dolly";
  } | null>(null);
  const commands = useRef<Command[]>([]);
  const sending = useRef(false);
  const mounted = useRef(true);
  const embeddedPlatform = /Mac/.test(navigator.userAgent);

  async function drain() {
    if (sending.current) return;
    sending.current = true;
    while (mounted.current && commands.current.length) {
      const command = commands.current.shift()!;
      try {
        await invoke("execute", { command });
      } catch (e) {
        if (mounted.current) setError(String(e));
        commands.current = [];
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
    } else commands.current.push(command);
    void drain();
  }
  function frameSelection() {
    const bounds = canvas.current?.getBoundingClientRect();
    if (bounds && bounds.height > 0)
      send({ type: "frame_selection", aspect: bounds.width / bounds.height });
  }
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      commands.current = [];
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
        <span>{p.t(ready ? "embedded" : "notConnected")}</span>
        <InfoTip label={p.t("info")} text={p.t("viewportHelp")} />
      </div>
      <div
        className={`gpu-canvas ${ready ? "ready" : ""} ${dragging ? "dragging" : ""}`}
        ref={canvas}
        tabIndex={0}
        role="region"
        aria-label={p.t("viewport")}
        aria-keyshortcuts="F Shift+Space"
        onContextMenu={(e) => {
          if (ready) e.preventDefault();
        }}
        onKeyDown={(e) => {
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
          if (!ready || (!e.altKey && e.button !== 0)) return;
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
          gesture.current = {
            id: e.pointerId,
            x: e.clientX,
            y: e.clientY,
            startX: e.clientX,
            startY: e.clientY,
            travel: 0,
            mode,
          };
          setDragging(Boolean(mode));
        }}
        onPointerMove={(e) => {
          const g = gesture.current;
          if (!g || g.id !== e.pointerId) return;
          const dx = e.clientX - g.x,
            dy = e.clientY - g.y;
          g.x = e.clientX;
          g.y = e.clientY;
          g.travel += Math.hypot(dx, dy);
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
          if (!g.mode && g.travel < 4) {
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
          gesture.current = null;
          setDragging(false);
        }}
        onPointerCancel={() => {
          gesture.current = null;
          setDragging(false);
        }}
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
