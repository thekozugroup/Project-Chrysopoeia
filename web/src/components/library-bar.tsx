"use client";

/**
 * The library bar: one horizontal bar per library showing how much of it is
 * converted (solid gold), on its way (gold stripes), still to convert (dark
 * neutral) and skipped (quiet neutral). Failed conversions are a thin amber
 * mark; originals that can't be read aren't drawn at all (they're listed
 * under "Needs your attention"). Every segment is also said in words.
 */

import { formatCount, percentOf } from "@/lib/format";
import type { LibraryStats } from "@/lib/types";
import { cn } from "@/lib/utils";

interface Segment {
  key: string;
  label: string;
  count: number;
  className: string;
}

function segments(stats: LibraryStats, unreadable: number): Segment[] {
  return [
    { key: "done", label: "Converted", count: stats.done, className: "bg-meter" },
    {
      key: "active",
      label: "In queue or converting",
      count: stats.queued + stats.processing,
      className: "bar-active",
    },
    { key: "pending", label: "To convert", count: stats.pending, className: "bg-bar-todo" },
    { key: "skipped", label: "Skipped", count: stats.skipped, className: "bg-bar-skipped" },
    {
      key: "failed",
      label: "Couldn't convert",
      count: Math.max(0, stats.failed - unreadable),
      className: "bg-warning",
    },
  ];
}

/**
 * Share of files that are finished: converted, or left as they are
 * (skipped). Shown as "% finished", never "% done", because "Converted"
 * counts only the first kind.
 */
export function finishedPercent(stats: LibraryStats): number {
  return percentOf(stats.done + stats.skipped, stats.file_count);
}

/** Files still waiting for work. */
export function remainingCount(stats: LibraryStats): number {
  return stats.pending + stats.queued + stats.processing;
}

/** One-sentence summary for screen readers and tooltips. */
export function statsSentence(stats: LibraryStats, unreadable = 0): string {
  if (stats.file_count === 0) return "No videos found yet.";
  const failed = Math.max(0, stats.failed - unreadable);
  const parts = [
    `${formatCount(stats.done)} converted`,
    stats.queued + stats.processing ? `${formatCount(stats.queued + stats.processing)} in queue or converting` : null,
    stats.pending ? `${formatCount(stats.pending)} to convert` : null,
    stats.skipped ? `${formatCount(stats.skipped)} skipped` : null,
    failed ? `${formatCount(failed)} couldn't be converted` : null,
    unreadable ? `${formatCount(unreadable)} can't be read` : null,
  ].filter(Boolean);
  return `${formatCount(stats.file_count)} files: ${parts.join(", ")}.`;
}

export function LibraryBar({
  stats,
  unreadable = 0,
  size = "md",
  className,
}: {
  stats: LibraryStats;
  /** Failed files whose original can't be read: left out of the bar. */
  unreadable?: number;
  size?: "xs" | "md";
  className?: string;
}) {
  const total = Math.max(0, stats.file_count - unreadable);
  return (
    <div
      role="img"
      aria-label={statsSentence(stats, unreadable)}
      className={cn(
        "flex w-full gap-px overflow-hidden rounded-full bg-raised",
        size === "xs" ? "h-1" : "h-2",
        className,
      )}
    >
      {total > 0
        ? segments(stats, unreadable)
            .filter((s) => s.count > 0)
            .map((s) => (
              <div
                key={s.key}
                className={cn("h-full min-w-[3px] first:rounded-l-full last:rounded-r-full", s.className)}
                style={{ width: `${percentOf(s.count, total)}%` }}
              />
            ))
        : null}
    </div>
  );
}

/** The bar's legend: each segment with a swatch, a word and a count. */
export function LibraryLegend({
  stats,
  unreadable = 0,
  className,
}: {
  stats: LibraryStats;
  unreadable?: number;
  className?: string;
}) {
  return (
    <ul className={cn("flex flex-wrap gap-x-5 gap-y-1.5 text-[0.8125rem] text-muted", className)}>
      {segments(stats, unreadable)
        .filter((s) => s.count > 0 || s.key === "done")
        .map((s) => (
          <li key={s.key} className="inline-flex items-center gap-1.5">
            <span aria-hidden className={cn("size-2.5 rounded-[3px] ring-1 ring-line ring-inset", s.className)} />
            <span>
              {s.label} <span className="tabular font-medium text-fg">{formatCount(s.count)}</span>
            </span>
          </li>
        ))}
    </ul>
  );
}
