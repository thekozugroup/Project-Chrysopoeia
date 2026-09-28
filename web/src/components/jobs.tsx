"use client";

/**
 * Job views: the live "now converting" card and the detail sheet with the
 * before/after, verification report, ffmpeg command and log.
 */

import { ArrowRight, Check, CircleSlash, FileVideo, Info, RotateCcw, ArrowUpToLine, SlidersHorizontal, X } from "lucide-react";
import { useState, type ReactNode } from "react";
import { useThrottledAnnouncement } from "@/components/providers";
import { CheckIcon, EncoderBadge, JobStateBadge } from "@/components/status";
import { Button, buttonVariants } from "@/components/ui/button";
import { Callout, CodeBlock, Detail, Meter, Skeleton } from "@/components/ui/display";
import { ConfirmDialog, Sheet } from "@/components/ui/overlays";
import { useJobActions } from "@/lib/actions";
import {
  formatBytes,
  formatDateTime,
  formatDuration,
  formatEta,
  formatRelative,
  middleTruncate,
  percentOf,
} from "@/lib/format";
import { HW_API_LABEL, JOB_STAGE_LABEL, JOB_STAGES, VALIDATION_LABEL } from "@/lib/labels";
import { overallProgress } from "@/lib/progress";
import { useJob, useLibraries, useSettings } from "@/lib/queries";
import { href } from "@/lib/router";
import { useLiveJob } from "@/lib/store";
import type { Job, JobStage, ValidationCheck, ValidationReport } from "@/lib/types";
import { cn } from "@/lib/utils";

/** Where a job is in Preparing → Converting → Checking quality → Finishing. */
function StageSteps({ stage }: { stage: JobStage }) {
  const current = JOB_STAGES.indexOf(stage);
  return (
    <ol className="flex flex-wrap items-center gap-x-1.5 gap-y-1 text-xs" aria-label="Steps">
      {JOB_STAGES.map((s, i) => {
        const done = current > i;
        const active = current === i;
        return (
          <li key={s} className="flex items-center gap-1.5">
            {i > 0 ? <span aria-hidden className={cn("h-px w-3", done || active ? "bg-accent-ink/60" : "bg-line")} /> : null}
            <span
              aria-current={active ? "step" : undefined}
              className={cn("inline-flex items-center gap-1", active ? "font-semibold text-fg" : "text-muted")}
            >
              {/* Done: a check. Current: a gold dot. Not reached: a hollow ring. */}
              {done ? (
                <Check className="size-3 text-success" aria-hidden />
              ) : (
                <span
                  aria-hidden
                  className={cn(
                    "size-1.5 rounded-full",
                    active ? "bg-accent-ink" : "border border-line-strong bg-transparent",
                  )}
                />
              )}
              {JOB_STAGE_LABEL[s]}
              {done ? <span className="sr-only"> (done)</span> : null}
            </span>
          </li>
        );
      })}
    </ol>
  );
}

function speedText(job: Job): string | null {
  const parts = [
    // Slow CPU encodes run below 10 fps; "0 fps" would read as stuck.
    job.fps && job.fps >= 0.05 ? `${job.fps >= 10 ? Math.round(job.fps) : job.fps.toFixed(1)} fps` : null,
    job.speed ? `${job.speed >= 10 ? Math.round(job.speed) : job.speed.toFixed(1)}×` : null,
  ].filter(Boolean);
  return parts.length ? parts.join(" · ") : null;
}

/**
 * Stop a running job (after asking: its work so far is lost) or take a
 * queued one out of the queue (one click; nothing is lost).
 */
