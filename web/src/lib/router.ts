"use client";

/**
 * Tiny hash router. The Rust server only serves one `index.html`, so every
 * view lives in the URL hash (`#/library/<id>?status=failed`) and reloads or
 * shared links land on the same view.
 */

import { useMemo, useSyncExternalStore } from "react";

/** A parsed location: `#/library/abc?file=1` → `["library","abc"]` + params. */
export interface Route {
  segments: string[];
  params: URLSearchParams;
  /** The hash without the leading `#`, e.g. `/queue/history`. */
  raw: string;
}

function subscribe(onChange: () => void): () => void {
  window.addEventListener("hashchange", onChange);
  return () => window.removeEventListener("hashchange", onChange);
}

function snapshot(): string {
  return window.location.hash;
}

function serverSnapshot(): string {
  return "";
}

/** Parse a hash (with or without `#`) into a route. */
export function parseRoute(hash: string): Route {
  const raw = hash.replace(/^#/, "") || "/";
  const [pathPart, queryPart = ""] = raw.split("?", 2);
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

/** Go to a route. `replace` avoids adding a history entry (filters, tabs). */
export function navigate(target: string, options: { replace?: boolean } = {}): void {
  const next = target.startsWith("#") ? target : `#${target.startsWith("/") ? target : `/${target}`}`;
  if (next === window.location.hash) return;
  if (options.replace) {
    const url = `${window.location.pathname}${window.location.search}${next}`;
    window.history.replaceState(window.history.state, "", url);
    window.dispatchEvent(new HashChangeEvent("hashchange"));
  } else {
    window.location.hash = next;
  }
}

/**
 * Change query parameters of the current route, keeping the path. `null`
 * removes a parameter. Filters and sheets replace history by default so the
 * back button leaves the screen instead of stepping through filter changes.
 */
export function updateParams(
  updates: Record<string, string | number | null | undefined>,
  options: { replace?: boolean } = { replace: true },
): void {
  const route = parseRoute(window.location.hash);
  const params = new URLSearchParams(route.params);
  for (const [key, value] of Object.entries(updates)) {
    if (value === null || value === undefined || value === "") params.delete(key);
    else params.set(key, String(value));
  }
  const path = `/${route.segments.map(encodeURIComponent).join("/")}`;
  const qs = params.toString();
  navigate(`#${path}${qs ? `?${qs}` : ""}`, options);
}
