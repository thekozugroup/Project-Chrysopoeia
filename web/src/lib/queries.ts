"use client";

/**
 * TanStack Query hooks for every read endpoint, plus the query keys the
 * WebSocket client uses to patch caches in place.
 */

import { keepPreviousData, useQueries, useQuery, type QueryClient } from "@tanstack/react-query";
import { api, ApiError } from "./api";
import { settlingCount } from "./convertible";
import { isDetecting } from "./hardware";
import {
  NO_FAILURES,
  countFailures,
  failureGroup,
  keptAsConverted,
  setupProblem,
  type FailureCounts,
  type SetupProblem,
} from "./outcomes";
import type {
  FileQuery,
  FileStatus,
  HardwareInfo,
  Job,
  JobQuery,
  Library,
  LibraryStats,
  MediaFile,
  SystemInfo,
} from "./types";

/** Query keys. Lists take their parameters as the last element. */
export const keys = {
  health: ["health"] as const,
  system: ["system"] as const,
  overview: ["overview"] as const,
  libraries: ["libraries"] as const,
  files: (query?: FileQuery) => (query ? (["files", query] as const) : (["files"] as const)),
  file: (id: string) => ["file", id] as const,
  jobs: (query?: JobQuery) => (query ? (["jobs", query] as const) : (["jobs"] as const)),
  job: (id: string) => ["job", id] as const,
  queue: ["queue"] as const,
  settings: ["settings"] as const,
  hardware: ["hardware"] as const,
  presets: ["presets"] as const,
  browse: (path: string | undefined) => ["browse", path ?? ""] as const,
  activity: ["activity"] as const,
};

/** Retry transient failures a few times, but never client errors (4xx). */
export function shouldRetry(failureCount: number, error: unknown): boolean {
  if (error instanceof ApiError && error.status >= 400 && error.status < 500) return false;
  return failureCount < 2;
}

/**
 * Facts about the server (`GET /api/system`), e.g. the automatic work folder.
 * `null` when the server is too old to have the endpoint (404), so callers
 * can fall back to generic wording instead of showing an error.
 */
export async function fetchSystem(signal?: AbortSignal): Promise<SystemInfo | null> {
  try {
    return await api.system(signal);
  } catch (err) {
    if (err instanceof ApiError && (err.status === 404 || err.status === 405)) return null;
    throw err;
  }
}

export function useSystem() {
  return useQuery({
    queryKey: keys.system,
    queryFn: ({ signal }) => fetchSystem(signal),
    staleTime: 10 * 60_000,
  });
}

export function useOverview() {
  return useQuery({ queryKey: keys.overview, queryFn: ({ signal }) => api.overview(signal) });
}

export function useLibraries() {
  return useQuery({ queryKey: keys.libraries, queryFn: ({ signal }) => api.libraries(signal) });
}

/** One library, read from the shared list so every view agrees. */
export function useLibrary(id: string | undefined): {
  library: Library | undefined;
  isPending: boolean;
  error: Error | null;
} {
  const query = useLibraries();
  return {
    library: id ? query.data?.find((l) => l.id === id) : undefined,
    isPending: query.isPending,
    error: query.error,
  };
}

export function useFiles(query: FileQuery) {
  return useQuery({
    queryKey: keys.files(query),
    queryFn: ({ signal }) => api.files(query, signal),
    // Keep the old page on screen while filtering, but never show another
    // library's files while switching libraries.
    placeholderData: (previous, previousQuery) => {
      const prevQuery = previousQuery?.queryKey[1] as FileQuery | undefined;
      return prevQuery?.library === query.library ? previous : undefined;
    },
  });
}

/**
 * One file with its probe and jobs. `active: false` stops fetching but keeps
 * returning what is cached (a sheet showing its last file while it closes).
 */
export function useFile(id: string | null, active = true) {
  return useQuery({
    queryKey: keys.file(id ?? ""),
    queryFn: ({ signal }) => api.file(id ?? "", signal),
    enabled: Boolean(id) && active,
  });
}

export function useJobs(query: JobQuery, options: { enabled?: boolean } = {}) {
  return useQuery({
    queryKey: keys.jobs(query),
    queryFn: ({ signal }) => api.jobs(query, signal),
    placeholderData: keepPreviousData,
    enabled: options.enabled ?? true,
  });
}

/** One job. `active` works as for `useFile`. */
export function useJob(id: string | null, active = true) {
  return useQuery({
    queryKey: keys.job(id ?? ""),
    queryFn: ({ signal }) => api.job(id ?? "", signal),
    enabled: Boolean(id) && active,
  });
}

export function useQueueState() {
  return useQuery({ queryKey: keys.queue, queryFn: ({ signal }) => api.queue(signal) });
}