export function CancelJobButton({
  job,
  size = "sm",
  variant = "quiet",
}: {
  job: Job;
  size?: "sm" | "md";
  variant?: "quiet" | "secondary";
}) {
  const { cancel } = useJobActions();
  const [confirm, setConfirm] = useState(false);
  if (job.state === "queued") {
    return (
      <Button variant={variant} size={size} onClick={() => cancel.mutate(job)} loading={cancel.isPending}>
        <CircleSlash aria-hidden />
        Remove from queue
      </Button>
    );
  }
  return (
    <>
      <Button variant={variant} size={size} onClick={() => setConfirm(true)}>
        <X aria-hidden />
        Cancel
      </Button>
      <ConfirmDialog
        open={confirm}
        onOpenChange={setConfirm}
        title={`Stop converting ${middleTruncate(job.file_name, 60)}?`}
        confirmLabel="Stop converting"
        cancelLabel="Keep converting"
        destructive
        loading={cancel.isPending}
        onConfirm={() => cancel.mutate(job, { onSettled: () => setConfirm(false) })}
      >
        <p>The work done on it so far is discarded. The original file is untouched.</p>
        <p>It won&apos;t be converted again unless you queue it.</p>
      </ConfirmDialog>
    </>
  );
}

/** Whole-file progress for a job (see `overallProgress`). */
export function jobOverall(job: Pick<Job, "stage" | "progress">): number {
  return overallProgress(job.stage, job.progress);
}

/** "Checking quality · 27%": the stage and how far into it, as secondary detail. */
function stageDetail(job: Job): string {
  const stage = JOB_STAGE_LABEL[job.stage];
  return job.stage === "transcoding" || job.stage === "waiting" ? stage : `${stage} · ${Math.round(job.progress)}%`;
}

/** Live card for a running job. */
export function JobCard({
  job: baseJob,
  onOpen,
  announce = false,
}: {
  job: Job;
  onOpen: (job: Job) => void;
  /** Read progress to screen readers (only one card per screen should). */
  announce?: boolean;
}) {
  const job = useLiveJob(baseJob);
  const libraries = useLibraries();
  const libraryName = libraries.data?.find((l) => l.id === job.library_id)?.name;
  const eta = formatEta(job.eta_secs);
  const speed = speedText(job);
  const stage = JOB_STAGE_LABEL[job.stage];
  const overall = jobOverall(job);

  useThrottledAnnouncement(
    announce ? `${job.file_name}: ${Math.round(overall)}% done, ${stage.toLowerCase()}${eta ? `, ${eta}` : ""}.` : null,
    `${job.id}:${job.stage}`,
  );

  return (
    <article className="flex flex-col gap-3.5 rounded-lg border border-line bg-surface p-4 shadow-card sm:p-5">
      <div className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <button
            type="button"
            onClick={() => onOpen(job)}
            className="block max-w-full truncate text-left text-[0.9375rem] font-semibold text-fg hover:text-accent-ink"
            title={job.file_name}
          >
            {job.file_name}
          </button>
          <p className="mt-0.5 truncate text-[0.8125rem] text-muted">
            {libraryName ? `${libraryName} · ` : ""}
            {formatBytes(job.input_size)}
          </p>
        </div>
        <EncoderBadge api={job.hw_api} encoder={job.encoder} />
      </div>

      <StageSteps stage={job.stage} />

      <div>
        <Meter value={overall} label={`${job.file_name}: whole file`} live size="md" />
        <div className="mt-2 flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 text-[0.8125rem]">
          <p className="text-fg">
            <span className="font-semibold tabular">{Math.round(overall)}%</span>
            <span className="text-muted"> · {eta ?? stage.toLowerCase()}</span>
          </p>
          <p className="text-xs text-muted tabular">
            {job.stage === "transcoding" ? null : <span>{stageDetail(job)}</span>}
            {speed ? <span className="font-mono">{job.stage === "transcoding" ? "" : " · "}{speed}</span> : null}
          </p>
        </div>
        {job.attempt > 1 ? (
          <p className="mt-2 text-[0.8125rem] text-warning">
            Attempt {job.attempt}: the first try didn&apos;t work, so Chrysopoeia is trying another way.
          </p>
        ) : null}
      </div>

      <div className="flex items-center justify-end gap-2 border-t border-line pt-3">
        <CancelJobButton job={job} />
        <Button variant="secondary" size="sm" onClick={() => onOpen(job)}>
          Details
        </Button>
      </div>
    </article>
  );
}

