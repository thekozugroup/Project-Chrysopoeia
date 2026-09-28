"use client";

/**
 * Job views: the live "now converting" card and the detail sheet. Every
 * sheet reads in the same order: what happened, the size before and after,
 * the checks in plain sentences, then one closed "Technical details" with
 * the encoder, similarity scores, ffmpeg command and log.
 */

import {
  ArrowRight,
  ArrowUpToLine,
  Check,
  CircleMinus,
  CircleStop,
  FileVideo,
  Info,
  Play,
  RotateCcw,
  Wrench,
} from "lucide-react";
import { useState, type ReactNode } from "react";
import { useThrottledAnnouncement } from "@/components/providers";
import { CheckIcon, EncoderBadge, JobStateBadge } from "@/components/status";
import { Button, buttonVariants } from "@/components/ui/button";
import { Callout, CodeBlock, Detail, Disclosure, Meter, Skeleton } from "@/components/ui/display";
import { ConfirmDialog, Sheet } from "@/components/ui/overlays";
import { useFileActions, useJobActions } from "@/lib/actions";
import { convertsAgain, skipFollowsSettings } from "@/lib/convertible";
import {
  formatBytes,
  formatDateTime,
  formatDuration,
  formatEta,
  formatRelative,
  middleTruncate,
  percentOf,
} from "@/lib/format";
import { HW_API_LABEL, HW_API_TECH, JOB_STAGE_LABEL, JOB_STAGES, VALIDATION_LABEL } from "@/lib/labels";
import {
  CHANGED_TITLE,
  KEPT_CONVERTED,
  failureGroup,
  failureNote,
  isUnreadable,
  keptAsConverted,
  setupFix,
  setupProblem,
  skipNote,
  skipSummary,
  unreadableDetail,
  type Failure,
  type SetupProblem,
} from "@/lib/outcomes";
import { overallProgress } from "@/lib/progress";
import { useFile, useJob, useLibraries, useLibrary, useSettings } from "@/lib/queries";
import { href } from "@/lib/router";
import { useLive, useLiveJob } from "@/lib/store";
import type { Job, JobStage, MediaFile, TranscodeProfile, ValidationCheck, ValidationReport } from "@/lib/types";
import { cn, useRetained } from "@/lib/utils";

/**
 * Where a job is in Preparing → Converting → Checking quality → Finishing.
 * Phones get one line ("Checking quality · step 3 of 4") instead of the
 * stepper, which would wrap.
 */
