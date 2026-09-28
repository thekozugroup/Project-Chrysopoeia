/**
 * REST client for the Chrysopoeia server (`/api`, see docs/ARCHITECTURE.md).
 *
 * The UI is served by the same binary, so requests go to the same origin by
 * default. `NEXT_PUBLIC_API_URL` (baked in at build time) points a dev build
 * at another server, e.g. `http://localhost:8080` or `http://nas:8080/api`.
 */

import type {
  Affected,
  BulkRequest,
  CreateLibraryRequest,
  FileDetail,
  FileQuery,
  FsBrowse,
  HardwareInfo,
  Health,
  Job,
  JobQuery,
  Library,
  ListResponse,
  MediaFile,
  Overview,
  Presets,
  QueueState,
  Settings,
  UpdateLibraryRequest,
  ActivityEntry,
} from "./types";

/** Base URL of the REST API, without a trailing slash. Always ends in `/api`. */
export function apiBase(): string {
  const configured = process.env.NEXT_PUBLIC_API_URL?.trim();
  if (!configured) return "/api";
  const trimmed = configured.replace(/\/+$/, "");
  return trimmed.endsWith("/api") ? trimmed : `${trimmed}/api`;
}

/** WebSocket URL for live events, derived from the API base. */
export function wsUrl(): string {
  const base = apiBase();
  if (/^https?:\/\//.test(base)) {
    return `${base.replace(/^http/, "ws")}/ws`;
  }
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${window.location.host}${base}/ws`;
}

/**
 * An error from the API. `code` is the server's snake_case error code
 * (`path_not_found`, `library_exists`, ...), or one of the client-side codes
 * `network_error`, `bad_response` and `http_<status>`.
 */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
  }

  /** True when the server could not be reached at all. */
  get isNetwork(): boolean {
    return this.status === 0;
  }

  /** True for gateway errors a reverse proxy returns while the app restarts. */
  get isUnavailable(): boolean {
    return this.isNetwork || this.status === 502 || this.status === 503 || this.status === 504;
  }
}

/** Plain-language message for any thrown value. */
export function errorMessage(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  if (error instanceof Error && error.message) return error.message;
  return "Something went wrong. Please try again.";
}

const FALLBACK_MESSAGES: Record<number, string> = {
  400: "The server didn't accept that request.",
  403: "That folder is outside the folders Chrysopoeia is allowed to show.",
  404: "That item no longer exists. It may have been removed.",
  409: "That conflicts with something that's already happening.",
  500: "The server ran into a problem. Check the container logs for details.",
  502: "The server isn't answering. It may be restarting.",
  503: "The server isn't ready yet. It may still be starting up.",
  504: "The server took too long to answer.",
};

type Query = Record<string, string | number | boolean | null | undefined>;

function withQuery(path: string, query?: Query): string {
  if (!query) return path;
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === "") continue;
    params.set(key, String(value));
  }
  const qs = params.toString();
  return qs ? `${path}?${qs}` : path;
}

async function parseError(res: Response): Promise<ApiError> {
  let code = `http_${res.status}`;
  let message = FALLBACK_MESSAGES[res.status] ?? `The server answered with an error (${res.status}).`;
  try {
    const body: unknown = await res.json();
    if (body && typeof body === "object") {
      const record = body as Record<string, unknown>;
      if (typeof record.error === "string" && record.error) message = record.error;
      if (typeof record.code === "string" && record.code) code = record.code;
    }
  } catch {
    // Not JSON (e.g. a proxy error page); keep the fallback message.
  }
  return new ApiError(res.status, code, message);
}

interface RequestOptions {
  method?: "GET" | "POST" | "PATCH" | "DELETE";
  body?: unknown;
  query?: Query;
  signal?: AbortSignal;
}

/** Perform one API request and decode the JSON body. */
export async function request<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const { method = "GET", body, query, signal } = options;
  const headers: Record<string, string> = { Accept: "application/json" };
  if (body !== undefined) headers["Content-Type"] = "application/json";

  let res: Response;
  try {
    res = await fetch(`${apiBase()}${withQuery(path, query)}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      signal,
      cache: "no-store",
    });
  } catch (err) {
    if (err instanceof DOMException && err.name === "AbortError") throw err;
    throw new ApiError(0, "network_error", "Can't reach the Chrysopoeia server.");
  }

  if (!res.ok) throw await parseError(res);
  if (res.status === 204) return undefined as T;

  const text = await res.text();
  if (!text) return undefined as T;
  try {
    return JSON.parse(text) as T;
  } catch {
    throw new ApiError(res.status, "bad_response", "The server sent a response the app couldn't read.");
  }
}

