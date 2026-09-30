"use client";

/**
 * Server-side folder browser (`GET /api/fs/browse`). Click or press Enter to
 * open a folder, Backspace or ← to go up, ↑/↓ to move; the "Use “…”" button
 * names the folder you are in, and how many videos it holds, and picks it.
 */

import { ChevronLeft, ChevronRight, CornerDownLeft, Folder, FolderOpen, HardDrive } from "lucide-react";
import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/controls";
import { Callout, Skeleton } from "@/components/ui/display";
import { ApiError, errorMessage } from "@/lib/api";
import { formatCount } from "@/lib/format";
import { useBrowse } from "@/lib/queries";
import type { FsBrowse } from "@/lib/types";
import { cn } from "@/lib/utils";

const NO_LIBRARIES: readonly KnownLibrary[] = [];

interface Crumb {
  label: string;
  path: string;
}

/**
 * The browse root a folder lives under: the longest root that is the folder
 * itself or one of its parents (`/media` holds `/media/tv`, not `/media2`).
 */
export function rootOf(path: string, roots: string[]): string | null {
  return (
    [...roots]
      .sort((a, b) => b.length - a.length)
      .find((r) => path === r || path.startsWith(r.endsWith("/") ? r : `${r}/`)) ?? null
  );
}

/** Breadcrumbs from the root that contains `path` down to `path`. */
function crumbsFor(data: FsBrowse): Crumb[] {
  const root = rootOf(data.path, data.roots) ?? "/";
  const crumbs: Crumb[] = [{ label: root === "/" ? "Server" : root, path: root }];
  const rest = data.path.slice(root.length).split("/").filter(Boolean);
  let current = root.replace(/\/$/, "");
  for (const part of rest) {
    current = `${current}/${part}`;
    crumbs.push({ label: part, path: current });
  }
  return crumbs;
}

/**
 * "12 videos", "1 video", "1,000+ videos" (the server stopped counting), or
 * `null` when there are none or the server didn't count.
 */
export function videoCount(count: number | null | undefined, capped = false): string | null {
  if (!count || count <= 0) return null;
  return `${formatCount(count)}${capped ? "+" : ""} ${count === 1 && !capped ? "video" : "videos"}`;
}

/**
 * The count on the "Use" button: "1,204 videos", "2,000+ videos", or "no
 * videos", so picking a folder that holds none is noticed before Start.
 */
export function folderVideos(count: number, capped = false): string {
  return videoCount(count, capped) ?? "no videos";
}

/** A library already set up, as the picker needs it. */
export interface KnownLibrary {
  name: string;
  path: string;
}

/** A path without trailing slashes ("/media/tv/" → "/media/tv"; "/" stays). */
function trimSlashes(path: string): string {
  return path.replace(/(.)\/+$/, "$1");
}

/** Whether `child` is a folder somewhere inside `parent`. */
function isInside(child: string, parent: string): boolean {
  return parent === "/" ? child !== "/" : child.startsWith(`${parent}/`);
}

/**
 * Why a folder can't become a new library, in the server's terms
 * (`library_exists`, `library_overlaps`), so it's said before the goal step
 * rather than after "Add library": it is a library, is inside one, or holds
 * one. `null` when it's free. The server still checks (it also resolves
 * links).
 */
export function libraryConflict(path: string, libraries: readonly KnownLibrary[]): string | null {
  const folder = trimSlashes(path);
  for (const library of libraries) {
    const existing = trimSlashes(library.path);
    if (folder === existing) return `This folder is already the library “${library.name}”.`;
    if (isInside(folder, existing)) return `This folder is inside “${library.name}”, which is already a library.`;
    if (isInside(existing, folder)) {
      return `This folder contains the library “${library.name}”. Pick a folder inside it or next to it, or remove “${library.name}” first.`;
    }
  }
  return null;
}

/** The library a folder is, if any, to mark it in the listing. */
function libraryAt(path: string, libraries: readonly KnownLibrary[]): KnownLibrary | undefined {
  const folder = trimSlashes(path);
  return libraries.find((l) => trimSlashes(l.path) === folder);
}

/** The last part of a folder path, for naming it on the button. */
function folderName(path: string, roots: string[]): string {
  const trimmed = path.replace(/(.)\/+$/, "$1");
  if (trimmed === "/") return "/";
  if (roots.includes(trimmed)) return trimmed;
  return trimmed.slice(trimmed.lastIndexOf("/") + 1) || trimmed;
}

interface FolderPickerProps {
  /** Folder to open first; defaults to the first browse root. */
  initialPath?: string;
  /** Called with the folder the user settled on. */
  onSelect: (path: string) => void;
  /** Button text instead of "Use “<folder>”". */
  actionLabel?: string;
  /** Inline error from the step that uses the folder (e.g. library_exists). */
  error?: string | null;
  busy?: boolean;
  className?: string;
  /** Called whenever the user navigates, so parents can clear stale errors. */
  onNavigate?: (path: string) => void;
  /**
   * Libraries already set up, when picking a folder for a new one: they're
   * marked in the listing, and a folder that is, holds or is inside one
   * can't be used (see `libraryConflict`).
   */
  libraries?: readonly KnownLibrary[];
}