export function useSettings() {
  return useQuery({ queryKey: keys.settings, queryFn: ({ signal }) => api.settings(signal) });
}

/**
 * Hardware info. While the server runs its first detection it answers with
 * a "Checking your hardware…" stand-in (or 503 behind some proxies); keep
 * polling quietly until the real results are in.
 */
export function useHardware() {
  return useQuery({
    queryKey: keys.hardware,
    queryFn: ({ signal }) => api.hardware(signal),
    staleTime: 5 * 60_000,
    refetchInterval: (query) => (query.state.error || isDetecting(query.state.data) ? 2500 : false),
  });
}

/**
 * Hardware info for display: `hw` is only set once detection has finished,
 * so a stand-in is never shown as "ffmpeg wasn't found". `pending` covers
 * loading, the first detection and a server that isn't ready yet.
 */
export function useHardwareInfo(): {
  hw: HardwareInfo | undefined;
  pending: boolean;
  detecting: boolean;
  error: Error | null;
} {
  const query = useHardware();
  const detecting = isDetecting(query.data);
  const hw = detecting ? undefined : query.data;
  return {
    hw,
    detecting,
    pending: !hw && (query.isPending || detecting || Boolean(query.error)),
    error: hw ? null : query.error,
  };
}

export function usePresets() {
  return useQuery({
    queryKey: keys.presets,
    queryFn: ({ signal }) => api.presets(signal),
    staleTime: 10 * 60_000,
  });
}

export function useBrowse(path: string | undefined, enabled = true) {
  return useQuery({
    queryKey: keys.browse(path),
    queryFn: ({ signal }) => api.browse(path, signal),
    enabled,
    staleTime: 30_000,
    placeholderData: keepPreviousData,
  });
}

/** The failed files of every library, to tell damaged originals from failed conversions. */
const FAILED_QUERY: FileQuery = { status: "failed", limit: 500, sort: "name" };

/** Failed files split by cause, overall and per library. */
export interface Failures {
  /** False until the failed files have loaded (counts are 0 meanwhile). */
  ready: boolean;
  total: FailureCounts;
  byLibrary: Record<string, FailureCounts>;
  /** Failed files whose original can't be read. */
  unreadable: MediaFile[];
  /**
   * Failed files waiting on a fix in the setup, by cause, across every
   * library: the work folder, the destination, free space or the chosen
   * hardware. Fixed once, then all tried again.
   */
  setup: Partial<Record<SetupProblem, MediaFile[]>>;
  /** Failed files that changed while they were being converted. */
  changed: MediaFile[];
  /** Ids of failed files worth trying again (not damaged originals), by library. */
  retryIds: Record<string, string[]>;
}

/** Group failed files by cause (see `failureGroup`); `total` as for `countFailures`. */
export function failuresFrom(libraries: Library[], items: MediaFile[], cutShort: boolean): Failures {
  const byLibrary: Record<string, FailureCounts> = {};
  const retryIds: Record<string, string[]> = {};
  for (const library of libraries) {
    const own = items.filter((f) => f.library_id === library.id);
    // A cut-short list counts the rest as conversion failures.
    byLibrary[library.id] = countFailures(own, cutShort ? Math.max(library.stats.failed, own.length) : own.length);
    retryIds[library.id] = own.filter((f) => failureGroup(f) !== "unreadable").map((f) => f.id);
  }
  const total = Object.values(byLibrary).reduce(
    (sum, c) => ({
      unreadable: sum.unreadable + c.unreadable,
      conversion: sum.conversion + c.conversion,
      setup: sum.setup + c.setup,
      changed: sum.changed + c.changed,
    }),
    NO_FAILURES,
  );
  const setup: Partial<Record<SetupProblem, MediaFile[]>> = {};
  for (const file of items) {
    const kind = setupProblem(file);
    if (kind) (setup[kind] ??= []).push(file);
  }
  return {
    ready: true,
    total,
    byLibrary,
    unreadable: items.filter((f) => failureGroup(f) === "unreadable"),
    setup,
    changed: items.filter((f) => failureGroup(f) === "changed"),
    retryIds,
  };
}

/**
 * Why files failed. `stats.failed` counts every kind alike; this reads the
 * failed files themselves (only when some library has any) so each cause
 * gets its own wording and action.
 */
export function useFailures(): Failures {
  const libraries = useLibraries();
  const anyFailed = Boolean(libraries.data?.some((l) => l.stats.failed > 0));
  const query = useQuery({
    queryKey: keys.files(FAILED_QUERY),
    queryFn: ({ signal }) => api.files(FAILED_QUERY, signal),
    enabled: anyFailed,
  });
  const items = anyFailed ? (query.data?.items ?? []) : [];
  const cutShort = Boolean(query.data && query.data.total > query.data.items.length);
  return { ...failuresFrom(libraries.data ?? [], items, cutShort), ready: !anyFailed || Boolean(query.data) };
}

