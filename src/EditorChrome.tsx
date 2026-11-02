import {
  useEffect,
  useId,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type ReactNode,
  type CSSProperties,
} from "react";
import { Info, type LucideIcon } from "lucide-react";

export function IconButton({
  icon: Icon,
  label,
  className = "",
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  icon: LucideIcon;
  label: string;
}) {
  return (
    <button
      type="button"
      className={`icon-button ${className}`}
      title={label}
      aria-label={label}
      {...props}
    >
      <Icon size={17} strokeWidth={1.7} />
    </button>
  );
}

export function InfoTip({ text, label }: { text: string; label: string }) {
  const id = useId();
  const [dismissed, setDismissed] = useState(false);
  const root = useRef<HTMLSpanElement>(null);
  const tip = useRef<HTMLSpanElement>(null);
  const [position, setPosition] = useState<CSSProperties>();
  function positionTooltip() {
    const toolbar = root.current?.closest(".viewport-toolbar");
    if (!toolbar || !root.current || !tip.current) return;
    const anchor = root.current.getBoundingClientRect();
    const panel = toolbar.closest(".panel")?.getBoundingClientRect();
    const bounds = toolbar.getBoundingClientRect();
    const minLeft = Math.max(16, bounds.left + 8);
    const maxRight = Math.min(innerWidth - 16, bounds.right - 8);
    const width = Math.max(1, Math.min(270, maxRight - minLeft));
    const left = Math.max(
      minLeft,
      Math.min(anchor.right - width, maxRight - width),
    );
    setPosition({
      left: left - anchor.left,
      right: "auto",
      width,
      maxHeight: Math.max(
        60,
        Math.min(innerHeight - 16, panel?.bottom ?? innerHeight) -
          anchor.bottom -
          12,
      ),
      overflowY: "auto",
    });
  }
  useEffect(() => {
    window.addEventListener("resize", positionTooltip);
    return () => window.removeEventListener("resize", positionTooltip);
  }, []);
  return (
    <span
      ref={root}
      className={`info-tip ${dismissed ? "dismissed" : ""}`}
      onMouseEnter={() => {
        setDismissed(false);
        positionTooltip();
      }}
      onFocus={() => {
        setDismissed(false);
        positionTooltip();
      }}
    >
      <button
        type="button"
        className="icon-button"
        aria-label={label}
        aria-describedby={id}
        onKeyDown={(e) => {
          if (e.key === "Escape") setDismissed(true);
        }}
        onClick={() => setDismissed(false)}
      >
        <Info size={14} />
      </button>
      <span ref={tip} id={id} role="tooltip" style={position}>
        {text}
      </span>
    </span>
  );
}

// Disclosure navigation uses native buttons/Tab rather than claiming menu keyboard semantics.
export function Menu({
  label,
  children,
}: {
  label: string;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const id = useId();
  useEffect(() => {
    if (!open) return;
    const outside = (e: PointerEvent) => {
      if (!root.current?.contains(e.target as Node)) setOpen(false);
    };
    const escape = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setOpen(false);
        trigger.current?.focus();
      }
    };
    document.addEventListener("pointerdown", outside);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("pointerdown", outside);
      document.removeEventListener("keydown", escape);
    };
  }, [open]);
  return (
    <div
      className="menu"
      ref={root}
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget)) setOpen(false);
      }}
    >
      <button
        ref={trigger}
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        onClick={() => setOpen((v) => !v)}
      >
        {label}
      </button>
      {open && (
        <div
          className="menu-popup"
          id={id}
          onClick={(e) => {
            if ((e.target as Element).closest("button:not(:disabled)")) {
              setOpen(false);
              trigger.current?.focus();
            }
          }}
        >
          {children}
        </div>
      )}
    </div>
  );
}
