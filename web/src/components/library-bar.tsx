"use client";

/**
 * The library bar: one horizontal bar per library that fills from the left
 * as files finish. Finished files are one gold family (converted is solid
 * gold, skipped a pale gold tint with a gold outline, which keeps it 3:1
 * against the track), so the filled length is the "% finished" next to it;
 * files on their way are gold stripes; files still to convert are the plain
 * track. Failed conversions are a thin amber mark
 * at the end; originals that can't be read aren't drawn at all (they're
 * listed under "Needs your attention") and aren't counted in "% finished"
 * either. Every segment is also said in words.
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
    { key: "skipped", label: "Skipped", count: stats.skipped, className: "bar-skipped" },
    {
      key: "active",
      label: "In queue or converting",
      count: stats.queued + stats.processing,
      className: "bar-active",
    },
    // The plain track: drawn empty, so the filled part is what's finished.
    { key: "pending", label: "To convert", count: stats.pending, className: "bg-transparent" },
    {
      key: "failed",
      label: "Couldn't convert",
      count: Math.max(0, stats.failed - unreadable),
      className: "bg-warning",
    },
  ];
}

/**
 * Files the bar and "% finished" count: every file except originals that
 * can't be read, which are set aside until they're replaced or ignored.
 */
export function countedFiles(stats: LibraryStats, unreadable = 0): number {
  return Math.max(0, stats.file_count - unreadable);
}

/**
 * Share of files that are finished: converted, or left as they are
 * (skipped), out of the files the bar draws. Shown as "% finished", never
 * "% done", because "Converted" counts only the first kind.
 */
export function finishedPercent(stats: LibraryStats, unreadable = 0): number {
  return percentOf(stats.done + stats.skipped, countedFiles(stats, unreadable));
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
  const total = countedFiles(stats, unreadable);
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
            <span
              aria-hidden
              className={cn(
                "size-2.5 rounded-[3px] ring-1 ring-inset",
                // "To convert" is the empty track: an outlined swatch. "Skipped"
                // draws its own gold outline, as in the bar.
                s.key === "pending"
                  ? "bg-raised ring-line-strong/60"
                  : s.key === "skipped"
                    ? s.className
                    : cn("ring-line", s.className),
              )}
            />
            <span>
              {s.label} <span className="tabular font-medium text-fg">{formatCount(s.count)}</span>
            </span>
          </li>
        ))}
    </ul>
  );
}
