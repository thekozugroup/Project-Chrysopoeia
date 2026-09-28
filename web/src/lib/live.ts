"use client";

/**
 * WebSocket client for `/api/ws`. Events patch the TanStack Query cache in
 * place (so lists update without refetching) and feed the live store with
 * progress. The socket reconnects with exponential backoff and refetches
 * everything after a reconnect, because events sent while disconnected are
 * gone.
 *
 * When the socket stays down (a reverse proxy without WebSocket support is
 * the usual cause on Unraid), the app falls back to refreshing the live
 * views every few seconds until the socket opens again.
 */

import type { QueryClient, QueryKey } from "@tanstack/react-query";
import { onReachability, wsUrl } from "./api";
import { keys } from "./queries";
import { navigate, parseRoute } from "./router";
import { useLive } from "./store";
import type {
  ActivityEntry,
  FileDetail,
  Job,
  Library,
  ListResponse,
  MediaFile,
  Overview,
  QueueState,
  ServerEvent,
} from "./types";

const MIN_BACKOFF_MS = 1000;
/**
 * Longest wait between reconnect attempts. Short, because the server is on
 * the LAN and a restart should be picked up within seconds; a failed attempt
 * costs one refused connection.
 */
export const MAX_BACKOFF_MS = 8000;
/** How long the socket may be down before polling starts. */
export const POLL_GRACE_MS = 4000;
/** How often the live views refresh while polling. */
export const POLL_INTERVAL_MS = 5000;
/** Failed attempts, without ever connecting, before live updates count as unavailable. */
export const UNAVAILABLE_AFTER = 3;

/** Debounced invalidation, so bursts of events cause one refetch. */
export class Invalidator {
  private timers = new Map<string, ReturnType<typeof setTimeout>>();

  constructor(private client: QueryClient) {}

  schedule(queryKey: QueryKey, delayMs = 800): void {
    const id = JSON.stringify(queryKey);
    if (this.timers.has(id)) return;
    this.timers.set(
      id,
      setTimeout(() => {
        this.timers.delete(id);
        void this.client.invalidateQueries({ queryKey });
      }, delayMs),
    );
  }

  cancelAll(): void {
    for (const timer of this.timers.values()) clearTimeout(timer);
    this.timers.clear();
  }
}

/** Copy of an event without its `type` tag. */
function withoutType<T extends { type: string }>(event: T): Omit<T, "type"> {
  const copy: Partial<T> = { ...event };
  delete copy.type;
  return copy as Omit<T, "type">;
}

function replaceInList<T extends { id: string }>(
  list: ListResponse<T> | undefined,
  item: T,
): ListResponse<T> | undefined {
  if (!list) return list;
  let found = false;
  const items = list.items.map((existing) => {
    if (existing.id !== item.id) return existing;
    found = true;
    return item;
  });
  return found ? { ...list, items } : list;
}

function upsertLibrary(list: Library[] | undefined, library: Library): Library[] | undefined {
  if (!list) return list;
  const index = list.findIndex((l) => l.id === library.id);
  if (index === -1) return [...list, library];
  const next = list.slice();
  next[index] = library;
  return next;
}

