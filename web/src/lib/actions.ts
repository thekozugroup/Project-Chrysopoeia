"use client";

/**
 * User actions (mutations) with consistent feedback: a toast that names what
 * happened, a plain-language error toast when it fails, and a cache refresh.
 */

import { useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { ApiError, api, errorMessage } from "./api";
import { serverLeftOutText } from "./convertible";
import { plural } from "./format";
import { invalidateWork, keys } from "./queries";
import { useLive } from "./store";
import type { BulkRequest, Job, MediaFile, QueueState } from "./types";

function fail(prefix: string) {
  return (error: unknown) => toast.error(prefix, { description: errorMessage(error) });
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

  const retry = useMutation({
    mutationFn: (job: Job) => api.queueFile(job.file_id),
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
    mutationFn: async ({ file, next = false }: { file: MediaFile; next?: boolean }) => {
      const job = await api.queueFile(file.id);
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

  /** "Ignore this file": a damaged original is left alone and marked "Skipped by you". */
  const ignore = useMutation({
    mutationFn: (file: MediaFile) => api.skipFile(file.id),
    onSuccess: (file) => {
      toast("Ignored", { description: `${file.file_name} is left as it is. A new copy is picked up automatically.` });
      refresh();
    },
    onError: fail("Couldn't ignore it"),
  });

  /**
   * "Convert anyway": one conversion without the library's skip rules
   * (checks still run). A server from before this option refuses the
   * unknown `force` field; say so plainly instead of a raw error.
   */
  const convertAnyway = useMutation({
    mutationFn: (file: MediaFile) => api.queueFile(file.id, { force: true }),
    onSuccess: (job) => {
      useLive.getState().markForced(job.id);
      toast.success("Converting anyway", { description: job.file_name });
      refresh();
    },
    onError: (error) => {
      if (error instanceof ApiError && error.status === 400 && (error.field === "force" || /\bforce\b/.test(error.message))) {
        toast.error("This server can't convert skipped files anyway", {
          description: "Update the Chrysopoeia container to use Convert anyway.",
        });
      } else toast.error("Couldn't add it to the queue", { description: errorMessage(error) });
    },
  });

  const bulk = useMutation({
    // `note` says what the selection left out and `ignored` marks a skip of
    // damaged originals ("Ignore"); neither is sent.
    mutationFn: ({ action, ids, library, status }: BulkRequest & { note?: string | null; ignored?: boolean }) =>
      api.bulk({ action, ids, library, status }),
    onSuccess: (res, { action, note, ignored }) => {
      const n = plural(res.affected, "file");
      if (ignored && res.affected > 0) {
        toast(`Ignored ${n}`, {
          description: `${res.affected === 1 ? "It's" : "They're"} left as ${res.affected === 1 ? "it is" : "they are"}. A replaced copy is picked up automatically.`,
        });
        refresh();
        return;
      }
      const text =
        action === "skip" ? `Skipped ${n}` : action === "retry_failed" ? `Trying ${n} again` : `Added ${n} to the queue`;
      // What the selection left out, then what the server left out on top.
      const details = [note, action === "queue" ? serverLeftOutText(res.left_out) : null].filter(Boolean).join(" ");
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
        const res = await api.bulk(request);
        affected += res.affected;
        leftOut += res.left_out ?? 0;
      }
      // "Convert anyway" conversions are queued the same way again.
      for (const id of forced) {
        await api.queueFile(id, { force: true });
        affected += 1;
      }
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

  return { queue, skip, ignore, convertAnyway, bulk, retryAll };
}