/** A path segment, encoded so an id from the address bar can't reach another endpoint. */
function seg(value: string): string {
  return encodeURIComponent(value);
}

/** Whether a string looks like a UUID (ids in `?job=` and `?file=` links). */
export function isUuid(value: string | null | undefined): value is string {
  return Boolean(value && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value));
}

/** Typed endpoint helpers, one per route in docs/ARCHITECTURE.md. */
export const api = {
  health: (signal?: AbortSignal) => request<Health>("/health", { signal }),
  overview: (signal?: AbortSignal) => request<Overview>("/overview", { signal }),

  libraries: (signal?: AbortSignal) => request<Library[]>("/libraries", { signal }),
  library: (id: string, signal?: AbortSignal) => request<Library>(`/libraries/${seg(id)}`, { signal }),
  createLibrary: (body: CreateLibraryRequest) =>
    request<Library>("/libraries", { method: "POST", body }),
  updateLibrary: (id: string, body: UpdateLibraryRequest) =>
    request<Library>(`/libraries/${seg(id)}`, { method: "PATCH", body }),
  deleteLibrary: (id: string) => request<void>(`/libraries/${seg(id)}`, { method: "DELETE" }),
  scanLibrary: (id: string) =>
    request<{ started: boolean }>(`/libraries/${seg(id)}/scan`, { method: "POST" }),
  scanAll: () => request<{ started: boolean }>("/scan", { method: "POST" }),

  files: (query: FileQuery, signal?: AbortSignal) =>
    request<ListResponse<MediaFile>>("/files", { query: { ...query }, signal }),
  file: (id: string, signal?: AbortSignal) => request<FileDetail>(`/files/${seg(id)}`, { signal }),
  queueFile: (id: string, priority?: number) =>
    request<Job>(`/files/${seg(id)}/queue`, {
      method: "POST",
      body: priority === undefined ? {} : { priority },
    }),
  skipFile: (id: string) => request<MediaFile>(`/files/${seg(id)}/skip`, { method: "POST" }),
  bulk: (body: BulkRequest) => request<Affected>("/files/bulk", { method: "POST", body }),

  jobs: (query: JobQuery, signal?: AbortSignal) =>
    request<ListResponse<Job>>("/jobs", { query: { ...query }, signal }),
  job: (id: string, signal?: AbortSignal) => request<Job>(`/jobs/${seg(id)}`, { signal }),
  cancelJob: (id: string) => request<Job>(`/jobs/${seg(id)}/cancel`, { method: "POST" }),
  moveJobToTop: (id: string) =>
    request<Job>(`/jobs/${seg(id)}/priority`, { method: "POST", body: { move: "top" } }),
  setJobPriority: (id: string, priority: number) =>
    request<Job>(`/jobs/${seg(id)}/priority`, { method: "POST", body: { priority } }),
  clearHistory: () =>
    request<Affected>("/jobs/clear", { method: "POST", body: { state: "history" } }),

  queue: (signal?: AbortSignal) => request<QueueState>("/queue", { signal }),
  pauseQueue: () => request<QueueState>("/queue/pause", { method: "POST" }),
  resumeQueue: () => request<QueueState>("/queue/resume", { method: "POST" }),
  stopQueue: () => request<QueueState>("/queue/stop", { method: "POST" }),

  settings: (signal?: AbortSignal) => request<Settings>("/settings", { signal }),
  updateSettings: (patch: Partial<Settings>) =>
    request<Settings>("/settings", { method: "PATCH", body: patch }),

  hardware: (signal?: AbortSignal) => request<HardwareInfo>("/hardware", { signal }),
  detectHardware: () => request<HardwareInfo>("/hardware/detect", { method: "POST" }),

  presets: (signal?: AbortSignal) => request<Presets>("/presets", { signal }),
  browse: (path: string | undefined, signal?: AbortSignal) =>
    request<FsBrowse>("/fs/browse", { query: { path }, signal }),
  activity: (limit = 50, signal?: AbortSignal) =>
    request<{ items: ActivityEntry[] }>("/activity", { query: { limit }, signal }),
};