/** Apply one server event to the caches and the live store. */
export function applyEvent(client: QueryClient, invalidate: Invalidator, event: ServerEvent): void {
  const live = useLive.getState();
  switch (event.type) {
    case "job.progress": {
      live.setJobProgress(withoutType(event));
      break;
    }
    case "job.updated": {
      const job: Job = event.job;
      client.setQueryData<Job>(keys.job(job.id), job);
      client.setQueriesData<ListResponse<Job>>({ queryKey: keys.jobs() }, (old) => replaceInList(old, job));
      client.setQueryData<FileDetail>(keys.file(job.file_id), (old) => {
        if (!old) return old;
        const others = old.jobs.filter((j) => j.id !== job.id);
        return { ...old, jobs: [job, ...others].sort((a, b) => b.created_at.localeCompare(a.created_at)) };
      });
      if (job.state !== "running") live.clearJob(job.id);
      // State changes move jobs between Running / Up next / History.
      invalidate.schedule(keys.jobs());
      if (job.state !== "running" && job.state !== "queued") {
        invalidate.schedule(keys.overview, 1500);
        // A finished job may have replaced the file: its tracks changed.
        invalidate.schedule(keys.file(job.file_id), 1000);
      }
      break;
    }
    case "file.updated": {
      const file: MediaFile = event.file;
      let reprobe = false;
      client.setQueryData<FileDetail>(keys.file(file.id), (old) => {
        if (!old) return old;
        const before = old.file;
        // Events carry no probe. When the file itself changed (converted,
        // replaced), the cached tracks are stale: refetch them.
        reprobe =
          !file.probe &&
          (before.size_bytes !== file.size_bytes ||
            before.modified_at !== file.modified_at ||
            before.path !== file.path ||
            (before.status !== file.status && file.status === "done"));
        return { ...old, file: { ...file, probe: file.probe ?? before.probe } };
      });
      if (reprobe) invalidate.schedule(keys.file(file.id), 300);
      const listFile: MediaFile = { ...file };
      delete listFile.probe;
      client.setQueriesData<ListResponse<MediaFile>>({ queryKey: keys.files() }, (old) =>
        replaceInList(old, listFile),
      );
      // Status filters and counts may have changed.
      invalidate.schedule(keys.files(), 2000);
      invalidate.schedule(keys.libraries, 1500);
      break;
    }
    case "files.changed": {
      invalidate.schedule(keys.files(), 600);
      invalidate.schedule(keys.libraries, 600);
      invalidate.schedule(keys.overview, 1500);
      break;
    }
    case "library.updated": {
      client.setQueryData<Library[]>(keys.libraries, (old) => upsertLibrary(old, event.library));
      break;
    }
    case "library.removed": {
      client.setQueryData<Library[]>(keys.libraries, (old) => old?.filter((l) => l.id !== event.id));
      live.clearScan(event.id);
      invalidate.schedule(keys.overview, 500);
      invalidate.schedule(keys.jobs(), 500);
      const route = parseRoute(window.location.hash);
      if (route.segments[0] === "library" && route.segments[1] === event.id) {
        navigate("/", { replace: true });
      }
      break;
    }
    case "scan.progress": {
      const scan = withoutType(event);
      live.setScan(scan);
      if (scan.phase === "done") {
        setTimeout(() => useLive.getState().clearScan(scan.library_id), 1500);
        invalidate.schedule(keys.files(), 300);
        invalidate.schedule(keys.libraries, 300);
        invalidate.schedule(keys.overview, 600);
      }
      break;
    }
    case "queue.state": {
      const state: QueueState = withoutType(event);
      client.setQueryData<QueueState>(keys.queue, state);
      client.setQueryData<Overview>(keys.overview, (old) => (old ? { ...old, queue: state } : old));
      break;
    }
    case "stats.updated": {
      client.setQueryData<Overview>(keys.overview, (old) => (old ? { ...old, totals: event.totals } : old));
      // Savings history and breakdowns are not in the event.
      invalidate.schedule(keys.overview, 4000);
      break;
    }
    case "hardware.updated": {
      client.setQueryData(keys.hardware, event.hardware);
      break;
    }
    case "settings.updated": {
      client.setQueryData(keys.settings, event.settings);
      invalidate.schedule(keys.queue, 300);
      break;
    }
    case "activity": {
      const entry: ActivityEntry = event.entry;
      client.setQueryData<{ items: ActivityEntry[] }>(keys.activity, (old) =>
        old ? { items: [entry, ...old.items.filter((e) => e.id !== entry.id)].slice(0, 100) } : old,
      );
      break;
    }
    default:
      // Unknown event types from a newer server are ignored.
      break;
  }
}

/** Query keys refreshed while polling: everything that moves on its own. */
const LIVE_KEYS: QueryKey[] = [
  keys.queue,
  keys.overview,
  keys.libraries,
  keys.activity,
  keys.jobs(),
  ["job"],
  ["file"],
  keys.files(),
];

/** Refetch the live views that are on screen. */
export function refreshLive(client: QueryClient): void {
  for (const queryKey of LIVE_KEYS) void client.invalidateQueries({ queryKey });
}

/**
 * Open the live connection. Returns a function that closes it for good.
 * Safe to call once per QueryClient.
 */