export function FolderPicker({
  initialPath,
  onSelect,
  actionLabel,
  error,
  busy,
  className,
  onNavigate,
  libraries = NO_LIBRARIES,
}: FolderPickerProps) {
  const [path, setPath] = useState<string | undefined>(initialPath || undefined);
  const [typed, setTyped] = useState("");
  const [showTyped, setShowTyped] = useState(false);
  // No row is highlighted until an arrow key moves into the list.
  const [focusIndex, setFocusIndex] = useState(-1);
  const listRef = useRef<HTMLUListElement>(null);
  const emptyRef = useRef<HTMLDivElement>(null);
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
  // Video counts seen in listings (the entries' and the folder's own), so a
  // folder opened from its parent keeps the count it showed there.
  const [counts, setCounts] = useState<Record<string, { count: number; capped: boolean }>>({});
  const [countedFrom, setCountedFrom] = useState<FsBrowse | null>(null);
  if (data && data !== countedFrom) {
    setCountedFrom(data);
    const next: Record<string, { count: number; capped: boolean }> = {};
    for (const e of data.entries) {
      if (typeof e.media_count === "number") next[e.path] = { count: e.media_count, capped: Boolean(e.media_count_capped) };
    }
    if (typeof data.media_count === "number") {
      next[data.path] = { count: data.media_count, capped: Boolean(data.media_count_capped) };
    }
    if (Object.keys(next).length) setCounts((prev) => ({ ...prev, ...next }));
  }
  const own = counts[current];

  const go = (next: string | undefined, focusList = true) => {
    shouldFocusList.current = focusList;
    setFocusIndex(-1);
    setPath(next);
    if (next) onNavigate?.(next);
  };

  // After opening a folder from the list, keep focus in the list (on the
  // list itself, so no row looks chosen), or on the "No folders inside"
  // note (where Backspace still goes up) when it's empty.
  useEffect(() => {
    if (!shouldFocusList.current || browse.isFetching || !data) return;
    shouldFocusList.current = false;
    if (listRef.current) listRef.current.focus();
    else emptyRef.current?.focus();
  }, [browse.isFetching, data]);

  const entries = data?.entries.filter((e) => e.is_dir) ?? [];
  const parent = shown?.parent ?? null;

  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
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
        const entry = focusIndex >= 0 ? entries[focusIndex] : undefined;
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
  const conflict = data && !loadingNew ? libraryConflict(data.path, libraries) : null;
  const conflictId = useId();

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
        <div className="flex flex-wrap gap-1.5 border-b border-line px-3 py-2" role="group" aria-label="Allowed folders">
          {shown.roots.map((root) => {
            const active = rootOf(current, shown.roots) === root;
            return (
              <button
                key={root}
                type="button"
                aria-pressed={active}
                onClick={() => go(root, false)}
                className={cn(
                  "rounded-full border px-2.5 py-0.5 font-mono text-xs",
                  active ? "border-accent-ink/50 text-accent-ink" : "border-line text-muted hover:text-fg",
                )}
              >
                {root}
              </button>
            );
          })}
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
          <div
            ref={emptyRef}
            tabIndex={-1}
            onKeyDown={onKeyDown}
            className="flex h-full min-h-56 flex-col items-center justify-center gap-1 px-6 text-center outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent-ink"
          >
            <FolderOpen className="size-6 text-muted" aria-hidden />
            <p className="text-sm font-medium text-fg">No folders inside</p>
            <p className="text-[0.8125rem] text-muted">You can still use this folder.</p>
          </div>
        ) : (
          <ul
            ref={listRef}
            tabIndex={-1}
            aria-label={`Folders in ${current}`}
            aria-busy={loadingNew || undefined}
            onKeyDown={onKeyDown}
            // The old listing can't be clicked while the new one loads.
            inert={loadingNew || undefined}
            className={cn("py-1 outline-none", browse.isFetching && "opacity-60 transition-opacity")}
          >
            {entries.map((entry, i) => {
              const known = libraryAt(entry.path, libraries);
              return (
                <li key={entry.path}>
                  <button
                    type="button"
                    data-index={i}
                    tabIndex={i === Math.max(0, focusIndex) ? 0 : -1}
                    onFocus={() => setFocusIndex(i)}
                    onClick={() => go(entry.path)}
                    className="flex w-full items-center gap-3 px-4 py-2 text-left text-sm hover:bg-raised focus-visible:bg-raised focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent-ink pointer-coarse:min-h-11"
                  >
                    <Folder className="size-[1.125rem] shrink-0 text-accent-ink" aria-hidden />
                    <span className="min-w-0 flex-1 truncate text-fg">{entry.name}</span>
                    {known ? (
                      <span
                        className="shrink-0 rounded-full border border-line-strong/60 px-2 py-0.5 text-xs text-muted"
                        title={`Already the library “${known.name}”`}
                      >
                        Library
                      </span>
                    ) : null}
                    {videoCount(entry.media_count, entry.media_count_capped) ? (
                      <span className="shrink-0 text-[0.8125rem] text-muted tabular">
                        {videoCount(entry.media_count, entry.media_count_capped)}
                      </span>
                    ) : null}
                    <ChevronRight className="size-4 shrink-0 text-muted" aria-hidden />
                  </button>
                </li>
              );
            })}
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
          onClick={() => data && !loadingNew && !conflict && onSelect(data.path)}
          disabled={!data || loadingNew || Boolean(browse.error) || Boolean(conflict)}
          aria-describedby={conflict ? conflictId : undefined}
          loading={busy}
          className="max-w-full min-w-0"
        >
          {actionLabel ?? (
            <>
              <span className="min-w-0 truncate">Use “{folderName(current || "/", shown?.roots ?? [])}”</span>
              {own && !loadingNew ? <span className="shrink-0 font-normal">· {folderVideos(own.count, own.capped)}</span> : null}
            </>
          )}
        </Button>
      </div>
      {conflict ? (
        <p id={conflictId} role="status" className="border-t border-warning/30 bg-warning-soft px-4 py-2.5 text-[0.8125rem] font-medium text-fg">
          {conflict}
        </p>
      ) : error ? (
        <p role="alert" className="border-t border-danger/30 bg-danger-soft px-4 py-2.5 text-[0.8125rem] font-medium text-danger">
          {error}
        </p>
      ) : null}
    </div>
  );
}
