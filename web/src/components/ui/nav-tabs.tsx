"use client";

import { useEffect, useRef, useState, type ReactNode } from "react";
import { clampOffset } from "@/lib/progress";
import { updateParams } from "@/lib/router";
import { cn } from "@/lib/utils";

export interface NavTab {
  href: string;
  label: ReactNode;
  active: boolean;
  count?: number | null;
}

/**
 * Tabs that are links, so each tab has its own URL and survives reloads. On
 * narrow screens the row scrolls sideways: the active tab is scrolled into
 * view and a soft fade marks the edge that has more tabs behind it.
 */
export function NavTabs({ tabs, label, className }: { tabs: NavTab[]; label: string; className?: string }) {
  const scroller = useRef<HTMLElement>(null);
  const [edges, setEdges] = useState({ left: false, right: false });
  const activeHref = tabs.find((t) => t.active)?.href;

  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const measure = () => {
      const left = el.scrollLeft > 2;
      const right = el.scrollLeft + el.clientWidth < el.scrollWidth - 2;
      setEdges((prev) => (prev.left === left && prev.right === right ? prev : { left, right }));
    };
    measure();
    el.addEventListener("scroll", measure, { passive: true });
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    observer?.observe(el);
    return () => {
      el.removeEventListener("scroll", measure);
      observer?.disconnect();
    };
  }, []);

  // Keep the current tab visible when the row is wider than the screen.
  useEffect(() => {
    const el = scroller.current;
    const active = el?.querySelector<HTMLElement>("[aria-current='page']");
    if (!el || !active) return;
    const start = active.offsetLeft - 16;
    const end = active.offsetLeft + active.offsetWidth + 16;
    if (start < el.scrollLeft) el.scrollLeft = start;
    else if (end > el.scrollLeft + el.clientWidth) el.scrollLeft = end - el.clientWidth;
  }, [activeHref]);

  const fade =
    edges.left && edges.right
      ? "[mask-image:linear-gradient(to_right,transparent,black_2rem,black_calc(100%-2rem),transparent)]"
      : edges.right
        ? "[mask-image:linear-gradient(to_right,black_calc(100%-2.5rem),transparent)]"
        : edges.left
          ? "[mask-image:linear-gradient(to_right,transparent,black_2.5rem)]"
          : "";

  return (
    <nav
      ref={scroller}
      aria-label={label}
      className={cn("-mx-4 overflow-x-auto px-4 [scrollbar-width:none] sm:mx-0 sm:px-0", fade, className)}
    >
      <ul className="flex min-w-max gap-1 border-b border-line">
        {tabs.map((tab) => (
          <li key={tab.href}>
            <a
              href={tab.href}
              aria-current={tab.active ? "page" : undefined}
              className={cn(
                "relative -mb-px flex h-10 items-center gap-2 border-b-2 px-3 text-sm font-medium no-underline transition-colors pointer-coarse:h-11",
                tab.active
                  ? "border-accent-ink text-fg"
                  : "border-transparent text-muted hover:border-line-strong hover:text-fg",
              )}
            >
              {tab.label}
              {/* A zero count says nothing the empty tab doesn't. */}
              {tab.count ? (
                <span
                  className={cn(
                    "rounded-full px-1.5 text-xs tabular",
                    tab.active ? "bg-accent-soft text-accent-ink" : "bg-raised text-muted",
                  )}
                >
                  {tab.count.toLocaleString()}
                </span>
              ) : null}
            </a>
          </li>
        ))}
      </ul>
    </nav>
  );
}

/** "1–50 of 1,208" with previous/next buttons. */
export function Pager({
  offset,
  limit,
  total,
  onPage,
  noun = "items",
}: {
  offset: number;
  limit: number;
  total: number;
  onPage: (offset: number) => void;
  noun?: string;
}) {
  if (total <= limit && offset === 0) return null;
  const from = total === 0 ? 0 : offset + 1;
  const to = Math.min(total, offset + limit);
  return (
    <div className="mt-4 flex items-center justify-between gap-3 text-[0.8125rem] text-muted">
      <p className="tabular" aria-live="polite">
        {from.toLocaleString()}–{to.toLocaleString()} of {total.toLocaleString()} {noun}
      </p>
      <div className="flex gap-2">
        <button
          type="button"
          onClick={() => onPage(Math.max(0, offset - limit))}
          disabled={offset === 0}
          className="h-8 rounded-md border border-line-strong/60 bg-surface px-3 font-medium text-fg hover:bg-raised disabled:opacity-50 pointer-coarse:h-11"
        >
          Previous
        </button>
        <button
          type="button"
          onClick={() => onPage(offset + limit)}
          disabled={to >= total}
          className="h-8 rounded-md border border-line-strong/60 bg-surface px-3 font-medium text-fg hover:bg-raised disabled:opacity-50 pointer-coarse:h-11"
        >
          Next
        </button>
      </div>
    </div>
  );
}

/**
 * When the page in `?offset=` points past the end of a list (the list
 * shrank, or an old link), move to the last page that exists instead of
 * showing an empty page that claims there is nothing.
 */
export function useClampedOffset(offset: number, total: number | undefined, limit: number): void {
  useEffect(() => {
    if (total === undefined) return;
    const fixed = clampOffset(offset, total, limit);
    if (fixed !== null) updateParams({ offset: fixed || null });
  }, [offset, total, limit]);
}