export function connectLive(client: QueryClient): () => void {
  const invalidate = new Invalidator(client);
  const live = useLive.getState();
  let socket: WebSocket | null = null;
  let attempts = 0;
  let failures = 0;
  let everOpened = false;
  let stopped = false;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;
  let pollDelay: ReturnType<typeof setTimeout> | null = null;
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  const pollOnce = () => {
    if (document.visibilityState === "hidden") return;
    refreshLive(client);
  };

  /** Refresh by polling once the socket has been down for a moment. */
  const startPolling = () => {
    if (stopped || pollTimer || pollDelay) return;
    pollDelay = setTimeout(() => {
      pollDelay = null;
      if (stopped || socket?.readyState === WebSocket.OPEN) return;
      live.setPolling(true);
      pollOnce();
      pollTimer = setInterval(pollOnce, POLL_INTERVAL_MS);
    }, POLL_GRACE_MS);
  };

  const stopPolling = () => {
    if (pollDelay) clearTimeout(pollDelay);
    if (pollTimer) clearInterval(pollTimer);
    pollDelay = null;
    pollTimer = null;
    live.setPolling(false);
  };

  const downState = () =>
    everOpened ? "reconnecting" : failures >= UNAVAILABLE_AFTER ? "unavailable" : "connecting";

  const scheduleReconnect = () => {
    if (stopped || retryTimer) return;
    const base = Math.min(MAX_BACKOFF_MS, MIN_BACKOFF_MS * 2 ** attempts);
    const delay = base / 2 + Math.random() * (base / 2);
    attempts += 1;
    retryTimer = setTimeout(() => {
      retryTimer = null;
      open();
    }, delay);
  };

  const reconnectNow = () => {
    if (stopped) return;
    if (socket && (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING)) return;
    if (retryTimer) {
      clearTimeout(retryTimer);
      retryTimer = null;
    }
    attempts = 0;
    open();
  };

  /** Detach and close a socket that is no longer the current one. */
  const retire = (ws: WebSocket) => {
    ws.onopen = null;
    ws.onmessage = null;
    ws.onclose = null;
    ws.onerror = null;
    try {
      ws.close();
    } catch {
      // Already closed.
    }
  };

  function open() {
    if (stopped) return;
    if (socket) {
      // Never keep two sockets: events would be applied twice.
      retire(socket);
      socket = null;
    }
    let ws: WebSocket;
    try {
      ws = new WebSocket(wsUrl());
    } catch {
      failures += 1;
      live.setConnection(downState());
      startPolling();
      scheduleReconnect();
      return;
    }
    socket = ws;

    ws.onopen = () => {
      if (socket !== ws) return;
      attempts = 0;
      failures = 0;
      stopPolling();
      live.setServerDown(false);
      live.setConnection("open");
      if (everOpened) {
        // Events sent while we were away are lost: refetch everything.
        void client.invalidateQueries();
      }
      everOpened = true;
    };

    ws.onmessage = (message) => {
      if (socket !== ws || typeof message.data !== "string") return;
      let event: ServerEvent;
      try {
        event = JSON.parse(message.data) as ServerEvent;
      } catch {
        return;
      }
      if (event && typeof event === "object" && typeof event.type === "string") {
        applyEvent(client, invalidate, event);
      }
    };

    ws.onclose = () => {
      // A socket that was replaced by a newer one must not schedule anything.
      if (socket !== ws) return;
      socket = null;
      if (stopped) return;
      failures += everOpened ? 0 : 1;
      live.setConnection(downState());
      // Progress and scan counts we hold are stale now; the views fall back
      // to what the server last reported until the refetch or next event.
      live.clearProgress();
      startPolling();
      scheduleReconnect();
    };

    ws.onerror = () => {
      // onclose follows and handles the retry.
    };
  }

  const onOnline = () => reconnectNow();
  const onVisible = () => {
    if (document.visibilityState === "visible") reconnectNow();
  };
  window.addEventListener("online", onOnline);
  document.addEventListener("visibilitychange", onVisible);
  // Requests say whether the server is there. When one gets through again
  // (it restarted), reconnect right away instead of waiting out the backoff.
  let serverWasDown = false;
  const stopReachability = onReachability((reachable) => {
    useLive.getState().setServerDown(!reachable);
    if (reachable && serverWasDown) reconnectNow();
    serverWasDown = !reachable;
  });

  open();

  return () => {
    stopped = true;
    invalidate.cancelAll();
    if (retryTimer) clearTimeout(retryTimer);
    stopPolling();
    window.removeEventListener("online", onOnline);
    document.removeEventListener("visibilitychange", onVisible);
    stopReachability();
    if (socket) {
      retire(socket);
      socket = null;
    }
  };
}
