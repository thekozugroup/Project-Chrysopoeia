"use client";

/**
 * User actions (mutations) with consistent feedback: a toast that names what
 * happened, a plain-language error toast when it fails, and a cache refresh.
 */

import { useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { ApiError, api, errorMessage } from "./api";
import { repeatsForce, serverLeftOutText } from "./convertible";
import { plural } from "./format";
import { invalidateWork, keys } from "./queries";
import type { BulkRequest, Job, MediaFile, QueueState } from "./types";

function fail(prefix: string) {
  return (error: unknown) => toast.error(prefix, { description: errorMessage(error) });
}

/** The history is read this many jobs at a time to find "Convert anyway" conversions… */
const FORCED_PAGE = 200;
/** …and at most this far back. */
export const FORCED_READ_MAX = 2_000;

/**
 * Of these files, the ones whose latest finished conversion was "Convert
 * anyway" and ended without a result (see `repeatsForce`), so trying them
 * again repeats that choice. Read from the history, newest first, until
 * every file's latest conversion was seen (failed files' conversions are
 * normally the latest ones) or `FORCED_READ_MAX` jobs were read.
 */
export async function forcedRetries(fileIds: readonly string[], signal?: AbortSignal): Promise<Set<string>> {
  const wanted = new Set(fileIds);
  const seen = new Set<string>();
  const forced = new Set<string>();
  for (let offset = 0; offset < FORCED_READ_MAX && seen.size < wanted.size; offset += FORCED_PAGE) {
    const page = await api.jobs({ state: "history", limit: FORCED_PAGE, offset }, signal);
    for (const job of page.items) {
      if (!wanted.has(job.file_id) || seen.has(job.file_id)) continue;
      seen.add(job.file_id);
      if (repeatsForce(job)) forced.add(job.file_id);
    }
    if (offset + page.items.length >= page.total || page.items.length < FORCED_PAGE) break;
  }
  return forced;
}

/** Queue one file with "Convert anyway" again; `false` when it's already queued. */
async function queueForced(id: string): Promise<boolean> {
  try {
    await api.queueFile(id, { force: true });
    return true;
  } catch (error) {
    if (error instanceof ApiError && error.status === 409) return false;
    throw error;
  }
}

/**
 * Send a bulk request. "Try again" for chosen files (`retry_failed` with
 * `ids`) queues those whose last conversion was "Convert anyway" the same
 * way again, one by one, and the rest in bulk: the bulk request can't
 * carry the choice.
 */
export async function sendBulk(request: BulkRequest): Promise<{ affected: number; leftOut: number }> {
  const ids = request.action === "retry_failed" ? request.ids : undefined;
  const forced = ids?.length ? await forcedRetries(ids) : new Set<string>();
  let affected = 0;
  let leftOut = 0;
  const rest = ids ? ids.filter((id) => !forced.has(id)) : undefined;
  if (!rest || rest.length) {
    const res = await api.bulk(rest ? { ...request, ids: rest } : request);
    affected += res.affected;
    leftOut += res.left_out;
  }
  for (const id of forced) if (await queueForced(id)) affected += 1;
  return { affected, leftOut };
}

/** Cancel, move to top, and retry for jobs. */
export function useJobActions() {
  const client = useQueryClient();
  const refresh = () => invalidateWork(client);

  const cancel = useMutation({
    mutationFn: (job: Job) => api.cancelJob(job.id),
    onSuccess: (job) => {
      toast(job.started_at ? "Stopped" : "Removed from the queue", {
        description: `${job.file_name} was left as it is.`,
      });
      refresh();
    },
    onError: fail("Couldn't stop it"),
  });

  const moveToTop = useMutation({
    mutationFn: (job: Job) => api.moveJobToTop(job.id),
    onSuccess: (job) => {
      toast("Moved to the top", { description: `${job.file_name} is next in line.` });
      void client.invalidateQueries({ queryKey: keys.jobs() });
    },
    onError: fail("Couldn't move it"),
  });

  // A "Convert anyway" conversion that failed or was stopped is tried again the same way.
  const retry = useMutation({
    mutationFn: (job: Job) => api.queueFile(job.file_id, repeatsForce(job) ? { force: true } : {}),
    onSuccess: (job) => {
      toast.success("Added to the queue", { description: job.file_name });
      refresh();
      void client.invalidateQueries({ queryKey: keys.fileJobs(job.file_id) });
    },
    onError: fail("Couldn't add it to the queue"),
  });

  const clearHistory = useMutation({
    mutationFn: () => api.clearHistory(),
    onSuccess: (res) => {
      toast(`Cleared ${plural(res.affected, "entry", "entries")}`, {
        description: "Files keep their status; only the history list was emptied.",
      });
      void client.invalidateQueries({ queryKey: keys.jobs() });
    },
    onError: fail("Couldn't clear the history"),
  });

  return { cancel, moveToTop, retry, clearHistory };
}

/** Pause, resume and stop for the whole queue. */
export function useQueueActions() {
  const client = useQueryClient();
  const apply = (state: QueueState) => {
    client.setQueryData(keys.queue, state);
    void client.invalidateQueries({ queryKey: keys.overview });
  };

  const pause = useMutation({
    mutationFn: () => api.pauseQueue(),
    onSuccess: (state) => {
      apply(state);
      toast("Paused", {
        description: state.running
          ? "Files already converting will finish. Nothing new will start."
          : "Nothing new will start until you resume.",
      });
    },
    onError: fail("Couldn't pause"),
  });

  const resume = useMutation({
    mutationFn: () => api.resumeQueue(),
    onSuccess: (state) => {
      apply(state);
      toast.success("Resumed");
    },
    onError: fail("Couldn't resume"),
  });

  const stop = useMutation({
    mutationFn: () => api.stopQueue(),
    onSuccess: (state) => {
      apply(state);
      invalidateWork(client);
      toast("Stopped", { description: "Running files went back to the queue. Resume to continue." });
    },
    onError: fail("Couldn't stop"),
  });

  return { pause, resume, stop };
}

/** Queue, skip and bulk actions on files. */
export function useFileActions() {
  const client = useQueryClient();
  const refresh = () => invalidateWork(client);

  const queue = useMutation({
    // `force`: repeat "Convert anyway" (see `repeatsForce`), for trying a file again.
    mutationFn: async ({ file, next = false, force = false }: { file: MediaFile; next?: boolean; force?: boolean }) => {
      const job = await api.queueFile(file.id, force ? { force: true } : {});
      // "Convert next" means ahead of everything, including files moved to
      // the top earlier, which a fixed priority can't promise.
      if (next && job.state === "queued") return api.moveJobToTop(job.id);
      return job;
    },
    onSuccess: (job, { next }) => {
      toast.success(next ? "Converting next" : "Added to the queue", { description: job.file_name });
      refresh();
      void client.invalidateQueries({ queryKey: keys.fileJobs(job.file_id) });
    },
    onError: fail("Couldn't add it to the queue"),
  });

  const skip = useMutation({
    mutationFn: (file: MediaFile) => api.skipFile(file.id),
    onSuccess: (file) => {
      toast("Skipped", { description: `${file.file_name} will be left as it is.` });
      refresh();
    },
    onError: fail("Couldn't skip it"),
  });

  /** "Skip this file" for a damaged original: left alone ("Skipped by you") until it's replaced. */
  const skipUnreadable = useMutation({
    mutationFn: (file: MediaFile) => api.skipFile(file.id),
    onSuccess: (file) => {
      toast("Skipped", { description: `${file.file_name} is left as it is. A replaced copy is picked up automatically.` });
      refresh();
    },
    onError: fail("Couldn't skip it"),
  });

  /** "Convert anyway": one conversion without the library's skip rules (checks still run). */
  const convertAnyway = useMutation({
    mutationFn: (file: MediaFile) => api.queueFile(file.id, { force: true }),
    onSuccess: (job) => {
      toast.success("Converting anyway", { description: job.file_name });
      refresh();
    },
    onError: fail("Couldn't add it to the queue"),
  });

  const bulk = useMutation({
    // `note` says what the selection left out and `unreadable` marks a skip
    // of damaged originals; neither is sent.
    mutationFn: ({ action, ids, library, status }: BulkRequest & { note?: string | null; unreadable?: boolean }) =>
      sendBulk({ action, ids, library, status }),
    onSuccess: (res, { action, note, unreadable }) => {
      const n = plural(res.affected, "file");
      if (unreadable && res.affected > 0) {
        toast(`Skipped ${n}`, {
          description: `${res.affected === 1 ? "It's" : "They're"} left as ${res.affected === 1 ? "it is" : "they are"}. A replaced copy is picked up automatically.`,
        });
        refresh();
        return;
      }
      const text =
        action === "skip" ? `Skipped ${n}` : action === "retry_failed" ? `Trying ${n} again` : `Added ${n} to the queue`;
      // What the selection left out, then what the server left out on top.
      const details = [note, action === "queue" ? serverLeftOutText(res.leftOut) : null].filter(Boolean).join(" ");
      if (res.affected === 0) {
        toast("Nothing to do", { description: details || "None of those files could take that action." });
      } else toast.success(text, { description: details || undefined });
      refresh();
    },
    onError: fail("That didn't work"),
  });

  /**
   * "Try again" for everything one problem stopped, once it's fixed: failed
   * files (by id, or all failed files when there are too many to list) and
   * converted files whose second conversion it stopped, in one step with
   * one message.
   */
  const retryAll = useMutation({
    mutationFn: async ({ requests, forced = [] }: { requests: BulkRequest[]; forced?: string[] }) => {
      let affected = 0;
      let leftOut = 0;
      for (const request of requests) {
        const res = await sendBulk(request);
        affected += res.affected;
        leftOut += res.leftOut;
      }
      // "Convert anyway" conversions are queued the same way again.
      for (const id of forced) if (await queueForced(id)) affected += 1;
      return { affected, leftOut };
    },
    onSuccess: ({ affected, leftOut }) => {
      const left = serverLeftOutText(leftOut) ?? undefined;
      if (affected === 0) {
        toast("Nothing to do", { description: left ?? "Those files are already queued or were removed." });
      } else toast.success(`Trying ${plural(affected, "file")} again`, { description: left });
      refresh();
    },
    onError: fail("That didn't work"),
  });

  return { queue, skip, skipUnreadable, convertAnyway, bulk, retryAll };
}
