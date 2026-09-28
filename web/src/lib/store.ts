"use client";

/**
 * Small client-side store for state that changes many times a second and
 * does not belong in the TanStack Query cache: the WebSocket connection,
 * live job progress, scan progress and screen-reader announcements.
 */

import { create } from "zustand";
import type { Job, JobProgress, ScanProgress } from "./types";

/** WebSocket connection state as shown to the user. */
export type ConnectionState = "connecting" | "open" | "reconnecting";

interface LiveState {
  connection: ConnectionState;
  /** Live progress by job id, from `job.progress` events. */
  jobs: Record<string, JobProgress>;
  /** Scan progress by library id, from `scan.progress` events. */
  scans: Record<string, ScanProgress>;
  /** Latest polite screen-reader announcement. */
  announcement: string;
  setConnection: (state: ConnectionState) => void;
  setJobProgress: (progress: JobProgress) => void;
  clearJob: (jobId: string) => void;
  setScan: (scan: ScanProgress) => void;
  clearScan: (libraryId: string) => void;
  announce: (text: string) => void;
}

export const useLive = create<LiveState>((set) => ({
  connection: "connecting",
  jobs: {},
  scans: {},
  announcement: "",
  setConnection: (connection) => set({ connection }),
  setJobProgress: (progress) =>
    set((s) => ({ jobs: { ...s.jobs, [progress.job_id]: progress } })),
  clearJob: (jobId) =>
    set((s) => {
      if (!(jobId in s.jobs)) return s;
      const jobs = { ...s.jobs };
      delete jobs[jobId];
      return { jobs };
    }),
  setScan: (scan) => set((s) => ({ scans: { ...s.scans, [scan.library_id]: scan } })),
  clearScan: (libraryId) =>
    set((s) => {
      if (!(libraryId in s.scans)) return s;
      const scans = { ...s.scans };
      delete scans[libraryId];
      return { scans };
    }),
  announce: (announcement) => set({ announcement }),
}));

/** A job with its latest live progress applied (only while it is running). */
export function useLiveJob(job: Job): Job {
  const live = useLive((s) => s.jobs[job.id]);
  if (!live || job.state !== "running") return job;
  return {
    ...job,
    stage: live.stage,
    progress: live.progress,
    fps: live.fps,
    speed: live.speed,
    eta_secs: live.eta_secs,
    encoder: live.encoder ?? job.encoder,
    hw_api: live.hw_api ?? job.hw_api,
    attempt: live.attempt,
  };
}

/** Live progress (0..100) of whichever running job belongs to a file. */
export function useFileProgress(fileId: string): number | null {
  return useLive((s) => {
    for (const p of Object.values(s.jobs)) {
      if (p.file_id === fileId) return p.progress;
    }
    return null;
  });
}
