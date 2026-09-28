/**
 * Whole-file progress. The server reports progress per stage (converting
 * goes 0 → 100, then checking quality starts again at 0), which reads as
 * going backwards. The UI shows one number for the whole file instead,
 * weighting each stage by roughly how long it takes, and keeps the stage's
 * own percentage as secondary detail.
 */

import type { JobStage } from "./types";

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
 * Offset of the page to show when `offset` points past the end of a list
 * (the list shrank, or an old link). Returns `null` when the offset is fine.
 */
export function clampOffset(offset: number, total: number, limit: number): number | null {
  if (offset <= 0) return null;
  if (offset < total) return null;
  if (total <= 0 || limit <= 0) return 0;
  return Math.floor((total - 1) / limit) * limit;
}
