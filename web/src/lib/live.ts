"use client";

/**
 * WebSocket client for `/api/ws`. Events patch the TanStack Query cache in
 * place (so lists update without refetching) and feed the live store with
 * progress. The socket reconnects with exponential backoff and refetches
 * everything after a reconnect, because events sent while disconnected are
 * gone.
 */

import type { QueryClient, QueryKey } from "@tanstack/react-query";
import { wsUrl } from "./api";
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
const MAX_BACKOFF_MS = 30_000;

/** Debounced invalidation, so bursts of events cause one refetch. */
class Invalidator {
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
      }
      break;
    }
    case "file.updated": {
      const file: MediaFile = event.file;
      client.setQueryData<FileDetail>(keys.file(file.id), (old) =>
        old ? { ...old, file: { ...file, probe: file.probe ?? old.file.probe } } : old,
      );
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

/**
 * Open the live connection. Returns a function that closes it for good.
 * Safe to call once per QueryClient.
 */
export function connectLive(client: QueryClient): () => void {
  const invalidate = new Invalidator(client);
  let socket: WebSocket | null = null;
  let attempts = 0;
  let everOpened = false;
  let stopped = false;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;

  const setConnection = useLive.getState().setConnection;

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

  function open() {
    if (stopped) return;
    let ws: WebSocket;
    try {
      ws = new WebSocket(wsUrl());
    } catch {
      setConnection(everOpened ? "reconnecting" : "connecting");
      scheduleReconnect();
      return;
    }
    socket = ws;

    ws.onopen = () => {
      attempts = 0;
      setConnection("open");
      if (everOpened) {
        // Events sent while we were away are lost: refetch everything.
        void client.invalidateQueries();
      }
      everOpened = true;
    };

    ws.onmessage = (message) => {
      if (typeof message.data !== "string") return;
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
      if (socket === ws) socket = null;
      if (stopped) return;
      setConnection(everOpened ? "reconnecting" : "connecting");
      // Progress we hold may be stale now; the refetch after reconnect restores it.
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

  open();

  return () => {
    stopped = true;
    invalidate.cancelAll();
    if (retryTimer) clearTimeout(retryTimer);
    window.removeEventListener("online", onOnline);
    document.removeEventListener("visibilitychange", onVisible);
    if (socket) {
      socket.onclose = null;
      socket.close();
      socket = null;
    }
  };
}
