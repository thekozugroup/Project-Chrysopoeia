"use client";

/**
 * Small client-side store for state that changes many times a second and
 * does not belong in the TanStack Query cache: the WebSocket connection,
 * live job progress, scan progress and screen-reader announcements.
 */

import { create } from "zustand";
import { knownOverall, readProgress, type ProgressReading } from "./progress";
import type { Job, JobProgress, JobStage, ScanProgress } from "./types";
import { useSustained } from "./utils";

/**
 * WebSocket connection state as shown to the user. `unavailable` means the
 * socket has never opened after several tries (typically a reverse proxy
 * without WebSocket support); the app then refreshes by polling.
 */
export type ConnectionState = "connecting" | "open" | "reconnecting" | "unavailable";

interface LiveState {
  connection: ConnectionState;
  /** True while live updates are replaced by refreshing every few seconds. */
  polling: boolean;
  /** True while requests can't reach the server at all (stopped or restarting). */
  serverDown: boolean;
  /** Live progress by job id, from `job.progress` events. */
  jobs: Record<string, JobProgress>;
  /** Scan progress by library id, from `scan.progress` events. */
  scans: Record<string, ScanProgress>;
  /** Latest polite screen-reader announcement. */
  announcement: string;
  setConnection: (state: ConnectionState) => void;
  setPolling: (polling: boolean) => void;
  setServerDown: (down: boolean) => void;
  setJobProgress: (progress: JobProgress) => void;
  clearJob: (jobId: string) => void;
  setScan: (scan: ScanProgress) => void;
  clearScan: (libraryId: string) => void;
  /** Forget live progress, e.g. when the connection drops and it goes stale. */
  clearProgress: () => void;
  announce: (text: string) => void;
}

export const useLive = create<LiveState>((set) => ({
  connection: "connecting",
  polling: false,
  serverDown: false,
  jobs: {},
  scans: {},
  announcement: "",
  setConnection: (connection) => set((s) => (s.connection === connection ? s : { connection })),
  setPolling: (polling) => set((s) => (s.polling === polling ? s : { polling })),
  setServerDown: (serverDown) => set((s) => (s.serverDown === serverDown ? s : { serverDown })),
  setJobProgress: (progress) => set((s) => ({ jobs: { ...s.jobs, [progress.job_id]: progress } })),
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
  clearProgress: () =>
    set((s) => (Object.keys(s.jobs).length || Object.keys(s.scans).length ? { jobs: {}, scans: {} } : s)),
  announce: (announcement) => set({ announcement }),
}));

/**
 * Whether the server has been unreachable for a moment (the same signal as
 * the "Can't reach Szalinski" banner). Actions that change something are
 * disabled meanwhile, since they would fail.
 */
export function useServerDown(): boolean {
  const down = useLive((s) => s.serverDown && s.connection !== "open");
  // A single failed request during a blip shouldn't flash anything.
  return useSustained(down, 1500);
}

/** Why a control is disabled while the server is away. */
export const SERVER_DOWN_TITLE = "Available when Szalinski is back";

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
    // An older server's events don't say: then the share is as it says.
    progress_basis: live.progress_basis ?? null,
    frames: live.frames ?? null,
    elapsed_secs: live.elapsed_secs ?? null,
    encoder: live.encoder ?? job.encoder,
    hw_api: live.hw_api ?? job.hw_api,
    attempt: live.attempt,
  };
}

/** Live stage and whole-file progress of the running job for a file. */
export interface FileLive {
  stage: JobStage;
  /** Progress through the whole job, 0..100; `null` while it isn't known. */
  overall: number | null;
  /** How the share was worked out (see `readProgress`). */
  reading: ProgressReading;
}

/** Live progress of whichever running job belongs to a file, or `null`. */
export function useFileLive(fileId: string): FileLive | null {
  const live = useLive((s) => {
    for (const p of Object.values(s.jobs)) {
      if (p.file_id === fileId) return p;
    }
    return null;
  });
  if (!live) return null;
  return { stage: live.stage, overall: knownOverall(live), reading: readProgress(live) };
}
