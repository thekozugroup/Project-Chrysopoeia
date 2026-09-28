"use client";

/**
 * User actions (mutations) with consistent feedback: a toast that names what
 * happened, a plain-language error toast when it fails, and a cache refresh.
 */

import { useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { api, errorMessage } from "./api";
import { plural } from "./format";
import { invalidateWork, keys } from "./queries";
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
      toast(job.state === "cancelled" ? "Cancelled" : "Removed from the queue", {
        description: `${job.file_name} was left as it is.`,
      });
      refresh();
    },
    onError: fail("Couldn't cancel"),
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

  const bulk = useMutation({
    // `note` says what the selection left out; it isn't sent.
    mutationFn: ({ action, ids, library, status }: BulkRequest & { note?: string | null }) =>
      api.bulk({ action, ids, library, status }),
    onSuccess: (res, { action, note }) => {
      const n = plural(res.affected, "file");
      const text =
        action === "skip" ? `Skipped ${n}` : action === "retry_failed" ? `Retrying ${n}` : `Added ${n} to the queue`;
      if (res.affected === 0) {
        toast("Nothing to do", { description: note ?? "None of those files could take that action." });
      } else toast.success(text, { description: note ?? undefined });
      refresh();
    },
    onError: fail("That didn't work"),
  });

  return { queue, skip, bulk };
}
