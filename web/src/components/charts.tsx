"use client";

/**
 * Daily space saved, as columns: one hue, values in text colours, a hover
 * readout, and a table for screen readers. Shown once there are a few days
 * of savings to compare.
 */

import { useId, useState } from "react";
import { formatBytes, formatCount, formatDay, percentOf, plural } from "@/lib/format";
import type { SavingsPoint } from "@/lib/types";
import { cn } from "@/lib/utils";

/** Round a byte maximum up to a clean axis value (1, 2, 5 × 10^n bytes). */
function niceMax(value: number): number {
  if (value <= 0) return 1;
  const exp = 10 ** Math.floor(Math.log10(value));
  const f = value / exp;
  const nice = f <= 1 ? 1 : f <= 2 ? 2 : f <= 5 ? 5 : 10;
  return nice * exp;
}

export function SavingsChart({ points, className }: { points: SavingsPoint[]; className?: string }) {
  const tableId = useId();
  const [active, setActive] = useState<number | null>(null);
  const max = niceMax(Math.max(0, ...points.map((p) => p.saved_bytes)));
  const total = points.reduce((sum, p) => sum + Math.max(0, p.saved_bytes), 0);
  const shown = active !== null ? points[active] : null;
  const hasData = points.some((p) => p.saved_bytes > 0);

  return (
    <figure className={cn("flex flex-col", className)} aria-labelledby={`${tableId}-caption`}>
      <figcaption id={`${tableId}-caption`} className="flex items-baseline justify-between gap-3">
        <span className="text-sm font-semibold text-fg">Saved per day</span>
        <span className="text-xs text-muted tabular" aria-live="off">
          {shown
            ? `${formatDay(shown.date)}: ${formatBytes(shown.saved_bytes)} · ${plural(shown.files, "file")}`
            : `Last 30 days · ${formatBytes(total)}`}
        </span>
      </figcaption>
      <div className="mt-4 grid flex-1 grid-cols-[auto_1fr] gap-x-3" aria-hidden>
        <div className="flex flex-col justify-between py-0 text-right text-xs text-muted tabular">
          <span className="-translate-y-1/2">{hasData ? formatBytes(max) : ""}</span>
          <span className="translate-y-1/2">0</span>
        </div>
        <div className="relative h-32" onMouseLeave={() => setActive(null)}>
          <div className="absolute inset-x-0 top-0 h-px bg-line" />
          <div className="absolute inset-x-0 top-1/2 h-px bg-line/60" />
          <div className="absolute inset-x-0 bottom-0 h-px bg-line-strong/50" />
          <div className="absolute inset-0 flex items-end gap-[2px]">
            {points.map((p, i) => {
              const h = p.saved_bytes > 0 ? Math.max(2, percentOf(p.saved_bytes, max)) : 0;
              return (
                <div
                  key={p.date}
                  className="flex h-full min-w-0 flex-1 items-end justify-center"
                  onMouseEnter={() => setActive(i)}
                >
                  <div
                    className={cn(
                      "w-full max-w-6 rounded-t-[4px] transition-colors duration-150",
                      active === i ? "bg-accent-hover" : "bg-meter",
                    )}
                    style={{ height: `${h}%` }}
                  />
                </div>
              );
            })}
          </div>
        </div>
        <span />
        <div className="mt-1.5 flex justify-between text-xs text-muted">
          <span>{points[0] ? formatDay(points[0].date) : ""}</span>
          <span>Today</span>
        </div>
      </div>
      <table className="sr-only">
        <caption>Space saved per day, last 30 days</caption>
        <thead>
          <tr>
            <th scope="col">Day</th>
            <th scope="col">Saved</th>
            <th scope="col">Files</th>
          </tr>
        </thead>
        <tbody>
          {points
            .filter((p) => p.saved_bytes !== 0 || p.files > 0)
            .map((p) => (
              <tr key={p.date}>
                <td>{formatDay(p.date)}</td>
                <td>{formatBytes(p.saved_bytes)}</td>
                <td>{formatCount(p.files)}</td>
              </tr>
            ))}
        </tbody>
      </table>
    </figure>
  );
}

/** Whether the savings history has enough days with savings to be worth a chart. */
export function worthCharting(points: SavingsPoint[], minDays = 3): boolean {
  return points.filter((p) => p.saved_bytes > 0).length >= minDays;
}