/** Placeholder with the card's shape. */
export function JobCardSkeleton() {
  return (
    <div className="flex flex-col gap-4 rounded-lg border border-line bg-surface p-5">
      <Skeleton className="h-4 w-2/3" />
      <Skeleton className="h-3 w-1/3" />
      <Skeleton className="h-2.5 w-full rounded-full" />
      <Skeleton className="h-3 w-1/2" />
    </div>
  );
}

/** "Saved 1.2 GB (38%)" or "1.1 GB larger" for a finished job. */
export function savingsText(input: number, output: number | null): { text: string; saved: boolean } | null {
  if (output === null) return null;
  const diff = input - output;
  if (diff >= 0) {
    return { text: `Saved ${formatBytes(diff)} (${Math.round(percentOf(diff, input))}%)`, saved: true };
  }
  return { text: `${formatBytes(-diff)} larger`, saved: false };
}

export function SheetSection({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="mt-7 first:mt-0">
      <h3 className="mb-2.5 text-sm font-semibold text-fg">{title}</h3>
      {children}
    </section>
  );
}

function BeforeAfter({ job }: { job: Job }) {
  const kept = job.state === "done";
  const savings = savingsText(job.input_size, job.output_size);
  return (
    <div className="rounded-lg border border-line bg-sunken/50 p-4">
      <div className="flex items-center justify-between gap-3">
        <div>
          <p className="text-xs text-muted">Before</p>
          <p className="mt-0.5 text-lg font-semibold text-fg tabular">{formatBytes(job.input_size)}</p>
        </div>
        <ArrowRight className="size-5 shrink-0 text-muted" aria-label="became" />
        <div className="text-right">
          <p className="text-xs text-muted">{kept || job.output_size === null ? "After" : "New file (not kept)"}</p>
          <p className="mt-0.5 text-lg font-semibold text-fg tabular">
            {job.output_size === null ? "—" : formatBytes(job.output_size)}
          </p>
        </div>
      </div>
      {savings ? (
        <>
          <Meter
            className="mt-3"
            value={job.output_size === null ? 0 : Math.min(100, percentOf(job.output_size, job.input_size))}
            label="New size compared with the original"
            tone={!kept ? "muted" : savings.saved ? "accent" : "danger"}
          />
          <p
            className={cn(
              "mt-2 text-sm font-medium",
              !kept ? "text-muted" : savings.saved ? "text-accent-ink" : "text-danger",
            )}
          >
            {kept
              ? savings.text
              : `Would have been ${Math.round(Math.abs(percentOf(job.input_size - (job.output_size ?? 0), job.input_size)))}% ${
                  savings.saved ? "smaller" : "larger"
                }. The original was kept.`}
          </p>
        </>
      ) : null}
    </div>
  );
}

/** Tone for the similarity numbers, from the worker's own verdict on the visual check. */
function metricTone(report: ValidationReport): "danger" | "warning" | null {
  const visual = report.checks.find((c) => c.id === "visual");
  if (visual?.status === "fail") return "danger";
  if (visual?.status === "warn") return "warning";
  return null;
}

/** A check's measured value in words, or `null` when the sentence already says it all. */
export function checkValueText(check: ValidationCheck): string | null {
  const v = check.value;
  if (v === null || !Number.isFinite(v)) return null;
  switch (check.id) {
    case "duration":
      return v < 0.05 ? null : `${v < 10 ? v.toFixed(1) : Math.round(v)} s off`;
    case "visual":
      return `similarity ${v.toFixed(2)}`;
    case "black_frames":
    case "frozen_frames":
      return v < 0.05 ? null : `+${v < 10 ? v.toFixed(1) : Math.round(v)} s`;
    default:
      return null;
  }
}

