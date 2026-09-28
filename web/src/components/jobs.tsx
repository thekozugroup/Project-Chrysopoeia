"use client";

/**
 * Job views: the live "now converting" card and the detail sheet with the
 * before/after, verification report, ffmpeg command and log.
 */

import { ArrowRight, Check, CircleSlash, FileVideo, RotateCcw, ArrowUpToLine, X } from "lucide-react";
import type { ReactNode } from "react";
import { useThrottledAnnouncement } from "@/components/providers";
import { CheckIcon, EncoderBadge, JobStateBadge } from "@/components/status";
import { Button, buttonVariants } from "@/components/ui/button";
import { Callout, CodeBlock, Detail, Meter, Skeleton } from "@/components/ui/display";
import { Sheet } from "@/components/ui/overlays";
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
import { useJob, useLibraries } from "@/lib/queries";
import { href } from "@/lib/router";
import { useLiveJob } from "@/lib/store";
import type { Job, JobStage } from "@/lib/types";
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
              className={cn(
                "inline-flex items-center gap-1",
                active ? "font-semibold text-fg" : done ? "text-muted" : "text-muted/70",
              )}
            >
              {done ? (
                <Check className="size-3 text-success" aria-hidden />
              ) : (
                <span
                  aria-hidden
                  className={cn("size-1.5 rounded-full", active ? "bg-accent-ink" : "bg-line-strong/60")}
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
    job.fps ? `${Math.round(job.fps)} fps` : null,
    job.speed ? `${job.speed >= 10 ? Math.round(job.speed) : job.speed.toFixed(1)}×` : null,
  ].filter(Boolean);
  return parts.length ? parts.join(" · ") : null;
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
  const { cancel } = useJobActions();
  const eta = formatEta(job.eta_secs);
  const speed = speedText(job);
  const stage = JOB_STAGE_LABEL[job.stage];

  useThrottledAnnouncement(
    announce ? `${job.file_name}: ${stage} ${Math.round(job.progress)}%${eta ? `, ${eta}` : ""}.` : null,
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
        <Meter value={job.progress} label={`${job.file_name}: ${stage}`} live size="md" />
        <div className="mt-2 flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 text-[0.8125rem]">
          <p className="text-fg">
            <span className="font-semibold tabular">{Math.round(job.progress)}%</span>
            <span className="text-muted"> {eta ? `· ${eta}` : `· ${stage.toLowerCase()}`}</span>
          </p>
          {speed ? <p className="font-mono text-xs text-muted tabular">{speed}</p> : null}
        </div>
        {job.attempt > 1 ? (
          <p className="mt-2 text-[0.8125rem] text-warning">
            Attempt {job.attempt}: the first try didn&apos;t work, so Chrysopoeia is trying another way.
          </p>
        ) : null}
      </div>

      <div className="flex items-center justify-end gap-2 border-t border-line pt-3">
        <Button variant="quiet" size="sm" onClick={() => cancel.mutate(job)} loading={cancel.isPending}>
          <X aria-hidden />
          Cancel
        </Button>
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
  const metrics = [
    report.ssim_min !== null
      ? {
          label: "Lowest similarity",
          value: report.ssim_min.toFixed(3),
          hint: "SSIM, 1.000 = identical",
          bad: report.ssim_min < 0.9,
        }
      : null,
    report.ssim_avg !== null
      ? { label: "Average similarity", value: report.ssim_avg.toFixed(3), hint: "SSIM", bad: report.ssim_avg < 0.95 }
      : null,
    report.psnr_avg !== null
      ? { label: "Signal to noise", value: `${report.psnr_avg.toFixed(1)} dB`, hint: "PSNR", bad: false }
      : null,
  ].filter((m): m is { label: string; value: string; hint: string; bad: boolean } => m !== null);
  return (
    <div>
      <p className="mb-3 text-[0.8125rem] text-muted">
        {VALIDATION_LABEL[report.level]} checks · took {formatDuration(report.elapsed_secs)}
      </p>
      {metrics.length ? (
        <dl className="mb-4 grid grid-cols-3 gap-2">
          {metrics.map((m) => (
            <div key={m.label} className="rounded-md border border-line px-3 py-2">
              <dt className="text-xs text-muted">{m.label}</dt>
              <dd className={cn("mt-0.5 font-mono text-sm font-medium tabular", m.bad ? "text-danger" : "text-fg")}>
                {m.value}
                {m.bad ? <span className="sr-only"> (below the passing mark)</span> : null}
              </dd>
              <dd className="text-[0.6875rem] text-muted">{m.hint}</dd>
            </div>
          ))}
        </dl>
      ) : null}
      <ul className="divide-y divide-line rounded-lg border border-line">
        {report.checks.map((check) => (
          <li key={check.id} className="flex gap-3 px-3.5 py-3">
            <CheckIcon status={check.status} />
            <div className="min-w-0 flex-1">
              <p className="text-sm font-medium text-fg">{check.label}</p>
              <p className="mt-0.5 text-[0.8125rem] leading-snug text-muted">{check.detail}</p>
            </div>
            {check.value !== null ? (
              <span className="shrink-0 font-mono text-xs text-muted tabular">
                {Number.isInteger(check.value) ? check.value : check.value.toFixed(3)}
              </span>
            ) : null}
          </li>
        ))}
      </ul>
    </div>
  );
}

function Outcome({ job }: { job: Job }) {
  if (job.state === "failed") {
    return (
      <Callout tone="danger" title="This file couldn't be converted">
        <p>{job.error ?? "ffmpeg stopped with an error."}</p>
        {/original/i.test(job.error ?? "") ? null : <p className="mt-1.5">Your original file is untouched.</p>}
      </Callout>
    );
  }
  if (job.state === "skipped") {
    return (
      <Callout tone="info" title="Kept the original">
        {job.skip_reason ?? "The new file wasn't worth keeping."}
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
  if (job.state === "done" && job.validation?.passed) {
    return (
      <Callout tone="success" title="Verified and replaced">
        The new file passed every check before it took the original&apos;s place.
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
          <Meter className="mt-3" value={job.progress} label={JOB_STAGE_LABEL[job.stage]} live size="md" />
          <p className="mt-2 text-[0.8125rem] text-muted">
            <span className="font-semibold text-fg tabular">{Math.round(job.progress)}%</span>
            {eta ? ` · ${eta}` : ""}
            {speedText(job) ? <span className="font-mono text-xs"> · {speedText(job)}</span> : null}
          </p>
        </div>
      ) : null}

      <Outcome job={job} />

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
            {job.encoder ? <span className="ml-1.5 font-mono text-xs text-muted">{job.encoder}</span> : null}
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

function JobSheetActions({ job, onClose }: { job: Job; onClose: () => void }) {
  const { cancel, moveToTop, retry } = useJobActions();
  const fileLink = href(`/library/${job.library_id}`, { file: job.file_id });
  return (
    <>
      <a href={fileLink} onClick={onClose} className={cn(buttonVariants({ variant: "quiet", size: "sm" }), "mr-auto")}>
        <FileVideo aria-hidden />
        Show file
      </a>
      {job.state === "queued" ? (
        <>
          <Button variant="secondary" size="sm" onClick={() => cancel.mutate(job)} loading={cancel.isPending}>
            <CircleSlash aria-hidden />
            Remove from queue
          </Button>
          <Button variant="primary" size="sm" onClick={() => moveToTop.mutate(job)} loading={moveToTop.isPending}>
            <ArrowUpToLine aria-hidden />
            Move to top
          </Button>
        </>
      ) : null}
      {job.state === "running" ? (
        <Button variant="secondary" size="sm" onClick={() => cancel.mutate(job)} loading={cancel.isPending}>
          <X aria-hidden />
          Cancel
        </Button>
      ) : null}
      {job.state === "failed" || job.state === "cancelled" || job.state === "skipped" || job.state === "done" ? (
        <Button
          variant={job.state === "failed" ? "primary" : "secondary"}
          size="sm"
          onClick={() => retry.mutate(job)}
          loading={retry.isPending}
        >
          <RotateCcw aria-hidden />
          {job.state === "failed" ? "Try again" : "Convert again"}
        </Button>
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
      footer={job ? <JobSheetActions job={job} onClose={onClose} /> : undefined}
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
