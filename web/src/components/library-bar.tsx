"use client";

/**
 * The library bar: one horizontal bar per library showing how much of it is
 * converted (gold), on its way (soft gold), still to do, skipped and failed.
 * Every segment is also listed in words.
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

function segments(stats: LibraryStats): Segment[] {
  return [
    { key: "done", label: "Done", count: stats.done, className: "bg-meter" },
    {
      key: "active",
      label: "In queue or converting",
      count: stats.queued + stats.processing,
      className: "bg-meter/45",
    },
    { key: "pending", label: "To convert", count: stats.pending, className: "bg-line-strong/60" },
    { key: "skipped", label: "Skipped", count: stats.skipped, className: "bg-line" },
    { key: "failed", label: "Failed", count: stats.failed, className: "bg-danger" },
  ];
}

/** Share of files that need no more work (done or skipped), 0..100. */
export function finishedPercent(stats: LibraryStats): number {
  return percentOf(stats.done + stats.skipped, stats.file_count);
}

/** Files still waiting for work. */
export function remainingCount(stats: LibraryStats): number {
  return stats.pending + stats.queued + stats.processing;
}

/** One-sentence summary for screen readers and tooltips. */
export function statsSentence(stats: LibraryStats): string {
  if (stats.file_count === 0) return "No media files found yet.";
  const parts = [
    `${formatCount(stats.done)} done`,
    stats.queued + stats.processing ? `${formatCount(stats.queued + stats.processing)} in queue or converting` : null,
    stats.pending ? `${formatCount(stats.pending)} to convert` : null,
    stats.skipped ? `${formatCount(stats.skipped)} skipped` : null,
    stats.failed ? `${formatCount(stats.failed)} failed` : null,
  ].filter(Boolean);
  return `${formatCount(stats.file_count)} files: ${parts.join(", ")}.`;
}

export function LibraryBar({
  stats,
  size = "md",
  className,
}: {
  stats: LibraryStats;
  size?: "xs" | "md";
  className?: string;
}) {
  const total = stats.file_count;
  return (
    <div
      role="img"
      aria-label={statsSentence(stats)}
      className={cn(
        "flex w-full gap-px overflow-hidden rounded-full bg-raised",
        size === "xs" ? "h-1" : "h-2.5",
        className,
      )}
    >
      {total > 0
        ? segments(stats)
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
export function LibraryLegend({ stats, className }: { stats: LibraryStats; className?: string }) {
  return (
    <ul className={cn("flex flex-wrap gap-x-5 gap-y-1.5 text-[0.8125rem] text-muted", className)}>
      {segments(stats)
        .filter((s) => s.count > 0 || s.key === "done")
        .map((s) => (
          <li key={s.key} className="inline-flex items-center gap-1.5">
            <span aria-hidden className={cn("size-2.5 rounded-[3px]", s.className)} />
            <span>
              {s.label} <span className="tabular font-medium text-fg">{formatCount(s.count)}</span>
            </span>
          </li>
        ))}
    </ul>
  );
}
