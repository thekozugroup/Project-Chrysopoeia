"use client";

/**
 * Server-side folder browser (`GET /api/fs/browse`). Click or press Enter to
 * open a folder, Backspace or ← to go up, ↑/↓ to move; "Use this folder"
 * picks the folder you are in.
 */

import { ChevronLeft, ChevronRight, CornerDownLeft, Folder, FolderOpen, HardDrive } from "lucide-react";
import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/controls";
import { Callout, Skeleton } from "@/components/ui/display";
import { ApiError, errorMessage } from "@/lib/api";
import { plural } from "@/lib/format";
import { useBrowse } from "@/lib/queries";
import type { FsBrowse } from "@/lib/types";
import { cn } from "@/lib/utils";

interface Crumb {
  label: string;
  path: string;
}

/** Breadcrumbs from the root that contains `path` down to `path`. */
function crumbsFor(data: FsBrowse): Crumb[] {
  const root =
    [...data.roots].sort((a, b) => b.length - a.length).find((r) => data.path === r || data.path.startsWith(r.endsWith("/") ? r : `${r}/`)) ??
    "/";
  const crumbs: Crumb[] = [{ label: root === "/" ? "Server" : root, path: root }];
  const rest = data.path.slice(root.length).split("/").filter(Boolean);
  let current = root.replace(/\/$/, "");
  for (const part of rest) {
    current = `${current}/${part}`;
    crumbs.push({ label: part, path: current });
  }
  return crumbs;
}

interface FolderPickerProps {
  /** Folder to open first; defaults to the first browse root. */
  initialPath?: string;
  /** Called with the folder the user settled on. */
  onSelect: (path: string) => void;
  actionLabel?: string;
  /** Inline error from the step that uses the folder (e.g. library_exists). */
  error?: string | null;
  busy?: boolean;
  className?: string;
  /** Called whenever the user navigates, so parents can clear stale errors. */
  onNavigate?: (path: string) => void;
}

