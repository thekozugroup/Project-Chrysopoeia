"use client";

/**
 * Tiny hash router. The Rust server only serves one `index.html`, so every
 * view lives in the URL hash (`#/library/<id>?status=failed`) and reloads or
 * shared links land on the same view.
 *
 * A screen with unsaved edits can register a navigation guard: leaving it
 * (a link, the back button, a typed address) then waits for the user to
 * save, discard or keep editing instead of silently dropping the edits.
 */

import { useEffect, useMemo, useRef, useSyncExternalStore } from "react";

/** A parsed location: `#/library/abc?file=1` → `["library","abc"]` + params. */
export interface Route {
  segments: string[];
  params: URLSearchParams;
  /** The hash without the leading `#`, e.g. `/queue/history`. */
  raw: string;
}

/** Parse a hash (with or without `#`) into a route. */
export function parseRoute(hash: string): Route {
  const raw = hash.replace(/^#/, "") || "/";
  // Split at the first "?" only: a typed search may contain another one.
  const mark = raw.indexOf("?");
  const pathPart = mark === -1 ? raw : raw.slice(0, mark);
  const queryPart = mark === -1 ? "" : raw.slice(mark + 1);
  const segments = pathPart
    .split("/")
    .filter(Boolean)
    .map((s) => {
      try {
        return decodeURIComponent(s);
      } catch {
        return s;
      }
    });
  return { segments, params: new URLSearchParams(queryPart), raw };
}

// ---------------------------------------------------------------------------
// Navigation guard
// ---------------------------------------------------------------------------

/** What a screen with unsaved edits tells the router. */
export interface NavigationGuard {
  /** Whether going to `target` would throw the edits away. */
  blocks: (target: Route) => boolean;
  /** Save the edits. Resolves `true` when they were saved. */
  save: () => Promise<boolean>;
  /** Throw the edits away. */
  discard: () => void;
}

/** Router state shared by every `useRoute` caller. */
const state = {
  /** The hash the app is showing; `null` until first read in the browser. */
  rendered: null as string | null,
  guard: null as NavigationGuard | null,
  /** A navigation waiting for the user's answer (a hash), if any. */
  pending: null as string | null,
  /** Bumped on every change, for `useSyncExternalStore`. */
  version: 0,
};

const subscribers = new Set<() => void>();

function emit(): void {
  state.version += 1;
  for (const fn of subscribers) fn();
}

function urlFor(hash: string): string {
  return `${window.location.pathname}${window.location.search}${hash}`;
}

function renderedHash(): string {
  if (state.rendered === null) state.rendered = window.location.hash;
  return state.rendered;
}

/** Follow the address bar, unless a guard wants to ask first. */
function onHashChange(): void {
  const next = window.location.hash;
  const current = renderedHash();
  if (next === current) return;
  if (state.guard?.blocks(parseRoute(next))) {
    // Back, forward or a typed address: put the address back and ask.
    window.history.replaceState(window.history.state, "", urlFor(current));
    state.pending = next;
    emit();
    return;
  }
  state.rendered = next;
  emit();
}

/**
 * Link clicks are caught before they change the address, so a blocked click
 * leaves no extra history entry behind.
 */
function onDocumentClick(event: MouseEvent): void {
  const guard = state.guard;
  if (!guard || event.defaultPrevented || event.button !== 0) return;
  if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
  const target = event.target instanceof Element ? event.target.closest("a[href]") : null;
  if (!target || target.getAttribute("target") === "_blank") return;
  const hrefAttr = target.getAttribute("href") ?? "";
  if (!hrefAttr.startsWith("#/")) return;
  if (!guard.blocks(parseRoute(hrefAttr))) return;
  event.preventDefault();
  state.pending = hrefAttr;
  emit();
}

function subscribe(onChange: () => void): () => void {
  if (subscribers.size === 0) {
    state.rendered = window.location.hash;
    window.addEventListener("hashchange", onHashChange);
  }
  subscribers.add(onChange);
  return () => {
    subscribers.delete(onChange);
    if (subscribers.size === 0) window.removeEventListener("hashchange", onHashChange);
  };
}

function snapshot(): string {
  return renderedHash();
}

function serverSnapshot(): string {
  return "";
}

/**
 * Register a guard. Returns the function that removes it. Only one screen
 * holds unsaved edits at a time; a newer guard replaces an older one.
 */
export function registerGuard(guard: NavigationGuard): () => void {
  state.guard = guard;
  document.addEventListener("click", onDocumentClick, true);
  return () => {
    if (state.guard !== guard) return;
    state.guard = null;
    document.removeEventListener("click", onDocumentClick, true);
    if (state.pending) {
      state.pending = null;
      emit();
    }
  };
}

/**
 * Hold navigation while `active` (the form is dirty). The callbacks may
 * change every render; the latest ones are used.
 */
export function useNavigationGuard(active: boolean, guard: NavigationGuard): void {
  const latest = useRef(guard);
  useEffect(() => {
    latest.current = guard;
  });
  useEffect(() => {
    if (!active) return;
    return registerGuard({
      blocks: (target) => latest.current.blocks(target),
      save: () => latest.current.save(),
      discard: () => latest.current.discard(),
    });
  }, [active]);
}

/** The navigation waiting for an answer, and the answers. */
export function usePendingNavigation(): {
  pending: string | null;
  save: () => Promise<void>;
  discard: () => void;
  stay: () => void;
} {
  useSyncExternalStore(subscribe, () => state.version, () => 0);
  const go = (target: string) => {
    state.pending = null;
    state.guard = null;
    document.removeEventListener("click", onDocumentClick, true);
    emit();
    navigate(target);
  };
  return {
    pending: state.pending,
    save: async () => {
      const target = state.pending;
      const guard = state.guard;
      if (!target || !guard) return;
      const saved = await guard.save();
      if (saved) go(target);
      else {
        state.pending = null;
        emit();
      }
    },
    discard: () => {
      const target = state.pending;
      if (!target) return;
      state.guard?.discard();
      go(target);
    },
    stay: () => {
      state.pending = null;
      emit();
    },
  };
}

// ---------------------------------------------------------------------------
// Reading and changing the route
// ---------------------------------------------------------------------------

/** The current route; re-renders on every hash change. */
export function useRoute(): Route {
  const hash = useSyncExternalStore(subscribe, snapshot, serverSnapshot);
  return useMemo(() => parseRoute(hash), [hash]);
}

/** `href` for a route path, e.g. `href("/queue")` → `#/queue`. */
export function href(path: string, params?: Record<string, string | number | null | undefined>): string {
  const clean = path.startsWith("/") ? path : `/${path}`;
  const query = new URLSearchParams();
  for (const [key, value] of Object.entries(params ?? {})) {
    if (value !== null && value !== undefined && value !== "") query.set(key, String(value));
  }
  const qs = query.toString();
  return `#${clean}${qs ? `?${qs}` : ""}`;
}

function normalizeTarget(target: string): string {
  return target.startsWith("#") ? target : `#${target.startsWith("/") ? target : `/${target}`}`;
}

/**
 * Go to a route. `replace` avoids adding a history entry (filters, tabs);
 * `state` is stored with a new entry.
 */
export function navigate(target: string, options: { replace?: boolean; state?: unknown } = {}): void {
  const next = normalizeTarget(target);
  if (next === window.location.hash) return;
  if (options.replace) {
    window.history.replaceState(window.history.state, "", urlFor(next));
  } else {
    window.history.pushState(options.state ?? null, "", urlFor(next));
  }
  // pushState and replaceState don't fire hashchange on their own.
  window.dispatchEvent(new HashChangeEvent("hashchange"));
}

/** The current hash with some query parameters changed; `null` removes one. */
function withParams(updates: Record<string, string | number | null | undefined>): string {
  const route = parseRoute(window.location.hash);
  const params = new URLSearchParams(route.params);
  for (const [key, value] of Object.entries(updates)) {
    if (value === null || value === undefined || value === "") params.delete(key);
    else params.set(key, String(value));
  }
  const path = `/${route.segments.map(encodeURIComponent).join("/")}`;
  const qs = params.toString();
  return `#${path}${qs ? `?${qs}` : ""}`;
}

/**
 * Change query parameters of the current route, keeping the path. `null`
 * removes a parameter. Filters replace history by default so the back
 * button leaves the screen instead of stepping through filter changes.
 */
export function updateParams(
  updates: Record<string, string | number | null | undefined>,
  options: { replace?: boolean } = { replace: true },
): void {
  navigate(withParams(updates), options);
}

/** Marks history entries created by opening a detail sheet. */
const SHEET_ENTRY = "szalinskiSheet";

function openedBySheet(): boolean {
  const current = window.history.state as Record<string, unknown> | null;
  return Boolean(current && typeof current === "object" && current[SHEET_ENTRY]);
}

/**
 * Open a detail sheet (`?job=` or `?file=`) as a new history entry, so the
 * back button (and Android's) closes it.
 */
export function openSheet(updates: Record<string, string | null>): void {
  navigate(withParams(updates), { state: { [SHEET_ENTRY]: true } });
}

/**
 * Close a detail sheet. A sheet opened here is closed by going back, so no
 * duplicate entry is left behind; one opened from a link or a reload has
 * its parameters removed instead.
 */
export function closeSheet(keys: string[]): void {
  if (openedBySheet()) {
    window.history.back();
    return;
  }
  updateParams(Object.fromEntries(keys.map((k) => [k, null])));
}

/** Test hook: forget router state between tests. */
export function resetRouterForTests(): void {
  state.rendered = null;
  state.guard = null;
  state.pending = null;
  state.version = 0;
}
