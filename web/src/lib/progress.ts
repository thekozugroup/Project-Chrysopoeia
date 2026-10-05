/**
 * Whole-file progress. The server reports progress per stage (converting
 * goes 0 → 100, then checking quality starts again at 0), which reads as
 * going backwards. The UI shows one number for the whole file instead,
 * weighting each stage by roughly how long it takes, and keeps the stage's
 * own percentage as secondary detail.
 */

import { formatCount, formatDuration } from "./format";
import type { Job, JobStage } from "./types";

/** Share of the whole job each stage covers, as [start, end] in percent. */
const STAGE_SPAN: Record<JobStage, readonly [number, number]> = {
  waiting: [0, 0],
  preparing: [0, 2],
  transcoding: [2, 85],
  verifying: [85, 98],
  finalizing: [98, 100],
};

/** Overall progress (0..100) from a stage and that stage's own progress. */
export function overallProgress(stage: JobStage, stageProgress: number): number {
  const [start, end] = STAGE_SPAN[stage] ?? STAGE_SPAN.waiting;
  const within = Number.isFinite(stageProgress) ? Math.max(0, Math.min(100, stageProgress)) : 0;
  return start + ((end - start) * within) / 100;
}

/**
 * What a running job's progress can honestly say. While converting, the
 * server tells how it worked the share out (`Job.progress_basis`):
 * - `measured`: from how far into the file ffmpeg is (or a stage other than
 *   converting, or a server that doesn't say);
 * - `estimated`: from the frames encoded over the frames the original should
 *   have, because ffmpeg didn't say how far it is: shown as an estimate;
 * - `unknown`: neither, so there is no share to show (the server's 0 means
 *   nothing): the frames encoded and the time spent instead, never "0%".
 */
export type ProgressReading =
  | { kind: "measured"; overall: number }
  | { kind: "estimated"; overall: number; frames: number | null }
  | { kind: "unknown"; frames: number | null; elapsedSecs: number | null };

type ProgressFields = Pick<Job, "stage" | "progress" | "progress_basis" | "frames" | "elapsed_secs">;

export function readProgress(job: ProgressFields): ProgressReading {
  const overall = overallProgress(job.stage, job.progress);
  if (job.stage !== "transcoding") return { kind: "measured", overall };
  switch (job.progress_basis) {
    case "unknown":
      return { kind: "unknown", frames: job.frames ?? null, elapsedSecs: job.elapsed_secs ?? null };
    case "frames":
      return { kind: "estimated", overall, frames: job.frames ?? null };
    default:
      return { kind: "measured", overall };
  }
}

/** Whole-file progress when there is one to show, `null` when it isn't known. */
export function knownOverall(job: ProgressFields): number | null {
  const reading = readProgress(job);
  return reading.kind === "unknown" ? null : reading.overall;
}

/** "1,017 frames": frames encoded, in the user's locale. */
export function framesText(frames: number): string {
  return `${formatCount(frames)} ${frames === 1 ? "frame" : "frames"}`;
}

/**
 * What a conversion whose share isn't known shows instead: "1,017 frames ·
 * 6 min elapsed" (either part alone when only one is known).
 */
export function framesElapsedText(frames: number | null, elapsedSecs: number | null): string {
  const parts = [
    frames !== null && Number.isFinite(frames) ? framesText(frames) : null,
    elapsedSecs !== null && Number.isFinite(elapsedSecs) ? `${formatDuration(elapsedSecs)} elapsed` : null,
  ].filter(Boolean);
  return parts.length ? parts.join(" · ") : "Converting";
}

/**
 * Offset of the page to show when `offset` points past the end of a list
 * (the list shrank, or an old link). Returns `null` when the offset is fine.
 */
export function clampOffset(offset: number, total: number, limit: number): number | null {
  if (offset <= 0) return null;
  if (offset < total) return null;
  if (total <= 0 || limit <= 0) return 0;
  return Math.floor((total - 1) / limit) * limit;
}
