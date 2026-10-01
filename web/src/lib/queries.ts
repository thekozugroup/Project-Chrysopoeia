"use client";

/**
 * TanStack Query hooks for every read endpoint, plus the query keys the
 * WebSocket client uses to patch caches in place.
 */

import { keepPreviousData, useQueries, useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useMemo } from "react";
import { api, ApiError } from "./api";
import { isDetecting } from "./hardware";
import {
  NO_FAILURES,
  countFailures,
  failureGroup,
  jobStanding,
  newFileName,
  setupProblem,
  type FailureCounts,
  type JobStanding,
  type SetupProblem,
} from "./outcomes";
import type {
  BulkRequest,
  FileDetail,
  FileQuery,
  FileStatus,
  HardwareInfo,
  Job,
  JobQuery,
  Library,
  LibraryStats,
  MediaFile,
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
  /** Every failed file, grouped by cause (see `useFailures`). */
  failures: ["failures"] as const,
  /** A file's status and latest jobs, to read what its finished jobs mean now (see `useJobStandings`). */
  fileJobs: (id: string) => ["file-jobs", id] as const,
};

/** Retry transient failures a few times, but never client errors (4xx). */
export function shouldRetry(failureCount: number, error: unknown): boolean {
  if (error instanceof ApiError && error.status >= 400 && error.status < 500) return false;
  return failureCount < 2;
}