function StageSteps({ stage }: { stage: JobStage }) {
  const current = JOB_STAGES.indexOf(stage);
  return (
    <>
      <p className="text-[0.8125rem] text-muted sm:hidden">
        <span className="font-medium text-fg">{JOB_STAGE_LABEL[stage]}</span>
        {current >= 0 ? ` · step ${current + 1} of ${JOB_STAGES.length}` : ""}
      </p>
      <ol className="hidden flex-wrap items-center gap-x-1.5 gap-y-1 text-xs sm:flex" aria-label="Steps">
        {JOB_STAGES.map((s, i) => {
          const done = current > i;
          const active = current === i;
          return (
            <li key={s} className="flex items-center gap-1.5">
              {i > 0 ? (
                <span aria-hidden className={cn("h-px w-3", done || active ? "bg-accent-ink/60" : "bg-line")} />
              ) : null}
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
    </>
  );
}

/** "235 fps · 9.8× real time", for the technical details. */
function speedText(job: Pick<Job, "fps" | "speed">): string | null {
  const parts = [
    // Slow CPU encodes run below 10 fps; "0 fps" would read as stuck.
    job.fps && job.fps >= 0.05 ? `${job.fps >= 10 ? Math.round(job.fps) : job.fps.toFixed(1)} fps` : null,
    job.speed ? `${job.speed >= 10 ? Math.round(job.speed) : job.speed.toFixed(1)}× real time` : null,
  ].filter(Boolean);
  return parts.length ? parts.join(" · ") : null;
}

/**
 * Stop a running job (after asking: its work so far is lost) or take a
 * queued one out of the queue (one click; nothing is lost). "Stop", not
 * "Cancel", so it can't be mistaken for closing a dialog.
 */
export function StopJobButton({
  job,
  size = "sm",
  variant = "quiet",
  label = "Stop",
}: {
  job: Job;
  size?: "sm" | "md";
  variant?: "quiet" | "secondary";
  /** "Stop" on a card, "Stop converting" in the sheet. */
  label?: string;
}) {
  const { cancel } = useJobActions();
  const [confirm, setConfirm] = useState(false);
  if (job.state === "queued") {
    return (
      <Button variant={variant} size={size} onClick={() => cancel.mutate(job)} loading={cancel.isPending} needsServer>
        <CircleMinus aria-hidden />
        Remove from queue
      </Button>
    );
  }
  return (
    <>
      <Button variant={variant} size={size} onClick={() => setConfirm(true)} needsServer>
        <CircleStop aria-hidden />
        {label}
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
        <p className="mt-2 text-[0.8125rem] text-fg">
          <span className="font-semibold tabular">{Math.round(overall)}%</span>
          {eta ? <span className="text-muted"> · {eta}</span> : null}
        </p>
        {job.attempt > 1 ? (
          <p className="mt-2 text-[0.8125rem] text-warning">
            Attempt {job.attempt}: the first try didn&apos;t work, so Chrysopoeia is trying another way.
          </p>
        ) : null}
      </div>

      <div className="flex items-center justify-end gap-2 border-t border-line pt-3">
        <StopJobButton job={job} />
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

/** "The disk is full" → "the disk is full", to follow a colon; "NVENC …" and "HEVC" stay as they are. */
export function lowerFirst(text: string): string {
  if (/^[A-Z]{2}/.test(text)) return text;
  return text.charAt(0).toLowerCase() + text.slice(1);
}

/**
 * The second line of a finished job in a list: what came of it, briefly.
 * Its badge already says "Kept original" or "Skipped", so a skip says why:
 * "6% smaller (needs at least 10%)" (`minSavingsPct` is the
 * library's current minimum, to name the rule). `kept` marks a second
 * conversion that left the converted file as it was (its badge reads
 * "Kept as converted").
 */
export function historyNote(job: Job, minSavingsPct?: number | null, kept = false): string {
  switch (job.state) {
    case "done":
      return savingsText(job.input_size, job.output_size)?.text ?? "Converted";
    case "failed":
      return kept ? `Converting it again failed: ${lowerFirst(failureNote(job))}` : failureNote(job);
    case "skipped": {
      const note = skipNote(job.skip_reason, minSavingsPct);
      if (kept) return note ? `Converting it again wasn't worth it: ${lowerFirst(note)}` : "Converting it again wasn't worth it";
      return note ?? (job.output_size !== null ? "Kept the original" : "No conversion needed");
    }
    default:
      return kept ? "Stopped. The converted file is unchanged." : "Stopped. The original was left as it is.";
  }
}

export function SheetSection({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="mt-7 first:mt-0">
      <h3 className="mb-2.5 text-sm font-semibold text-fg">{title}</h3>
      {children}
    </section>
  );
}

/** Before and after sizes; only for jobs that produced a new file. */
function BeforeAfter({ job, output }: { job: Job; output: number }) {
  const kept = job.state === "done";
  const savings = savingsText(job.input_size, output);
  return (
    <div className="rounded-lg border border-line bg-sunken/50 p-4">
      <div className="flex items-center justify-between gap-3">
        <div>
          <p className="text-[0.8125rem] text-muted">Before</p>
          <p className="mt-0.5 text-lg font-semibold text-fg tabular">{formatBytes(job.input_size)}</p>
        </div>
        <ArrowRight className="size-5 shrink-0 text-muted" aria-label="became" />
        <div className="text-right">
          <p className="text-[0.8125rem] text-muted">{kept ? "After" : "New file (not kept)"}</p>
          <p className="mt-0.5 text-lg font-semibold text-fg tabular">{formatBytes(output)}</p>
        </div>
      </div>
      {savings ? (
        <>
          <Meter
            className="mt-3"
            value={Math.min(100, percentOf(output, job.input_size))}
            label="New size compared with the original"
            tone={!kept ? "muted" : savings.saved ? "accent" : "danger"}
          />
          {/* A kept original's outcome above already says by how much and why. */}
          {kept ? (
            <p className={cn("mt-2 text-sm font-medium", savings.saved ? "text-accent-ink" : "text-danger")}>
              {savings.text}
            </p>
          ) : null}
        </>
      ) : null}
    </div>
  );
}

/** A check's measured value in words, or `null` when the sentence already says it all. */
export function checkValueText(check: ValidationCheck): string | null {
  const v = check.value;
  if (v === null || !Number.isFinite(v)) return null;
  switch (check.id) {
    case "duration":
      return v < 0.05 ? null : `${v < 10 ? v.toFixed(1) : Math.round(v)} s off`;
    case "black_frames":
    case "frozen_frames":
      return v < 0.05 ? null : `+${v < 10 ? v.toFixed(1) : Math.round(v)} s`;
    default:
      // Similarity scores live in the technical details.
      return null;
  }
}

/** "99.6%": a similarity score (0..1) as a percentage people can read. */
function similarityPercent(ssim: number): string {
  const pct = ssim * 100;
  return `${pct >= 99.95 ? "100" : pct.toFixed(1)}%`;
}

/**
 * The checks' verdict in one sentence: "Looks the same as the original:
 * 99.6% similar at its lowest point."
 */
export function checksLead(report: ValidationReport): string {
  const visual = report.checks.find((c) => c.id === "visual");
  const lowest = report.ssim_min ?? visual?.value ?? null;
  if (visual && lowest !== null && Number.isFinite(lowest)) {
    if (visual.status === "fail") {
      return `Looked different from the original: only ${similarityPercent(lowest)} similar at its lowest point.`;
    }
    return `Looks the same as the original: ${similarityPercent(lowest)} similar at its lowest point.`;
  }
  const failure = report.checks.find((c) => c.status === "fail");
  if (failure) return `Failed a check: ${failure.label.charAt(0).toLowerCase()}${failure.label.slice(1)}.`;
  return report.passed ? "Passed every check." : "Didn't pass its checks.";
}

function Checks({ report }: { report: ValidationReport }) {
  return (
    <div className="overflow-hidden rounded-lg border border-line">
      <p className="border-b border-line px-3.5 py-3 text-sm font-medium text-fg">{checksLead(report)}</p>
      <ul className="divide-y divide-line">
        {report.checks.map((check) => {
          const value = checkValueText(check);
          return (
            <li key={check.id} className="flex gap-3 px-3.5 py-3">
              <CheckIcon status={check.status} />
              <div className="min-w-0 flex-1">
                <p className="text-sm text-fg">{check.label}</p>
                <p className="mt-0.5 text-[0.8125rem] leading-snug text-muted">{check.detail}</p>
              </div>
              {value ? <span className="shrink-0 text-xs text-muted tabular">{value}</span> : null}
            </li>
          );
        })}
      </ul>
      <p className="border-t border-line px-3.5 py-2.5 text-[0.8125rem] text-muted">
        {VALIDATION_LABEL[report.level]} checks · took {formatDuration(report.elapsed_secs)}
      </p>
    </div>
  );
}

/** Whether a failure message says the original itself was affected. */
function originalAffected(error: string | null): boolean {
  return /original[^.]*\b(missing|deleted|removed|damaged|modified|overwritten|lost)\b/i.test(error ?? "");
}

/** Lines of an ffmpeg log that say what went wrong. */
const ERROR_LINE = /\b(error|errors|failed|invalid|corrupt|cannot|could not|couldn't|unable|no such|denied|not found|unsupported)\b/i;

/** The log with the lines that name the problem first, so a bug report starts with them. */
export function errorLinesFirst(log: string): { errors: string | null; rest: string } {
  const lines = log.split("\n");
  const errors = lines.filter((line) => ERROR_LINE.test(line));
  return { errors: errors.length ? errors.join("\n") : null, rest: log };
}

/** What happens next for a file whose original can't be read. */
export function CantBeReadCallout({ error }: { error: string | null }) {
  const found = unreadableDetail(error)?.replace(/\.$/, "");
  const detail = found ? `${found.charAt(0).toLowerCase()}${found.slice(1)}` : null;
  return (
    <Callout tone="warning" title="Can't be read">
      <p>
        This file looks damaged or isn&apos;t a video{detail ? ` (${detail})` : ""}.
        Chrysopoeia left it alone.
      </p>
      <p className="mt-1.5">
        Play it in Plex or Jellyfin to check. If it&apos;s broken, replace it; the new copy is picked up automatically.
      </p>
    </Callout>
  );
}

/**
 * A setup problem (work folder, destination, disk space, chosen hardware):
 * its cause, the server's sentence, the exact fix and a link to the setting.
 */
function SetupCallout({ kind, error }: { kind: SetupProblem; error: string | null }) {
  const settings = useSettings();
  const fix = setupFix(kind, settings.data?.output_mode);
  return (
    <Callout
      tone="warning"
      title={fix.title}
      action={
        <a href={href(fix.setting.path)} className={buttonVariants({ variant: "secondary", size: "sm" })}>
          <Wrench aria-hidden />
          {fix.setting.label}
        </a>
      }
    >
      {error ? <p>{error}</p> : null}
      <p className={error ? "mt-1.5" : undefined}>
        {fix.fix} Then try again. The original is untouched.
      </p>
    </Callout>
  );
}

/**
 * Why a file or job failed, by cause (see `failureGroup`): a damaged
 * original, a setup problem with its fix, a file that changed meanwhile,
 * or a failed conversion under `title`.
 */
export function FailureCallout({ failure, title }: { failure: Failure; title: string }) {
  switch (failureGroup(failure)) {
    case "unreadable":
      return <CantBeReadCallout error={failure.error} />;
    case "setup": {
      const kind = setupProblem(failure);
      if (kind) return <SetupCallout kind={kind} error={failure.error} />;
      break;
    }
    case "changed":
      return (
        <Callout tone="info" title={CHANGED_TITLE}>
          <p>
            It was being copied or replaced at the time, so nothing was replaced. Try again once it has finished
            changing.
          </p>
        </Callout>
      );
    default:
      break;
  }
  return (
    <Callout tone="danger" title={title}>
      <p>{failure.error ?? "ffmpeg stopped with an error."}</p>
      {originalAffected(failure.error) ? null : <p className="mt-1.5">Your original file is untouched.</p>}
    </Callout>
  );
}

/** A second conversion that left the already converted file as it was. */
function KeptConvertedCallout({ job, minSavingsPct }: { job: Job; minSavingsPct?: number | null }) {
  const note = job.state === "skipped" ? skipNote(job.skip_reason, minSavingsPct) : null;
  const what =
    job.state === "failed"
      ? "Converting it again failed."
      : job.state === "cancelled"
        ? "Converting it again was stopped."
        : note
          ? `Converting it again wasn't worth it: ${lowerFirst(note)}.`
          : "Converting it again wasn't worth it.";
  return (
    <Callout tone="info" title={KEPT_CONVERTED}>
      <p>{what} The file stays as it was converted before; nothing was lost.</p>
      {job.state === "failed" && job.error ? <p className="mt-1.5">{job.error}</p> : null}
    </Callout>
  );
}

function Outcome({ job, kept }: { job: Job; kept: boolean }) {
  const settings = useSettings();
  const { library } = useLibrary(job.library_id);
  const forced = useLive((s) => Boolean(s.forced[job.id]));
  if (kept) return <KeptConvertedCallout job={job} minSavingsPct={library?.profile.min_savings_pct} />;
  if (job.state === "failed") return <FailureCallout failure={job} title="This file couldn't be converted" />;
  if (job.state === "skipped") {
    // A new file was made and thrown away (the size rule), or the file never
    // needed work under the library's settings.
    const summary = skipSummary(job.skip_reason, job.output_size !== null, library?.profile.min_savings_pct);
    return (
      <Callout tone="info" title={summary.title}>
        <p>{summary.body}</p>
        {forced ? (
          <p className="mt-1.5">
            Convert anyway didn&apos;t take effect: this server still applied the library&apos;s rules. Update the
            Chrysopoeia container to convert files like this one.
          </p>
        ) : null}
      </Callout>
    );
  }
  if (job.state === "cancelled") {
    return (
      <Callout tone="info" title="Stopped">
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
            ? "The new file was saved to the output folder. Checks were off, so it wasn't checked."
            : "The new file took the original's place. Checks were off, so it wasn't checked."}
      </Callout>
    );
  }
  return null;
}

/** Encoder, similarity scores, command and log: everything an expert or a bug report needs. */
function JobTechnical({ job }: { job: Job }) {
  const report = job.validation;
  const log = job.log_tail ? errorLinesFirst(job.log_tail) : null;
  const speed = job.state === "running" ? speedText(job) : null;
  const scores = [
    report?.ssim_min != null ? `lowest ${report.ssim_min.toFixed(3)}` : null,
    report?.ssim_avg != null ? `average ${report.ssim_avg.toFixed(3)}` : null,
  ].filter(Boolean);
  const rows = [
    job.encoder || job.hw_api ? (
      <Detail key="encoder" label="Encoder" mono>
        {[job.encoder, job.hw_api ? HW_API_TECH[job.hw_api] : null].filter(Boolean).join(" · ")}
      </Detail>
    ) : null,
    job.attempt > 1 ? (
      <Detail key="attempt" label="Attempt">
        {job.attempt}
      </Detail>
    ) : null,
    speed ? (
      <Detail key="speed" label="Speed" mono>
        {speed}
      </Detail>
    ) : null,
    scores.length ? (
      <Detail key="ssim" label="Similarity (SSIM)" mono>
        {scores.join(" · ")}
      </Detail>
    ) : null,
    report?.psnr_avg != null ? (
      <Detail key="psnr" label="Signal to noise (PSNR)" mono>
        {`${report.psnr_avg.toFixed(1)} dB`}
      </Detail>
    ) : null,
  ].filter(Boolean);
  if (!rows.length && !job.command && !log) return null;
  return (
    <Disclosure className="mt-7">
      {rows.length ? <dl className="-my-2 divide-y divide-line">{rows}</dl> : null}
      {log?.errors ? <CodeBlock code={log.errors} label="What ffmpeg reported" /> : null}
      {job.command ? <CodeBlock code={job.command} label="ffmpeg command of the final attempt" /> : null}
      {log ? <CodeBlock code={log.rest} label="Last lines from ffmpeg" maxHeight="20rem" /> : null}
    </Disclosure>
  );
}

/** Whether this job left an already converted file as it was (see `keptAsConverted`). */
function useKept(job: Job): boolean {
  const detail = useFile(job.file_id).data;
  return keptAsConverted(job, detail);
}

function JobSheetBody({ job: baseJob }: { job: Job }) {
  const job = useLiveJob(baseJob);
  const libraries = useLibraries();
  const library = libraries.data?.find((l) => l.id === job.library_id);
  const kept = useKept(job);
  const eta = formatEta(job.eta_secs);
  const overall = jobOverall(job);
  return (
    <>
      <div className="mb-6 flex flex-wrap items-center gap-2">
        <JobStateBadge job={job} kept={kept} />
        <span className="text-[0.8125rem] text-muted">
          {job.finished_at
            ? `Finished ${formatRelative(job.finished_at)}`
            : job.started_at
              ? `Started ${formatRelative(job.started_at)}`
              : `Added ${formatRelative(job.created_at)}`}
        </span>
      </div>

      {job.state === "running" ? (
        <div className="mb-6 flex flex-col gap-3">
          <StageSteps stage={job.stage} />
          <Meter value={overall} label="Whole file" live size="md" />
          <p className="text-[0.8125rem] text-muted">
            <span className="font-semibold text-fg tabular">{Math.round(overall)}%</span>
            {eta ? ` · ${eta}` : ""}
          </p>
        </div>
      ) : null}

      <Outcome job={job} kept={kept} />

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

      {job.output_size !== null ? (
        <SheetSection title="Size">
          <BeforeAfter job={job} output={job.output_size} />
        </SheetSection>
      ) : null}

      {job.validation ? (
        <SheetSection title="Checks">
          <Checks report={job.validation} />
        </SheetSection>
      ) : null}

      <SheetSection title="Details">
        <dl className="divide-y divide-line">
          <Detail label="Library">{library?.name ?? "—"}</Detail>
          <Detail label="File" mono>
            {job.file_path}
          </Detail>
          {job.hw_api ? <Detail label="Converted on">{HW_API_LABEL[job.hw_api]}</Detail> : null}
          {job.started_at && job.finished_at ? (
            <Detail label="Took">
              {formatDuration((Date.parse(job.finished_at) - Date.parse(job.started_at)) / 1000)}
            </Detail>
          ) : null}
          <Detail label="Added">{formatDateTime(job.created_at)}</Detail>
          {job.finished_at ? <Detail label="Finished">{formatDateTime(job.finished_at)}</Detail> : null}
        </dl>
      </SheetSection>

      <JobTechnical job={job} />
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
      <Button variant="secondary" size="sm" onClick={() => setConfirm(true)} needsServer>
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
          <p>
            It was converted for this library&apos;s old goal. Converting it again uses the new one; quality can drop a
            little each time.
          </p>
        )}
      </ConfirmDialog>
    </>
  );
}

/**
 * "Convert again" for a converted file, only when it would do anything: the
 * library's goal changed since, so the file isn't in its format any more.
 * A good result needs no homework, so otherwise there's nothing here.
 */
export function ConvertAgainAction({
  file,
  profile,
  onConfirm,
  loading,
}: {
  file: MediaFile;
  profile: TranscodeProfile | undefined;
  onConfirm: () => void;
  loading: boolean;
}) {
  if (!convertsAgain(file, profile)) return null;
  return <ConvertAgainButton fileName={file.file_name} onConfirm={onConfirm} loading={loading} />;
}

/**
 * "Convert anyway" for a file the library's settings skip (already
 * efficient, or kept because the result wasn't smaller): one conversion
 * without those rules, after a one-line confirmation.
 */
export function ConvertAnywayButton({ file, variant = "secondary" }: { file: MediaFile; variant?: "secondary" | "primary" }) {
  const { convertAnyway } = useFileActions();
  const [confirm, setConfirm] = useState(false);
  return (
    <>
      <Button variant={variant} size="sm" onClick={() => setConfirm(true)} needsServer>
        <Play aria-hidden />
        Convert anyway
      </Button>
      <ConfirmDialog
        open={confirm}
        onOpenChange={setConfirm}
        title={`Convert ${middleTruncate(file.file_name, 60)} anyway?`}
        confirmLabel="Convert anyway"
        loading={convertAnyway.isPending}
        onConfirm={() => convertAnyway.mutate(file, { onSettled: () => setConfirm(false) })}
      >
        <p>
          It&apos;s converted once, ignoring this library&apos;s rules for skipping files. The usual checks still run
          before anything is replaced.
        </p>
      </ConfirmDialog>
    </>
  );
}

/** "Ignore this file": a damaged original is left alone for good ("Skipped by you"). */
export function IgnoreFileButton({ file, variant = "primary" }: { file: MediaFile; variant?: "primary" | "secondary" }) {
  const { ignore } = useFileActions();
  return (
    <Button variant={variant} size="sm" onClick={() => ignore.mutate(file)} loading={ignore.isPending} needsServer>
      <CircleMinus aria-hidden />
      Ignore this file
    </Button>
  );
}

function JobSheetActions({ job }: { job: Job }) {
  const { moveToTop, retry } = useJobActions();
  const file = useFile(job.file_id).data?.file;
  const { library } = useLibrary(job.library_id);
  // A plain link: the new address has no `?job=`, which closes this sheet.
  const fileLink = href(`/library/${job.library_id}`, { file: job.file_id });
  const kept = useKept(job);
  const unreadable = job.state === "failed" && isUnreadable(job);
  const retryButton = (primary: boolean) => (
    <Button
      variant={primary ? "primary" : "secondary"}
      size="sm"
      onClick={() => retry.mutate(job)}
      loading={retry.isPending}
      needsServer
    >
      <RotateCcw aria-hidden />
      {job.state === "cancelled" ? "Queue again" : "Try again"}
    </Button>
  );
  return (
    <>
      <a href={fileLink} className={cn(buttonVariants({ variant: "quiet", size: "sm" }), "mr-auto")}>
        <FileVideo aria-hidden />
        Show file
      </a>
      {job.state === "queued" ? (
        <>
          <StopJobButton job={job} variant="secondary" />
          <Button
            variant="primary"
            size="sm"
            onClick={() => moveToTop.mutate(job)}
            loading={moveToTop.isPending}
            needsServer
          >
            <ArrowUpToLine aria-hidden />
            Move to top
          </Button>
        </>
      ) : null}
      {job.state === "running" ? <StopJobButton job={job} variant="secondary" label="Stop converting" /> : null}
      {kept && file ? (
        // The file is still converted: converting it once more asks first.
        <ConvertAgainAction
          file={file}
          profile={library?.profile}
          onConfirm={() => retry.mutate(job)}
          loading={retry.isPending}
        />
      ) : null}
      {!kept && job.state === "failed" && unreadable ? (
        <>
          {retryButton(false)}
          {file?.status === "failed" ? <IgnoreFileButton file={file} /> : null}
        </>
      ) : null}
      {!kept && job.state === "failed" && !unreadable ? retryButton(true) : null}
      {!kept && job.state === "cancelled" ? retryButton(false) : null}
      {!kept && job.state === "skipped" && file && skipFollowsSettings(file) ? <ConvertAnywayButton file={file} /> : null}
      {job.state === "done" && file ? (
        <ConvertAgainAction
          file={file}
          profile={library?.profile}
          onConfirm={() => retry.mutate(job)}
          loading={retry.isPending}
        />
      ) : null}
    </>
  );
}

/** Detail sheet for one job, opened from the queue, overview or a file. */
export function JobSheet({ jobId, onClose }: { jobId: string | null; onClose: () => void }) {
  // Keep showing the last job while the sheet animates closed.
  const shownId = useRetained(jobId);
  const query = useJob(shownId, Boolean(jobId));
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
