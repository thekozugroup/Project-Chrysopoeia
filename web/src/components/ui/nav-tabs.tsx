import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

export interface NavTab {
  href: string;
  label: ReactNode;
  active: boolean;
  count?: number | null;
}

/** Tabs that are links, so each tab has its own URL and survives reloads. */
export function NavTabs({ tabs, label, className }: { tabs: NavTab[]; label: string; className?: string }) {
  return (
    <nav aria-label={label} className={cn("-mx-4 overflow-x-auto px-4 sm:mx-0 sm:px-0", className)}>
      <ul className="flex min-w-max gap-1 border-b border-line">
        {tabs.map((tab) => (
          <li key={tab.href}>
            <a
              href={tab.href}
              aria-current={tab.active ? "page" : undefined}
              className={cn(
                "relative -mb-px flex h-10 items-center gap-2 border-b-2 px-3 text-sm font-medium no-underline transition-colors",
                tab.active
                  ? "border-accent-ink text-fg"
                  : "border-transparent text-muted hover:border-line-strong hover:text-fg",
              )}
            >
              {tab.label}
              {tab.count !== undefined && tab.count !== null ? (
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
          className="h-8 rounded-md border border-line-strong/60 bg-surface px-3 font-medium text-fg hover:bg-raised disabled:opacity-50"
        >
          Previous
        </button>
        <button
          type="button"
          onClick={() => onPage(offset + limit)}
          disabled={to >= total}
          className="h-8 rounded-md border border-line-strong/60 bg-surface px-3 font-medium text-fg hover:bg-raised disabled:opacity-50"
        >
          Next
        </button>
      </div>
    </div>
  );
}