export function useActivity(options: { enabled?: boolean } = {}) {
  return useQuery({
    queryKey: keys.activity,
    queryFn: ({ signal }) => api.activity(30, signal),
    enabled: options.enabled ?? true,
  });
}

/**
 * Files of a library still being copied in: the server's `stats.settling`,
 * or, only from servers too old to send it, read from the latest scan
 * summary in the activity feed (which is then fetched).
 */
export function useSettling(library: Pick<Library, "id" | "stats">): number {
  const known = typeof library.stats.settling === "number";
  const activity = useActivity({ enabled: !known });
  return settlingCount(library, known ? undefined : activity.data?.items);
}

/**
 * Which of these finished jobs were a second conversion that left an
 * already converted file as it was (see `keptAsConverted`). Only skipped,
 * failed and stopped jobs can be; their files are read (and cached with the
 * file sheet's data) to tell.
 */
export function useKeptAsConverted(jobs: Job[]): Set<string> {
  const candidates = jobs.filter((j) => j.state === "skipped" || j.state === "failed" || j.state === "cancelled");
  const fileIds = [...new Set(candidates.map((j) => j.file_id))].slice(0, 50);
  const details = useQueries({
    queries: fileIds.map((id) => ({
      queryKey: keys.file(id),
      queryFn: ({ signal }: { signal: AbortSignal }) => api.file(id, signal),
      staleTime: 60_000,
    })),
  });
  const byFile = new Map(fileIds.map((id, i) => [id, details[i]?.data]));
  return new Set(candidates.filter((j) => keptAsConverted(j, byFile.get(j.file_id))).map((j) => j.id));
}

/** Statuses a file that saved space can have: converted, or queued again after that. */
const SAVED_STATUSES: FileStatus[] = ["done", "queued", "processing"];
const SAVED_PAGE = 500;
/** At most this many files are read per status to count them. */
const SAVED_MAX_PER_STATUS = 5_000;

/**
 * Count the files that saved space (`saved_bytes > 0`): converted files,
 * and converted files queued again (they keep their savings); files that
 * came out larger ("Convert anyway") don't count. Converted files are read
 * page by page (in a very large library, those past the first 5,000 are
 * taken to have saved space, as nearly all do); queued ones only when the
 * converted files don't account for all of `totals.saved_bytes`.
 */
export async function countSavedFiles(totals: LibraryStats, signal?: AbortSignal): Promise<number> {
  let files = 0;
  let savedSeen = 0;
  for (const status of SAVED_STATUSES) {
    // Nothing else holds savings once the converted files add up to the total.
    if (status !== "done" && savedSeen >= totals.saved_bytes) break;
    for (let offset = 0; ; offset += SAVED_PAGE) {
      const page = await api.files({ status, limit: SAVED_PAGE, offset, sort: "name" }, signal);
      for (const file of page.items) {
        if (file.saved_bytes === null) continue;
        savedSeen += file.saved_bytes;
        if (file.saved_bytes > 0) files += 1;
      }
      const read = offset + page.items.length;
      if (read >= page.total || page.items.length === 0) break;
      if (read >= SAVED_MAX_PER_STATUS) {
        if (status === "done") {
          files += page.total - read;
          savedSeen = totals.saved_bytes;
        }
        break;
      }
    }
  }
  return files;
}

/**
 * How many files the space saved comes from (see `countSavedFiles`). Read
 * again only when the totals change.
 */
export function useSavedFileCount(totals: LibraryStats | undefined) {
  return useQuery({
    queryKey: [
      "saved-files",
      totals?.done ?? 0,
      totals?.queued ?? 0,
      totals?.processing ?? 0,
      totals?.saved_bytes ?? 0,
    ] as const,
    queryFn: ({ signal }) => countSavedFiles(totals as LibraryStats, signal),
    enabled: Boolean(totals && totals.saved_bytes > 0),
    staleTime: Infinity,
    placeholderData: keepPreviousData,
  });
}

/** Refresh everything that depends on files and jobs after a user action. */
export function invalidateWork(client: QueryClient): void {
  void client.invalidateQueries({ queryKey: keys.files() });
  void client.invalidateQueries({ queryKey: keys.jobs() });
  void client.invalidateQueries({ queryKey: keys.libraries });
  void client.invalidateQueries({ queryKey: keys.overview });
  void client.invalidateQueries({ queryKey: keys.queue });
  void client.invalidateQueries({ queryKey: ["file"] });
}