function Verification({ job }: { job: Job }) {
  const report = job.validation;
  if (!report) {
    return (
      <p className="text-sm text-muted">
        {job.state === "running" || job.state === "queued"
          ? "Checks run after converting."
          : "No checks ran for this file."}
      </p>
    );
  }
  const tone = metricTone(report);
  const metrics = [
    report.ssim_min !== null
      ? { label: "Lowest similarity", value: report.ssim_min.toFixed(3), hint: "SSIM, 1 = identical" }
      : null,
    report.ssim_avg !== null ? { label: "Average similarity", value: report.ssim_avg.toFixed(3), hint: "SSIM" } : null,
    report.psnr_avg !== null ? { label: "Signal to noise", value: `${report.psnr_avg.toFixed(1)} dB`, hint: "PSNR" } : null,
  ].filter((m): m is { label: string; value: string; hint: string } => m !== null);
  return (
    <div className="overflow-hidden rounded-lg border border-line">
      <p className="border-b border-line px-3.5 py-2.5 text-[0.8125rem] text-muted">
        {VALIDATION_LABEL[report.level]} checks, took {formatDuration(report.elapsed_secs)}
      </p>
      {metrics.length ? (
        // One row of numbers with dividers, inside the checks box rather than
        // as separate cards.
        <dl className="grid grid-cols-3 divide-x divide-line border-b border-line">
          {metrics.map((m) => (
            <div key={m.label} className="min-w-0 px-3.5 py-2.5">
              <dt className="text-xs leading-snug text-muted">{m.label}</dt>
              <dd
                className={cn(
                  "mt-0.5 font-mono text-sm font-medium tabular",
                  tone === "danger" ? "text-danger" : tone === "warning" ? "text-warning" : "text-fg",
                )}
              >
                {m.value}
                {tone ? <span className="sr-only"> ({tone === "danger" ? "failed the check" : "a warning"})</span> : null}
              </dd>
              <dd className="text-[0.6875rem] text-muted">{m.hint}</dd>
            </div>
          ))}
        </dl>
      ) : null}
      <ul className="divide-y divide-line">
        {report.checks.map((check) => {
          const value = checkValueText(check);
          return (
            <li key={check.id} className="flex gap-3 px-3.5 py-3">
              <CheckIcon status={check.status} />
              <div className="min-w-0 flex-1">
                <p className="text-sm font-medium text-fg">{check.label}</p>
                <p className="mt-0.5 text-[0.8125rem] leading-snug text-muted">{check.detail}</p>
              </div>
              {value ? <span className="shrink-0 text-xs text-muted tabular">{value}</span> : null}
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/** Whether a failure message says the original itself was affected. */
function originalAffected(error: string | null): boolean {
  return /original[^.]*\b(missing|deleted|removed|damaged|modified|overwritten|lost)\b/i.test(error ?? "");
}

function Outcome({ job }: { job: Job }) {
  const settings = useSettings();
  if (job.state === "failed") {
    return (
      <Callout tone="danger" title="This file couldn't be converted">
        <p>{job.error ?? "ffmpeg stopped with an error."}</p>
        {originalAffected(job.error) ? null : <p className="mt-1.5">Your original file is untouched.</p>}
      </Callout>
    );
  }
  if (job.state === "skipped") {
    return (
      <Callout tone="info" title="Kept the original">
        <p>{job.skip_reason ?? "The new file wasn't worth keeping."}</p>
        <p className="mt-1.5">
          That follows the library&apos;s settings, so converting it again would end the same way. To keep results like
          this, change the goal or the minimum savings in the library settings.
        </p>
      </Callout>
    );
  }
  if (job.state === "cancelled") {
    return (
      <Callout tone="info" title="Cancelled">
        The original file was left as it is.
      </Callout>
    );
  }
  if (job.state === "done") {
    const toFolder = settings.data?.output_mode === "folder";
    const verified = Boolean(job.validation?.passed);
    return (
      <Callout
        tone="success"
        title={verified ? (toFolder ? "Verified and saved" : "Verified and replaced") : toFolder ? "Saved" : "Replaced"}
      >
        {verified
          ? toFolder
            ? "The new file passed every check and was saved to the output folder. The original is untouched."
            : "The new file passed every check before it took the original's place."
          : toFolder
            ? "The new file was saved to the output folder. Verification was off, so it wasn't checked."
            : "The new file took the original's place. Verification was off, so it wasn't checked."}
      </Callout>
    );
  }
  return null;
}

function JobSheetBody({ job: baseJob }: { job: Job }) {
  const job = useLiveJob(baseJob);
  const libraries = useLibraries();
  const library = libraries.data?.find((l) => l.id === job.library_id);
  const eta = formatEta(job.eta_secs);
  const overall = jobOverall(job);
  return (
    <>
      <div className="mb-6 flex flex-wrap items-center gap-2">
        <JobStateBadge job={job} />
        <EncoderBadge api={job.hw_api} encoder={job.encoder} />
        <span className="text-[0.8125rem] text-muted">
          {job.finished_at
            ? `Finished ${formatRelative(job.finished_at)}`
            : job.started_at
              ? `Started ${formatRelative(job.started_at)}`
              : `Added ${formatRelative(job.created_at)}`}
        </span>
      </div>

      {job.state === "running" ? (
        <div className="mb-6">
          <StageSteps stage={job.stage} />
          <Meter className="mt-3" value={overall} label="Whole file" live size="md" />
          <p className="mt-2 text-[0.8125rem] text-muted">
            <span className="font-semibold text-fg tabular">{Math.round(overall)}%</span>
            {eta ? ` · ${eta}` : ""}
            {job.stage !== "transcoding" ? ` · ${stageDetail(job)}` : ""}
            {speedText(job) ? <span className="font-mono text-xs"> · {speedText(job)}</span> : null}
          </p>
        </div>
      ) : null}

      <Outcome job={job} />

      {job.notes && job.notes.length > 0 ? (
        <SheetSection title="What changed">
          <ul className="divide-y divide-line overflow-hidden rounded-lg border border-line">
            {job.notes.map((note, i) => (
              <li key={`${i}-${note}`} className="flex gap-3 px-3.5 py-3 text-sm leading-snug text-fg">
                <Info className="mt-0.5 size-4 shrink-0 text-info" aria-hidden />
                <span className="min-w-0">{note}</span>
              </li>
            ))}
          </ul>
        </SheetSection>
      ) : null}

      <SheetSection title="Size">
        <BeforeAfter job={job} />
      </SheetSection>

      <SheetSection title="Checks">
        <Verification job={job} />
      </SheetSection>

      <SheetSection title="Details">
        <dl className="divide-y divide-line">
          <Detail label="Library">{library?.name ?? "—"}</Detail>
          <Detail label="File" mono>
            {job.file_path}
          </Detail>
          <Detail label="Done by">
            {job.hw_api ? HW_API_LABEL[job.hw_api] : "—"}
            {job.encoder ? <span className="ml-1.5 font-mono text-xs text-muted"><span className="sr-only">, encoder </span>{job.encoder}</span> : null}
          </Detail>
          {job.attempt > 1 ? <Detail label="Attempts">{job.attempt}</Detail> : null}
          <Detail label="Added">{formatDateTime(job.created_at)}</Detail>
          {job.started_at ? <Detail label="Started">{formatDateTime(job.started_at)}</Detail> : null}
          {job.finished_at ? <Detail label="Finished">{formatDateTime(job.finished_at)}</Detail> : null}
          {job.started_at && job.finished_at ? (
            <Detail label="Took">
              {formatDuration((Date.parse(job.finished_at) - Date.parse(job.started_at)) / 1000)}
            </Detail>
          ) : null}
        </dl>
      </SheetSection>

      {job.command ? (
        <SheetSection title="ffmpeg command">
          <CodeBlock code={job.command} label="Command of the final attempt" />
        </SheetSection>
      ) : null}

      {job.log_tail ? (
        <SheetSection title="Log">
          <CodeBlock code={job.log_tail} label="Last lines from ffmpeg" wrap={false} maxHeight="20rem" />
        </SheetSection>
      ) : null}
    </>
  );
}

/** Queue a finished file again, asking first when that means a second lossy pass. */
export function ConvertAgainButton({
  fileName,
  onConfirm,
  loading,
  label = "Convert again",
}: {
  fileName: string;
  onConfirm: () => void;
  loading: boolean;
  label?: string;
}) {
  const settings = useSettings();
  const [confirm, setConfirm] = useState(false);
  const toFolder = settings.data?.output_mode === "folder";
  return (
    <>
      <Button variant="secondary" size="sm" onClick={() => setConfirm(true)}>
        <RotateCcw aria-hidden />
        {label}
      </Button>
      <ConfirmDialog
        open={confirm}
        onOpenChange={setConfirm}
        title={`Convert ${middleTruncate(fileName, 60)} again?`}
        confirmLabel="Convert again"
        loading={loading}
        onConfirm={() => {
          onConfirm();
          setConfirm(false);
        }}
      >
        {toFolder ? (
          <p>The original is converted again and the file in the output folder is replaced with the new result.</p>
        ) : (
          <>
            <p>This converts the already-converted file again. Quality can drop a little each time a file is converted.</p>
            <p>Usually this is only worth it after changing the library&apos;s goal.</p>
          </>
        )}
      </ConfirmDialog>
    </>
  );
}

function JobSheetActions({ job }: { job: Job }) {
  const { moveToTop, retry } = useJobActions();
  // A plain link: the new address has no `?job=`, which closes this sheet.
  const fileLink = href(`/library/${job.library_id}`, { file: job.file_id });
  return (
    <>
      <a href={fileLink} className={cn(buttonVariants({ variant: "quiet", size: "sm" }), "mr-auto")}>
        <FileVideo aria-hidden />
        Show file
      </a>
      {job.state === "queued" ? (
        <>
          <CancelJobButton job={job} variant="secondary" />
          <Button variant="primary" size="sm" onClick={() => moveToTop.mutate(job)} loading={moveToTop.isPending}>
            <ArrowUpToLine aria-hidden />
            Move to top
          </Button>
        </>
      ) : null}
      {job.state === "running" ? <CancelJobButton job={job} variant="secondary" /> : null}
      {job.state === "failed" || job.state === "cancelled" ? (
        <Button
          variant={job.state === "failed" ? "primary" : "secondary"}
          size="sm"
          onClick={() => retry.mutate(job)}
          loading={retry.isPending}
        >
          <RotateCcw aria-hidden />
          {job.state === "failed" ? "Try again" : "Queue again"}
        </Button>
      ) : null}
      {job.state === "skipped" ? (
        // Queueing it again would reach the same verdict; the settings decide.
        <a
          href={href(`/library/${job.library_id}/settings`)}
          className={buttonVariants({ variant: "secondary", size: "sm" })}
        >
          <SlidersHorizontal aria-hidden />
          Library settings
        </a>
      ) : null}
      {job.state === "done" ? (
        <ConvertAgainButton fileName={job.file_name} onConfirm={() => retry.mutate(job)} loading={retry.isPending} />
      ) : null}
    </>
  );
}

/** Detail sheet for one job, opened from the queue, overview or a file. */
export function JobSheet({ jobId, onClose }: { jobId: string | null; onClose: () => void }) {
  const query = useJob(jobId);
  const job = query.data;
  return (
    <Sheet
      open={Boolean(jobId)}
      onOpenChange={(open) => !open && onClose()}
      title={job ? middleTruncate(job.file_name, 80) : "Loading…"}
      description={job ? "Conversion details" : undefined}
      footer={job ? <JobSheetActions job={job} /> : undefined}
    >
      {job ? (
        <JobSheetBody job={job} />
      ) : query.error ? (
        <Callout tone="danger" title="Couldn't load this job">
          It may have been cleared from the history.
        </Callout>
      ) : (
        <div className="space-y-4">
          <Skeleton className="h-6 w-40" />
          <Skeleton className="h-24 w-full" />
          <Skeleton className="h-40 w-full" />
        </div>
      )}
    </Sheet>
  );
}
