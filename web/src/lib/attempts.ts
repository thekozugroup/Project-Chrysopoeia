/**
 * A job's attempts in plain words (`Job.attempts`): where each one ran,
 * how the original was decoded, how long it took and why it failed, e.g.
 * "Attempt 1 · AMD GPU (VA-API) · decoded on the GPU · 2 min" and
 * "Failed: Plays start to finish: playback stopped at 1:01 of 2:21:02".
 * Also the text a bug report needs about a job.
 */

import { formatDateTime, formatDuration } from "./format";
import { GPU_VENDOR_LABEL, HW_API_LABEL, HW_API_TECH } from "./labels";
import type { GpuDevice, Job, JobAttempt, ValidationCheck } from "./types";

/** "The disk is full" → "the disk is full", to follow a colon; "NVIDIA …" and "HEVC" stay as they are. */
export function lowerFirst(text: string): string {
  if (/^[A-Z]{2}/.test(text)) return text;
  return text.charAt(0).toLowerCase() + text.slice(1);
}

/**
 * Where an attempt ran: "CPU", "NVIDIA GPU", "AMD GPU (VA-API)". VA-API is
 * named after the GPU's maker when the render node it used is one the
 * hardware check found.
 */
export function attemptDevice(attempt: Pick<JobAttempt, "hw_api" | "device">, gpus: readonly GpuDevice[] = []): string {
  if (attempt.hw_api === "vaapi" && attempt.device) {
    const gpu = gpus.find((g) => g.render_node === attempt.device);
    if (gpu && gpu.vendor !== "other") return `${GPU_VENDOR_LABEL[gpu.vendor]} GPU (VA-API)`;
  }
  return HW_API_LABEL[attempt.hw_api] ?? attempt.hw_api;
}

/** "decoded on the GPU" or "decoded on the CPU"; nothing for the CPU, which does both anyway. */
export function decodingText(attempt: Pick<JobAttempt, "hw_api" | "hw_decode">): string | null {
  if (attempt.hw_api === "software") return null;
  return attempt.hw_decode ? "decoded on the GPU" : "decoded on the CPU";
}

/** "Attempt 1 · AMD GPU (VA-API) · decoded on the GPU · 2 min". */
export function attemptHeading(attempt: JobAttempt, gpus: readonly GpuDevice[] = []): string {
  return [
    `Attempt ${attempt.attempt}`,
    attemptDevice(attempt, gpus),
    decodingText(attempt),
    Number.isFinite(attempt.elapsed_secs) ? formatDuration(attempt.elapsed_secs) : null,
  ]
    .filter(Boolean)
    .join(" · ");
}

/** "Plays start to finish: playback stopped at 1:01 of 2:21:02", a failed check with its reason. */
export function checkReason(check: Pick<ValidationCheck, "label" | "detail">): string {
  const detail = check.detail.trim().replace(/\.$/, "");
  return detail ? `${check.label}: ${lowerFirst(detail)}` : check.label;
}

/**
 * How an attempt ended: "Worked", or "Failed: " and why (the check and its
 * reason, else the error).
 */
export function attemptOutcome(attempt: Pick<JobAttempt, "result" | "failed_check" | "error">): string {
  if (attempt.result === "succeeded") return "Worked";
  if (attempt.failed_check) return `Failed: ${checkReason(attempt.failed_check)}`;
  const error = attempt.error?.trim();
  return error ? `Failed: ${lowerFirst(error)}` : "Failed";
}

/** The job's attempts, the one still running added (it isn't in the list until it ends). */
export interface AttemptRow {
  attempt: JobAttempt | null;
  /** For the running one: its number and where it runs. */
  running: { attempt: number; device: string } | null;
}

/**
 * The attempts worth a section of their own: when there was more than one,
 * or while a later one runs after one failed. A single attempt is the job
 * itself (its outcome says it all).
 */
export function attemptRows(
  job: Pick<Job, "state" | "attempt" | "hw_api" | "attempts">,
  gpus: readonly GpuDevice[] = [],
): AttemptRow[] {
  const ended = job.attempts ?? [];
  const rows: AttemptRow[] = ended.map((attempt) => ({ attempt, running: null }));
  const runningNow = job.state === "running" && job.attempt > (ended.at(-1)?.attempt ?? 0);
  if (runningNow) {
    rows.push({
      attempt: null,
      running: {
        attempt: job.attempt,
        device: job.hw_api ? attemptDevice({ hw_api: job.hw_api, device: null }, gpus) : "Next way",
      },
    });
  }
  return rows.length > 1 ? rows : [];
}

/**
 * The last attempt that failed a check, for a short line on a running job's
 * card: "the last try failed a check (Plays start to finish)".
 */
export function lastFailedCheck(job: Pick<Job, "attempts">): ValidationCheck | null {
  const last = job.attempts?.at(-1);
  return last?.result === "failed" ? (last.failed_check ?? null) : null;
}

/**
 * Everything about a job a bug report needs, as plain text to paste: the
 * file, how it ended, every attempt with its command, why it failed and
 * what ffmpeg said, the checks and the notes.
 */
export function jobReportText(job: Job, gpus: readonly GpuDevice[] = []): string {
  const lines: string[] = [`Job ${job.id}`, `File: ${job.file_path}`, `State: ${job.state} (${job.stage})`];
  if (job.started_at) lines.push(`Started: ${formatDateTime(job.started_at)}`);
  if (job.finished_at) lines.push(`Finished: ${formatDateTime(job.finished_at)}`);
  if (job.error) lines.push(`Error${job.problem ? ` (${job.problem})` : ""}: ${job.error}`);
  if (job.skip_reason) lines.push(`Skipped: ${job.skip_reason}`);
  if (job.encoder || job.hw_api) {
    lines.push(
      `Encoder: ${[job.encoder, job.hw_api ? HW_API_TECH[job.hw_api] : null].filter(Boolean).join(" · ")}, attempt ${job.attempt}`,
    );
  }
  if (job.state === "running" && job.progress_basis) {
    lines.push(
      `Progress: ${job.progress_basis === "unknown" ? "not known" : `${Math.round(job.progress)}% (${job.progress_basis})`}` +
        `${job.frames != null ? `, ${job.frames} frames` : ""}${job.elapsed_secs != null ? `, ${job.elapsed_secs} s` : ""}`,
    );
  }
  for (const note of job.notes ?? []) lines.push(`Note: ${note}`);
  for (const attempt of job.attempts ?? []) {
    lines.push("", `${attemptHeading(attempt, gpus)} (${attempt.encoder}, ${HW_API_TECH[attempt.hw_api]})`);
    lines.push(attemptOutcome(attempt));
    if (attempt.failed_check && attempt.error) lines.push(`Error: ${attempt.error}`);
    if (attempt.command) lines.push(`Command: ${attempt.command}`);
    if (attempt.log_tail) lines.push("ffmpeg said:", attempt.log_tail);
  }
  if (job.validation) {
    lines.push("", `Checks (${job.validation.level}): ${job.validation.passed ? "passed" : "failed"}`);
    for (const check of job.validation.checks) lines.push(`- ${check.status}: ${check.label}: ${check.detail}`);
  }
  if (job.command) lines.push("", `Final command: ${job.command}`);
  if (job.log_tail) lines.push("", "Last lines from ffmpeg:", job.log_tail);
  return lines.join("\n");
}