/** Facts about the server (`GET /api/system`), e.g. the automatic work folder. */
export function useSystem() {
  return useQuery({
    queryKey: keys.system,
    queryFn: ({ signal }) => api.system(signal),
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
 * The name a finished job gave its file when it isn't the one the job
 * started with (see `newFileName`), for a sheet's title. The job says it
 * itself; the file is only read for a job from a server that doesn't.
 */
export function useNewFileName(job: Job | undefined): string | null {
  const settings = useSettings();
  const needsFile = job !== undefined && job.state === "done" && job.output_name === undefined;
  const file = useFile(job?.file_id ?? null, needsFile);
  return job ? newFileName(job, file.data?.file, settings.data?.output_mode) : null;
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

/** Failed files are read in pages of this many… */
const FAILED_PAGE = 500;
/** …up to this many in all. Past it, the rest are counted but not sorted by cause. */
export const FAILED_READ_MAX = 5_000;

/**
 * Every failed file (up to `FAILED_READ_MAX`), page by page, so problems
 * are grouped and tried again by what really failed, not by one page of
 * them. `total` is how many failed files the server has.
 */
export async function fetchFailedFiles(signal?: AbortSignal): Promise<{ items: MediaFile[]; total: number }> {
  const byId = new Map<string, MediaFile>();
  let total = 0;
  for (let offset = 0; offset < FAILED_READ_MAX; offset += FAILED_PAGE) {
    const page = await api.files({ status: "failed", limit: FAILED_PAGE, offset, sort: "name" }, signal);
    total = page.total;
    // A file that moved between pages while they were read is kept once.
    for (const file of page.items) byId.set(file.id, file);
    if (offset + page.items.length >= page.total || page.items.length < FAILED_PAGE) break;
  }
  return { items: [...byId.values()], total: Math.max(total, byId.size) };
}

/** Failed files split by cause, overall and per library. */
export interface Failures {
  /** False until the failed files have loaded (counts are 0 meanwhile). */
  ready: boolean;
  /**
   * Every failed file was read. False past `FAILED_READ_MAX`: the rest
   * are counted in `unsorted` (and as failed conversions in the counts),
   * and "Try again" selects by status instead of by file.
   */
  complete: boolean;
  total: FailureCounts;
  byLibrary: Record<string, FailureCounts>;
  /** Failed files not read, by library (only when `complete` is false). */
  unsorted: Record<string, number>;
  /** Failed files whose original can't be read. */
  unreadable: MediaFile[];
  /**
   * Failed files waiting on a fix in the setup, by cause, across every
   * library: the work folder, the destination, free space or the chosen
   * hardware. Fixed once, then all tried again.
   */
  setup: Partial<Record<SetupProblem, MediaFile[]>>;
  /** Failed files that were moved or changed while they were being converted. */
  changed: MediaFile[];
  /** Ids of every failed file read. */
  failedIds: Set<string>;
  /**
   * "Try again" for a library's failed files that aren't damaged
   * originals: the request, and how many files it's for. `null` when
   * there are none.
   */
  retry: Record<string, { request: BulkRequest; count: number } | null>;
}

/**
 * Group failed files by cause (see `failureGroup`). `total` is how many
 * failed files the server has; past the files read, each library's
 * remaining failed files (its `stats.failed`) are `unsorted`.
 */
export function failuresFrom(libraries: Library[], items: MediaFile[], total = items.length): Failures {
  const complete = total <= items.length;
  const byLibrary: Record<string, FailureCounts> = {};
  const unsorted: Record<string, number> = {};
  const retry: Failures["retry"] = {};
  for (const library of libraries) {
    const own = items.filter((f) => f.library_id === library.id);
    const rest = complete ? 0 : Math.max(0, library.stats.failed - own.length);
    unsorted[library.id] = rest;
    // Files not read count as failed conversions, the cautious reading.
    byLibrary[library.id] = countFailures(own, own.length + rest);
    const ids = own.filter((f) => failureGroup(f) !== "unreadable").map((f) => f.id);
    retry[library.id] =
      rest > 0
        ? { request: { action: "retry_failed", library: library.id }, count: ids.length + rest }
        : ids.length
          ? { request: { action: "retry_failed", ids }, count: ids.length }
          : null;
  }
  const sum = Object.values(byLibrary).reduce(
    (acc, c) => ({
      unreadable: acc.unreadable + c.unreadable,
      conversion: acc.conversion + c.conversion,
      setup: acc.setup + c.setup,
      changed: acc.changed + c.changed,
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
    complete,
    total: sum,
    byLibrary,
    unsorted,
    unreadable: items.filter((f) => failureGroup(f) === "unreadable"),
    setup,
    changed: items.filter((f) => failureGroup(f) === "changed"),
    failedIds: new Set(items.map((f) => f.id)),
    retry,
  };
}

/**
 * Why files failed. `stats.failed` counts every kind alike; this reads the
 * failed files themselves (only when some library has any) so each cause
 * gets its own wording and action. They're read again when a library's
 * failed count changes (and after the user's own actions), not on every
 * refresh of the file lists, so a big list isn't read over and over while
 * files convert.
 */
export function useFailures(): Failures {
  const libraries = useLibraries();
  const failedCounts = (libraries.data ?? [])
    .filter((l) => l.stats.failed > 0)
    .map((l) => `${l.id}:${l.stats.failed}`)
    .join(",");
  const anyFailed = failedCounts !== "";
  const query = useQuery({
    queryKey: [...keys.failures, failedCounts] as const,
    queryFn: ({ signal }) => fetchFailedFiles(signal),
    enabled: anyFailed,
    staleTime: 30_000,
    // Keep the last grouping on screen while the counts move.
    placeholderData: keepPreviousData,
  });
  const data = anyFailed ? query.data : undefined;
  const grouped = useMemo(
    () => failuresFrom(libraries.data ?? [], data?.items ?? [], data?.total ?? 0),
    [libraries.data, data],
  );
  return { ...grouped, ready: !anyFailed || Boolean(data) };
}

export function useActivity(options: { enabled?: boolean } = {}) {
  return useQuery({
    queryKey: keys.activity,
    queryFn: ({ signal }) => api.activity(30, signal),
    enabled: options.enabled ?? true,
  });
}

/** At most this many files are read per list to tell where its jobs stand. */
const STANDING_FILES_MAX = 50;

const FINISHED_UNCONVERTED: Job["state"][] = ["skipped", "failed", "cancelled"];

/**
 * Where each job of a list of finished jobs stands now (see
 * `jobStanding`): a second conversion that left a converted file as it
 * was, an old outcome the file has moved past, or the job's own outcome.
 * Only skipped, failed and stopped jobs can be anything but `current`. A
 * failed job whose file is still failed is `current` without asking; the
 * other jobs' files are read (at most 50 per list), reusing what a file
 * sheet already loaded. They're read again when a job or file event names
 * that file, or after a minute, never with every refresh of the lists.
 */
export function useJobStandings(jobs: Job[]): Map<string, JobStanding> {
  const client = useQueryClient();
  const failures = useFailures();
  const candidates = jobs.filter((j) => FINISHED_UNCONVERTED.includes(j.state));
  const stillFailed = (job: Job) => job.state === "failed" && failures.failedIds.has(job.file_id);
  // Until the failed files are known, a failed job's file isn't read: it most likely is one.
  const toRead = candidates.filter((j) => !stillFailed(j) && (failures.ready || j.state !== "failed"));
  const fileIds = [...new Set(toRead.map((j) => j.file_id))].slice(0, STANDING_FILES_MAX);
  const details = useQueries({
    queries: fileIds.map((id) => ({
      queryKey: keys.fileJobs(id),
      queryFn: ({ signal }: { signal: AbortSignal }) => api.file(id, signal),
      staleTime: 60_000,
      initialData: () => client.getQueryData<FileDetail>(keys.file(id)),
      initialDataUpdatedAt: () => client.getQueryState(keys.file(id))?.dataUpdatedAt,
    })),
  });
  const byFile = new Map(fileIds.map((id, i) => [id, details[i]]));
  const standings = new Map<string, JobStanding>();
  for (const job of candidates) {
    if (stillFailed(job)) {
      standings.set(job.id, "current");
      continue;
    }
    const detail = byFile.get(job.file_id);
    // A file that can't be read (gone, or past the files read): the job's own outcome.
    if (!detail && failures.ready) standings.set(job.id, "current");
    else if (detail?.isError && !detail.data) standings.set(job.id, "current");
    else standings.set(job.id, jobStanding(job, detail?.data));
  }
  return standings;
}

/** The recent history read for second conversions a setup problem stopped. */
const KEPT_SETUP_QUERY: JobQuery = { state: "history", limit: 50, offset: 0 };

/** A failed job whose problem is in the setup, and whose file isn't failed: a candidate for "kept". */
function keptSetupCandidate(job: Job, failures: Failures): boolean {
  return job.state === "failed" && setupProblem(job) !== null && failures.ready && !failures.failedIds.has(job.file_id);
}

/**
 * Converted files whose second conversion a setup problem stopped (a work
 * folder that can't be used, a full disk…), by cause. They stay converted,
 * so they're not among the failed files; without this a broken setup would
 * look healthy while every "Convert again" fails. Read from the recent
 * history: each file's latest job, when it failed on a setup problem and
 * left the file as it was converted. The history is only read when the
 * latest results (`recent`) show such a job, or failed files show a setup
 * problem (its "Try again" should cover these files too).
 */
export function useKeptSetupFailures(recent: Job[]): Partial<Record<SetupProblem, Job[]>> {
  const failures = useFailures();
  const hint =
    recent.some((j) => keptSetupCandidate(j, failures)) ||
    Object.values(failures.setup).some((files) => files && files.length > 0);
  const history = useJobs(KEPT_SETUP_QUERY, { enabled: hint });
  const seen = new Set<string>();
  const latest: Job[] = [];
  for (const job of hint ? (history.data?.items ?? recent) : []) {
    if (seen.has(job.file_id)) continue;
    seen.add(job.file_id);
    if (keptSetupCandidate(job, failures)) latest.push(job);
  }
  const standings = useJobStandings(latest);
  const kept: Partial<Record<SetupProblem, Job[]>> = {};
  for (const job of latest) {
    const kind = setupProblem(job);
    if (kind && standings.get(job.id) === "kept") (kept[kind] ??= []).push(job);
  }
  return kept;
}

/** The latest results, as the overview lists them (also read for setup problems, see `useSetupProblems`). */
export const RECENT_RESULTS_QUERY: JobQuery = { state: "history", limit: 6 };
const NO_JOBS: Job[] = [];

/**
 * Setup problems that stop conversions, from every place they show: failed
 * files waiting on a fix (work folder, destination, disk space, chosen
 * hardware), converted files whose second conversion one stopped (see
 * `useKeptSetupFailures`), and hardware detection's errors (nothing can be
 * converted until they're fixed). `open` when there's any: the overview's
 * status and the queue pill in the frame then say a fix is needed instead
 * of "All caught up".
 */
export function useSetupProblems(): { kept: Partial<Record<SetupProblem, Job[]>>; open: boolean } {
  const failures = useFailures();
  const recent = useJobs(RECENT_RESULTS_QUERY);
  const { hw } = useHardwareInfo();
  const kept = useKeptSetupFailures(recent.data?.items ?? NO_JOBS);
  const open =
    Object.values(failures.setup).some((files) => files && files.length > 0) ||
    Object.values(kept).some((jobs) => jobs && jobs.length > 0) ||
    Boolean(hw?.hints.some((h) => h.level === "error"));
  return { kept, open };
}

/** Statuses a file holding a conversion's result can have: converted, or queued again after that. */
const SAVED_STATUSES: FileStatus[] = ["done", "queued", "processing"];
const SAVED_PAGE = 500;
/** At most this many files are read per status to count them. */
const SAVED_MAX_PER_STATUS = 5_000;

/** The converted files the space saved is added up from, and how many of them came out larger. */
export interface SavedFiles {
  files: number;
  larger: number;
}

/**
 * Count the converted files the space saved is added up from: every file
 * with a result (`saved_bytes` set), including converted files queued
 * again (they keep their savings), and, among them, the ones that came out
 * larger ("Convert anyway"), which take away from the total. The total is
 * the net sum, so the count matches it: "from 5 converted files (1 came out
 * larger)", beside "Converted 5". Converted files are read page by page
 * (in a very large library, those past the first 5,000 are counted without
 * being read); queued ones only when the converted files don't account for
 * all of `totals.saved_bytes`.
 */
export async function countSavedFiles(totals: LibraryStats, signal?: AbortSignal): Promise<SavedFiles> {
  let files = 0;
  let larger = 0;
  let savedSeen = 0;
  for (const status of SAVED_STATUSES) {
    // Nothing else holds savings once the converted files add up to the total.
    if (status !== "done" && savedSeen >= totals.saved_bytes) break;
    for (let offset = 0; ; offset += SAVED_PAGE) {
      const page = await api.files({ status, limit: SAVED_PAGE, offset, sort: "name" }, signal);
      for (const file of page.items) {
        if (file.saved_bytes === null) continue;
        savedSeen += file.saved_bytes;
        files += 1;
        if (file.saved_bytes < 0) larger += 1;
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
  return { files, larger };
}

/**
 * How many files the space saved comes from (see `countSavedFiles`). Read
 * again only when the space saved or the converted count changes, not
 * every time a job starts or finishes.
 */
export function useSavedFileCount(totals: LibraryStats | undefined) {
  return useQuery({
    queryKey: ["saved-files", totals?.done ?? 0, totals?.saved_bytes ?? 0] as const,
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
  void client.invalidateQueries({ queryKey: keys.failures });
}