export function FolderPicker({
  initialPath,
  onSelect,
  actionLabel = "Use this folder",
  error,
  busy,
  className,
  onNavigate,
}: FolderPickerProps) {
  const [path, setPath] = useState<string | undefined>(initialPath || undefined);
  const [typed, setTyped] = useState("");
  const [showTyped, setShowTyped] = useState(false);
  const [focusIndex, setFocusIndex] = useState(0);
  const listRef = useRef<HTMLUListElement>(null);
  const shouldFocusList = useRef(false);
  const browse = useBrowse(path);
  // While a new folder loads, the previous listing stays on screen
  // (placeholder data). It must not be picked or clicked as if it were the
  // new one: "Use this folder" would choose the parent.
  const loadingNew = browse.isPlaceholderData;
  const data = browse.error ? undefined : browse.data;
  const current = loadingNew ? (path ?? "") : (data?.path ?? path ?? "");
  // The last folder that opened, to fall back to when another one fails.
  const [lastGood, setLastGood] = useState<FsBrowse | null>(null);
  if (data && !loadingNew && data !== lastGood) setLastGood(data);
  const shown = data ?? (browse.error ? lastGood : undefined);

  const go = (next: string | undefined, focusList = true) => {
    shouldFocusList.current = focusList;
    setFocusIndex(0);
    setPath(next);
    if (next) onNavigate?.(next);
  };

  // After navigating with the keyboard, keep focus in the list.
  useEffect(() => {
    if (!shouldFocusList.current || browse.isFetching || !data) return;
    shouldFocusList.current = false;
    const first = listRef.current?.querySelector<HTMLButtonElement>("button[data-index='0']");
    first?.focus();
  }, [browse.isFetching, data]);

  const entries = data?.entries.filter((e) => e.is_dir) ?? [];
  const parent = shown?.parent ?? null;

  const onKeyDown = (event: KeyboardEvent<HTMLUListElement>) => {
    const count = entries.length;
    const move = (index: number) => {
      const next = Math.max(0, Math.min(count - 1, index));
      setFocusIndex(next);
      listRef.current?.querySelector<HTMLButtonElement>(`button[data-index='${next}']`)?.focus();
    };
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        move(focusIndex + 1);
        break;
      case "ArrowUp":
        event.preventDefault();
        move(focusIndex - 1);
        break;
      case "Home":
        event.preventDefault();
        move(0);
        break;
      case "End":
        event.preventDefault();
        move(count - 1);
        break;
      case "ArrowRight": {
        const entry = entries[focusIndex];
        if (entry) {
          event.preventDefault();
          go(entry.path);
        }
        break;
      }
      case "Backspace":
      case "ArrowLeft":
        if (parent) {
          event.preventDefault();
          go(parent);
        }
        break;
      default:
        break;
    }
  };

  const forbidden = browse.error instanceof ApiError && browse.error.status === 403;

  return (
    <div className={cn("flex flex-col overflow-hidden rounded-lg border border-line bg-surface", className)}>
      <div className="flex flex-wrap items-center gap-2 border-b border-line px-3 py-2.5">
        <Button
          variant="quiet"
          size="icon-sm"
          aria-label="Up one folder"
          disabled={!parent}
          onClick={() => parent && go(parent, false)}
        >
          <ChevronLeft />
        </Button>
        <nav aria-label="Folder path" className="min-w-0 flex-1">
          <ol className="flex min-w-0 flex-wrap items-center gap-0.5 font-mono text-[0.8125rem]">
            {shown
              ? crumbsFor(shown).map((crumb, i, all) => {
                  const last = i === all.length - 1;
                  return (
                    <li key={crumb.path} className="flex min-w-0 items-center gap-0.5">
                      {i > 0 ? <ChevronRight className="size-3.5 shrink-0 text-muted" aria-hidden /> : null}
                      {last ? (
                        <span aria-current="location" className="truncate px-1 font-medium text-fg">
                          {i === 0 ? <HardDrive className="mr-1 inline size-3.5 align-[-2px]" aria-hidden /> : null}
                          {crumb.label}
                        </span>
                      ) : (
                        <button
                          type="button"
                          onClick={() => go(crumb.path, false)}
                          className="truncate rounded px-1 text-muted hover:bg-raised hover:text-fg"
                        >
                          {i === 0 ? <HardDrive className="mr-1 inline size-3.5 align-[-2px]" aria-hidden /> : null}
                          {crumb.label}
                        </button>
                      )}
                    </li>
                  );
                })
              : <Skeleton className="h-4 w-40" />}
          </ol>
        </nav>
        <Button variant="quiet" size="sm" onClick={() => setShowTyped((v) => !v)} aria-expanded={showTyped}>
          Type a path
        </Button>
      </div>

      {showTyped ? (
        <form
          className="flex gap-2 border-b border-line px-3 py-2.5"
          onSubmit={(e) => {
            e.preventDefault();
            const value = typed.trim();
            if (value) go(value, true);
          }}
        >
          <Input
            aria-label="Folder path"
            placeholder="/media/movies"
            value={typed}
            onChange={(e) => setTyped(e.target.value)}
            className="font-mono text-[0.8125rem]"
            autoFocus
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
          />
          <Button type="submit" variant="secondary">
            <CornerDownLeft aria-hidden />
            Go
          </Button>
        </form>
      ) : null}

      {shown && shown.roots.length > 1 ? (
        <div className="flex flex-wrap gap-1.5 border-b border-line px-3 py-2" aria-label="Allowed folders">
          {shown.roots.map((root) => (
            <button
              key={root}
              type="button"
              onClick={() => go(root, false)}
              className={cn(
                "rounded-full border px-2.5 py-0.5 font-mono text-xs",
                current.startsWith(root) ? "border-accent-ink/50 text-accent-ink" : "border-line text-muted hover:text-fg",
              )}
            >
              {root}
            </button>
          ))}
        </div>
      ) : null}

      <div className="relative min-h-0 flex-1 overflow-y-auto" style={{ maxHeight: "22rem", minHeight: "14rem" }}>
        {browse.isPending ? (
          <div className="space-y-3 p-4" aria-label="Loading folders">
            {[0, 1, 2, 3, 4].map((i) => (
              <div key={i} className="flex items-center gap-3">
                <Skeleton className="size-5 rounded" />
                <Skeleton className="h-4" style={{ width: `${40 + ((i * 17) % 35)}%` }} />
              </div>
            ))}
          </div>
        ) : browse.error ? (
          <div className="p-4">
            <Callout
              tone={forbidden ? "warning" : "danger"}
              title={forbidden ? "That folder isn't available" : "Couldn't open that folder"}
              action={
                <div className="flex flex-wrap gap-2">
                  {lastGood && lastGood.path !== path ? (
                    <Button variant="secondary" size="sm" onClick={() => go(lastGood.path, false)}>
                      Back to {lastGood.path}
                    </Button>
                  ) : null}
                  <Button variant="quiet" size="sm" onClick={() => go(undefined, false)}>
                    Start from the top
                  </Button>
                </div>
              }
            >
              {errorMessage(browse.error)}
            </Callout>
          </div>
        ) : entries.length === 0 ? (
          <div className="flex h-full min-h-56 flex-col items-center justify-center gap-1 px-6 text-center">
            <FolderOpen className="size-6 text-muted" aria-hidden />
            <p className="text-sm font-medium text-fg">No folders inside</p>
            <p className="text-[0.8125rem] text-muted">You can still use this folder.</p>
          </div>
        ) : (
          <ul
            ref={listRef}
            aria-label={`Folders in ${current}`}
            aria-busy={loadingNew || undefined}
            onKeyDown={onKeyDown}
            // The old listing can't be clicked while the new one loads.
            inert={loadingNew || undefined}
            className={cn("py-1", browse.isFetching && "opacity-60 transition-opacity")}
          >
            {entries.map((entry, i) => (
              <li key={entry.path}>
                <button
                  type="button"
                  data-index={i}
                  tabIndex={i === focusIndex ? 0 : -1}
                  onFocus={() => setFocusIndex(i)}
                  onClick={() => go(entry.path)}
                  className="flex w-full items-center gap-3 px-4 py-2 text-left text-sm hover:bg-raised focus-visible:bg-raised focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent-ink"
                >
                  <Folder className="size-[1.125rem] shrink-0 text-accent-ink" aria-hidden />
                  <span className="min-w-0 flex-1 truncate text-fg">{entry.name}</span>
                  {entry.media_count ? (
                    <span className="shrink-0 text-xs text-muted tabular">{plural(entry.media_count, "video")}</span>
                  ) : null}
                  <ChevronRight className="size-4 shrink-0 text-muted" aria-hidden />
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>

      <div className="flex flex-col gap-2 border-t border-line bg-sunken/60 px-3 py-3 sm:flex-row sm:items-center sm:justify-between">
        <p className="min-w-0 text-[0.8125rem] text-muted">
          <span className="sr-only">Current folder: </span>
          <span className="block truncate font-mono text-fg">{current || "…"}</span>
        </p>
        <Button
          variant="primary"
          onClick={() => data && !loadingNew && onSelect(data.path)}
          disabled={!data || loadingNew || Boolean(browse.error)}
          loading={busy}
        >
          {actionLabel}
        </Button>
      </div>
      {error ? (
        <p role="alert" className="border-t border-danger/30 bg-danger-soft px-4 py-2.5 text-[0.8125rem] font-medium text-danger">
          {error}
        </p>
      ) : null}
    </div>
  );
}
