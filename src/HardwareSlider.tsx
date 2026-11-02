import type { CSSProperties, InputHTMLAttributes } from "react";

type HardwareSliderProps = Omit<
  InputHTMLAttributes<HTMLInputElement>,
  "type" | "min" | "max" | "value" | "className" | "style"
> & {
  min: number;
  max: number;
  value: number;
  ticks?: "frames";
};

// Decorative marks follow the thumb's travel, not the input's outer edges.
const controlTicks = Array.from({ length: 41 }, (_, i) => (
  <line key={i} x1={i * 10} x2={i * 10} y1="0" y2={i % 5 === 0 ? 7 : 4} />
));
const frameTicks = Array.from({ length: 120 }, (_, i) => (
  <line
    key={i}
    x1={(i / 119) * 400}
    x2={(i / 119) * 400}
    y1="0"
    y2={i === 0 || (i + 1) % 10 === 0 ? 7 : 4}
  />
));

export function HardwareSlider({
  min,
  max,
  value,
  ticks,
  ...input
}: HardwareSliderProps) {
  const progress =
    max > min ? Math.max(0, Math.min(1, (value - min) / (max - min))) : 0;
  return (
    <span
      className="hardware-slider"
      data-disabled={input.disabled || undefined}
      style={{ "--slider-progress": `${progress * 100}%` } as CSSProperties}
    >
      <svg
        className="hardware-slider-ticks"
        viewBox="0 0 400 7"
        preserveAspectRatio="none"
        aria-hidden="true"
        focusable="false"
      >
        {ticks === "frames" ? frameTicks : controlTicks}
      </svg>
      <input {...input} type="range" min={min} max={max} value={value} />
    </span>
  );
}
