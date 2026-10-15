import {
  useEffect,
  useId,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type ReactNode,
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
  return (
    <span
      className={`info-tip ${dismissed ? "dismissed" : ""}`}
      onMouseEnter={() => setDismissed(false)}
      onFocus={() => setDismissed(false)}
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
      <span id={id} role="tooltip">
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
