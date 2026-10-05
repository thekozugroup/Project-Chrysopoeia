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
  CircleX,
  FileVideo,
  Info,
  LoaderCircle,
  Play,
  RotateCcw,
  Wrench,
} from "lucide-react";
import { useState, type ReactNode } from "react";
import { FileName } from "@/components/file-name";
import { useThrottledAnnouncement } from "@/components/providers";
import { CheckIcon, EncoderBadge, JobStateBadge } from "@/components/status";
import { Button, buttonVariants } from "@/components/ui/button";
import { Callout, CodeBlock, CopyButton, Detail, Disclosure, Meter, Skeleton } from "@/components/ui/display";
import { ConfirmDialog, Sheet } from "@/components/ui/overlays";
import { useFileActions, useJobActions } from "@/lib/actions";
import {
  attemptHeading,
  attemptOutcome,
  attemptRows,
  jobReportText,
  lastFailedCheck,
  lowerFirst,
} from "@/lib/attempts";
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
  CHANGED_FALLBACK,
  CHANGED_TITLE,
  KEPT_CONVERTED,
  convertAnywayDetails,
  failureGroup,
  failureNote,
  isUnreadable,
  jobStanding,
  newFileName,
  replaceLoss,
  setupFix,
  setupProblem,
  skipNote,
  skipSummary,
  unreadableDetail,
  type Failure,
  type JobStanding,
  type SetupProblem,
} from "@/lib/outcomes";
import { framesElapsedText, framesText, overallProgress, readProgress } from "@/lib/progress";
import {
  useFile,
  useHardwareInfo,
  useJob,
  useLibraries,
  useLibrary,
  useNewFileName,
  useSettings,
} from "@/lib/queries";
import { href } from "@/lib/router";
import { useLiveJob } from "@/lib/store";
import type {
  GpuDevice,
  Job,
  JobStage,
  MediaFile,
  TranscodeProfile,
  ValidationCheck,
  ValidationReport,
} from "@/lib/types";
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
  named = false,
}: {
  job: Job;
  size?: "sm" | "md";
  variant?: "quiet" | "secondary";
  /** "Stop" on a card, "Stop converting" in the sheet. */
  label?: string;
  /**
   * Add the file's name to the accessible name ("Stop The Office…"), for a
   * card among others whose buttons would otherwise all be called "Stop".
   */
  named?: boolean;
}) {
  const { cancel } = useJobActions();
  const [confirm, setConfirm] = useState(false);
  if (job.state === "queued") {
    return (
      <Button
        variant={variant}
        size={size}
        onClick={() => cancel.mutate(job)}
        loading={cancel.isPending}
        aria-label={named ? `Remove from queue: ${job.file_name}` : undefined}
        needsServer
      >
        <CircleMinus aria-hidden />
        Remove from queue
      </Button>
    );
  }
  return (
    <>
      <Button
        variant={variant}
        size={size}
        onClick={() => setConfirm(true)}
        aria-label={named ? `${label} ${job.file_name}` : undefined}
        needsServer
      >
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

/**
 * A running job's progress in words, matching its bar: "37%", "About 37% ·
 * estimated from 1,017 frames", or, while the share isn't known, "1,017
 * frames · 7 min elapsed" (never a 0% that only looks like a number). The
 * time left only goes with a share.
 */
export function progressWords(job: Pick<Job, "stage" | "progress" | "progress_basis" | "frames" | "elapsed_secs" | "eta_secs">): {
  lead: string;
  detail: string | null;
  eta: string | null;
} {
  const reading = readProgress(job);
  switch (reading.kind) {
    case "unknown":
      return {
        lead: framesElapsedText(reading.frames, reading.elapsedSecs),
        detail: "how far isn't known yet",
        eta: null,
      };
    case "estimated":
      return {
        lead: `About ${Math.round(reading.overall)}%`,
        detail: reading.frames !== null ? `estimated from ${framesText(reading.frames)}` : "estimated from the frames encoded",
        eta: formatEta(job.eta_secs),
      };
    default:
      return { lead: `${Math.round(reading.overall)}%`, detail: null, eta: formatEta(job.eta_secs) };
  }
}

/** The bar and the line under it for a running job (see `progressWords`). */
function JobProgressBlock({ job, label, lineClassName }: { job: Job; label: string; lineClassName?: string }) {
  const reading = readProgress(job);
  const words = progressWords(job);
  const said = [words.lead, words.detail, words.eta].filter(Boolean).join(", ");
  return (
    <>
      <Meter
        value={reading.kind === "unknown" ? null : reading.overall}
        valueText={reading.kind === "measured" ? undefined : said}
        label={label}
        live
        size="md"
      />
      <p className={cn("text-[0.8125rem] text-fg", lineClassName)}>
        <span className="font-semibold tabular">{words.lead}</span>
        {words.detail ? <span className="text-muted"> · {words.detail}</span> : null}
        {words.eta ? <span className="text-muted"> · {words.eta}</span> : null}
      </p>
    </>
  );
}

/** "Attempt 2: …" on a running job's card, with the check the last try failed when it did. */
function attemptLine(job: Job): string {
  const check = lastFailedCheck(job);
  if (check) {
    return `Attempt ${job.attempt}: the last try made a file that failed a check (${check.label}), so Chrysopoeia is trying another way.`;
  }
  return `Attempt ${job.attempt}: the first try didn't work, so Chrysopoeia is trying another way.`;
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
  const stage = JOB_STAGE_LABEL[job.stage];
  const words = progressWords(job);
  const done = readProgress(job).kind === "unknown" ? words.lead : `${words.lead} done`;

  useThrottledAnnouncement(
    announce ? `${job.file_name}: ${done}, ${stage.toLowerCase()}${words.eta ? `, ${words.eta}` : ""}.` : null,
    `${job.id}:${job.stage}`,
  );

  const titleId = `job-card-${job.id}`;
  return (
    // `min-w-0`: a long name is cut to the card's width instead of widening it past the screen.
    <article
      aria-labelledby={titleId}
      className="flex min-w-0 flex-col gap-3.5 rounded-lg border border-line bg-surface p-4 shadow-card sm:p-5"
    >
      <div className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <button
            id={titleId}
            type="button"
            onClick={() => onOpen(job)}
            className="flex max-w-full text-left text-[0.9375rem] font-semibold text-fg hover:text-accent-ink"
          >
            <FileName name={job.file_name} />
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
        <JobProgressBlock job={job} label={`${job.file_name}: whole file`} lineClassName="mt-2" />
        {job.attempt > 1 ? <p className="mt-2 text-[0.8125rem] text-warning">{attemptLine(job)}</p> : null}
      </div>

      {/* Named after the file: with several cards, "Stop" and "Details" alone would be ambiguous. */}
      <div className="flex items-center justify-end gap-2 border-t border-line pt-3">
        <StopJobButton job={job} named />
        <Button variant="secondary" size="sm" onClick={() => onOpen(job)} aria-label={`Details for ${job.file_name}`}>
          Details
        </Button>
      </div>
    </article>
  );
}

/** Placeholder with the card's shape. */
export function JobCardSkeleton() {
  return (
    <div className="flex min-w-0 flex-col gap-4 rounded-lg border border-line bg-surface p-5">
      <Skeleton className="h-4 w-2/3" />
      <Skeleton className="h-3 w-1/3" />
      <Skeleton className="h-2.5 w-full rounded-full" />
      <Skeleton className="h-3 w-1/2" />
    </div>
  );
}

/**
 * "Saved 1.2 GB (38%)" or "1.1 GB larger" for a finished job. `freed` is
 * what the server says the conversion released (`Job.freed_bytes`): 0 means
 * nothing was (the original was hard-linked, so replacing it freed no
 * space), and then no saving is claimed however the two sizes compare; a
 * number is the saving; `null` or nothing (a job from before the server
 * recorded it) reads the sizes. A result that grew is still said, whatever
 * `freed` is.
 */
export function savingsText(
  input: number,
  output: number | null,
  freed?: number | null,
): { text: string; saved: boolean } | null {
  if (output === null) return null;
  const diff = input - output;
  if (freed === 0 && diff >= 0) return null;
  const gained = typeof freed === "number" && freed > 0 ? freed : diff;
  if (gained >= 0) {
    return { text: `Saved ${formatBytes(gained)} (${Math.round(percentOf(gained, input))}%)`, saved: true };
  }
  return { text: `${formatBytes(-gained)} larger`, saved: false };
}

/**
 * Whether a finished job made a new file but freed no disk space: the
 * server's figure is 0 and the file didn't grow (a result that did says so
 * by itself, see `savingsText`).
 */
export function noSpaceFreed(job: Pick<Job, "input_size" | "output_size" | "freed_bytes">): boolean {
  return job.freed_bytes === 0 && job.output_size !== null && job.output_size <= job.input_size;
}

export { lowerFirst };

/**
 * The second line of a finished job in a list: what came of it, briefly,
 * the reason first so a narrow column keeps the useful part. Its badge
 * already says "Kept original" or "Skipped", so a skip says why: "6%
 * smaller (needs at least 10%)" (`minSavingsPct` is the library's current
 * minimum, to name the rule). `standing` (see `jobStanding`) adds what
 * happened since: "· now converted", "· queued again", or, for a second
 * conversion a setup problem stopped (its badge reads "Needs a fix"), "·
 * converted file kept".
 */
export function historyNote(job: Job, minSavingsPct?: number | null, standing: JobStanding = "current"): string {
  const since = standing === "converted" ? " · now converted" : standing === "queued" ? " · queued again" : "";
  switch (job.state) {
    case "done":
      return (
        savingsText(job.input_size, job.output_size, job.freed_bytes)?.text ??
        (noSpaceFreed(job) ? "Converted, no space freed" : "Converted")
      );
    case "failed": {
      const note = failureNote(job);
      if (standing === "kept") return setupProblem(job) ? `${note} · converted file kept` : note;
      return `${note}${since}`;
    }
    case "skipped": {
      const note = skipNote(job.skip_reason, minSavingsPct);
      if (standing === "kept") return note ?? "Not worth converting again";
      return `${note ?? (job.output_size !== null ? "Kept the original" : "No conversion needed")}${since}`;
    }
    default:
      if (standing === "kept") return "Stopped. The converted file is unchanged.";
      return since ? `Stopped${since}` : "Stopped. The original was left as it is.";
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
  const savings = savingsText(job.input_size, output, kept ? job.freed_bytes : null);
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
      ) : kept && noSpaceFreed(job) ? (
        // The server's figure beats the two sizes: a hard-linked original's data stays on disk.
        <p className="mt-3 text-sm font-medium text-muted">No space was freed.</p>
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
 * its cause as the title, the server's sentence (which names the exact
 * cause and fix: a folder without write access, a file in the way, a full
 * disk) and a link to the setting. A generic fix only when there's no
 * sentence. `kept`: a second conversion it stopped, so the file is still
 * the converted one.
 */
function SetupCallout({ kind, error, kept = false }: { kind: SetupProblem; error: string | null; kept?: boolean }) {
  const settings = useSettings();
  const fix = setupFix(kind, settings.data?.output_mode);
  const text = error?.trim();
  return (
    <Callout
      tone="warning"
      title={fix.title}
      action={
        <a href={href(fix.setting.path, { focus: fix.setting.focus })} className={buttonVariants({ variant: "secondary", size: "sm" })}>
          <Wrench aria-hidden />
          {fix.setting.label}
        </a>
      }
    >
      <p>{text || `${fix.fix} Then try again. The original is untouched.`}</p>
      {kept ? (
        <p className="mt-1.5">It was a second conversion, so the file stays as it was converted before. Nothing was lost.</p>
      ) : null}
    </Callout>
  );
}

/**
 * Why a file or job failed, by cause (see `failureGroup`): a damaged
 * original, a setup problem with its fix, a file moved or replaced
 * meanwhile, or a failed conversion under `title`.
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
      // The server's sentence says what happened (moved, deleted, replaced).
      return (
        <Callout tone="info" title={CHANGED_TITLE}>
          <p>{failure.error?.trim() || CHANGED_FALLBACK}</p>
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

/**
 * A second conversion that left the already converted file as it was. One
 * a setup problem stopped gets that problem's fix and setting instead.
 */
function KeptConvertedCallout({ job, minSavingsPct }: { job: Job; minSavingsPct?: number | null }) {
  const setup = job.state === "failed" ? setupProblem(job) : null;
  if (setup) return <SetupCallout kind={setup} error={job.error} kept />;
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

/** "The file has been converted since…" / "…queued again": an old outcome the file has moved past. */
function MovedOnCallout({ job, standing }: { job: Job; standing: "converted" | "queued" }) {
  const after =
    standing === "converted"
      ? "The file has been converted since, so there's nothing to do here."
      : "The file is queued to be converted again.";
  return (
    <Callout tone="info" title={job.state === "failed" ? "This attempt failed" : "Stopped"}>
      {job.state === "failed" && job.error ? <p>{job.error}</p> : null}
      <p className={job.state === "failed" && job.error ? "mt-1.5" : undefined}>{after}</p>
    </Callout>
  );
}

function Outcome({ job, standing, renamed }: { job: Job; standing: JobStanding; renamed: string | null }) {
  const settings = useSettings();
  const { library } = useLibrary(job.library_id);
  if (standing === "unknown") return <Skeleton className="h-20 w-full rounded-lg" />;
  if (standing === "kept") return <KeptConvertedCallout job={job} minSavingsPct={library?.profile.min_savings_pct} />;
  if ((standing === "converted" || standing === "queued") && job.state !== "skipped") {
    return <MovedOnCallout job={job} standing={standing} />;
  }
  if (job.state === "failed") return <FailureCallout failure={job} title="This file couldn't be converted" />;
  if (job.state === "skipped") {
    // A new file was made and thrown away (the size rule), or the file never
    // needed work under the library's settings.
    const summary = skipSummary(job.skip_reason, job.output_size !== null, library?.profile.min_savings_pct);
    return (
      <Callout tone="info" title={summary.title}>
        <p>{summary.body}</p>
        {standing === "converted" ? <p className="mt-1.5">The file has been converted since.</p> : null}
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
        <p>
          {verified
            ? toFolder
              ? "The new file passed every check and was saved to the output folder. The original is untouched."
              : "The new file passed every check before it took the original's place."
            : toFolder
              ? "The new file was saved to the output folder. Checks were off, so it wasn't checked."
              : "The new file took the original's place. Checks were off, so it wasn't checked."}
        </p>
        {renamed ? (
          // Another format, another extension: say so, or the old name is searched for in vain.
          <p className="mt-1.5">
            It&apos;s now called <span className="font-medium break-all text-fg">{renamed}</span>.
          </p>
        ) : null}
      </Callout>
    );
  }
  return null;
}

/**
 * Every way the job tried, in plain words: "Attempt 1 · AMD GPU (VA-API) ·
 * decoded on the GPU · 2 min" and "Failed: Plays start to finish: playback
 * stopped at 1:01 of 2:21:02", with the one still running last. Shown when
 * there was more than one (see `attemptRows`).
 */
export function Attempts({ job, gpus }: { job: Job; gpus: readonly GpuDevice[] }) {
  const rows = attemptRows(job, gpus);
  if (!rows.length) return null;
  return (
    <SheetSection title="Attempts">
      <ol aria-label="Attempts" className="divide-y divide-line overflow-hidden rounded-lg border border-line">
        {rows.map(({ attempt, running }) =>
          attempt ? (
            <li key={attempt.attempt} className="flex gap-3 px-3.5 py-3">
              {attempt.result === "succeeded" ? (
                <Check className="mt-0.5 size-4 shrink-0 text-success" aria-hidden />
              ) : (
                <CircleX className="mt-0.5 size-4 shrink-0 text-danger" aria-hidden />
              )}
              <div className="min-w-0">
                <p className="text-sm font-medium text-fg">{attemptHeading(attempt, gpus)}</p>
                <p className="mt-0.5 text-[0.8125rem] leading-snug break-words text-muted">{attemptOutcome(attempt)}</p>
              </div>
            </li>
          ) : running ? (
            <li key={`running-${running.attempt}`} className="flex gap-3 px-3.5 py-3">
              <LoaderCircle className="spin mt-0.5 size-4 shrink-0 text-accent-ink" aria-hidden />
              <div className="min-w-0">
                <p className="text-sm font-medium text-fg">{`Attempt ${running.attempt} · ${running.device}`}</p>
                <p className="mt-0.5 text-[0.8125rem] leading-snug text-muted">Converting now</p>
              </div>
            </li>
          ) : null,
        )}
      </ol>
    </SheetSection>
  );
}

/** Encoder, similarity scores, every attempt's command and log: everything an expert or a bug report needs. */
function JobTechnical({ job, gpus }: { job: Job; gpus: readonly GpuDevice[] }) {
  const report = job.validation;
  const log = job.log_tail ? errorLinesFirst(job.log_tail) : null;
  const speed = job.state === "running" ? speedText(job) : null;
  // The final attempt's command and log are shown below as before; with
  // more than one attempt, each one's own are shown too.
  const attempts = (job.attempts ?? []).length > 1 ? (job.attempts ?? []) : [];
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
  if (!rows.length && !job.command && !log && !attempts.length) return null;
  return (
    <Disclosure className="mt-7">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-[0.8125rem] text-muted">Everything below, with every attempt, in one piece of text.</p>
        <CopyButton text={jobReportText(job, gpus)} label="Copy for a bug report" />
      </div>
      {rows.length ? <dl className="-my-2 divide-y divide-line">{rows}</dl> : null}
      {log?.errors ? <CodeBlock code={log.errors} label="What ffmpeg reported" /> : null}
      {job.command ? <CodeBlock code={job.command} label="ffmpeg command of the final attempt" /> : null}
      {log ? <CodeBlock code={log.rest} label="Last lines from ffmpeg" maxHeight="20rem" /> : null}
      {attempts.map((attempt) => (
        <section key={attempt.attempt} aria-label={`Attempt ${attempt.attempt}`} className="flex flex-col gap-2">
          <p className="text-[0.8125rem] font-medium text-fg">
            {attemptHeading(attempt, gpus)}
            <span className="font-normal text-muted">
              {" "}
              · <span className="font-mono text-xs">{attempt.encoder}</span> · {attemptOutcome(attempt)}
            </span>
          </p>
          {attempt.command ? <CodeBlock code={attempt.command} label={`Attempt ${attempt.attempt}: ffmpeg command`} /> : null}
          {attempt.log_tail ? (
            <CodeBlock code={attempt.log_tail} label={`Attempt ${attempt.attempt}: last lines from ffmpeg`} maxHeight="12rem" />
          ) : null}
        </section>
      ))}
    </Disclosure>
  );
}

/** Where this job stands now (see `jobStanding`), from its file; a file that can't be read leaves the job as it is. */
function useStanding(job: Job): JobStanding {
  const file = useFile(job.file_id);
  if (file.isError && !file.data) return "current";
  return jobStanding(job, file.data);
}

function JobSheetBody({ job: baseJob }: { job: Job }) {
  const job = useLiveJob(baseJob);
  const libraries = useLibraries();
  const library = libraries.data?.find((l) => l.id === job.library_id);
  const standing = useStanding(job);
  const settings = useSettings();
  const file = useFile(job.file_id).data?.file;
  const renamed = newFileName(job, file, settings.data?.output_mode);
  const gpus = useHardwareInfo().hw?.gpus ?? [];
  return (
    <>
      <div className="mb-6 flex flex-wrap items-center gap-2">
        <JobStateBadge job={job} standing={standing} />
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
          <JobProgressBlock job={job} label="Whole file" />
        </div>
      ) : null}

      <Outcome job={job} standing={standing} renamed={renamed} />

      <Attempts job={job} gpus={gpus} />

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
          <Detail label={renamed ? "Original" : "File"} mono>
            {job.file_path}
          </Detail>
          {renamed ? (
            <Detail label="Now" mono>
              {job.file_path.slice(0, job.file_path.lastIndexOf("/") + 1) + renamed}
            </Detail>
          ) : null}
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

      <JobTechnical job={job} gpus={gpus} />
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
 * efficient, kept because the result wasn't smaller, or left unchanged
 * because the new format can't hold some of its tracks): one conversion
 * without those rules, after a confirmation. When tracks would be lost, it
 * says plainly which, and that they're gone for good.
 */
export function ConvertAnywayButton({ file, variant = "secondary" }: { file: MediaFile; variant?: "secondary" | "primary" }) {
  const { convertAnyway } = useFileActions();
  const [confirm, setConfirm] = useState(false);
  const details = convertAnywayDetails(file.skip_reason);
  const loses = replaceLoss(file.skip_reason) !== null;
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
        {details?.map((line) => <p key={line}>{line}</p>)}
        {loses ? null : (
          <p>
            {details
              ? "It's converted once anyway."
              : "It's converted once, ignoring this library's rules for skipping files."}{" "}
            The usual checks still run before anything is replaced.
          </p>
        )}
      </ConfirmDialog>
    </>
  );
}

/**
 * "Skip this file": a damaged original is left alone until it's replaced
 * ("Skipped by you", the same word as every other skip; "Ignored files" in
 * Settings are the ignore patterns, a different thing).
 */
export function SkipUnreadableButton({ file, variant = "primary" }: { file: MediaFile; variant?: "primary" | "secondary" }) {
  const { skipUnreadable } = useFileActions();
  return (
    <Button
      variant={variant}
      size="sm"
      onClick={() => skipUnreadable.mutate(file)}
      loading={skipUnreadable.isPending}
      needsServer
    >
      <CircleMinus aria-hidden />
      Skip this file
    </Button>
  );
}

function JobSheetActions({ job }: { job: Job }) {
  const { moveToTop, retry } = useJobActions();
  const file = useFile(job.file_id).data?.file;
  const { library } = useLibrary(job.library_id);
  // A plain link: the new address has no `?job=`, which closes this sheet.
  const fileLink = href(`/library/${job.library_id}`, { file: job.file_id });
  const standing = useStanding(job);
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
  // Until the file is read, only the way to it: a converted file must never be queued again unasked.
  const current = standing === "current";
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
      {(standing === "kept" || standing === "converted") && file?.status === "done" ? (
        // The file is converted: converting it once more asks first.
        <ConvertAgainAction
          file={file}
          profile={library?.profile}
          onConfirm={() => retry.mutate(job)}
          loading={retry.isPending}
        />
      ) : null}
      {current && job.state === "failed" && unreadable ? (
        <>
          {retryButton(false)}
          {file?.status === "failed" ? <SkipUnreadableButton file={file} /> : null}
        </>
      ) : null}
      {current && job.state === "failed" && !unreadable ? retryButton(true) : null}
      {current && job.state === "cancelled" ? retryButton(false) : null}
      {current && job.state === "skipped" && file && skipFollowsSettings(file) ? <ConvertAnywayButton file={file} /> : null}
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
  // A result with a new name is called by it, the old one secondary: "land1080.mkv", "was land1080.mp4".
  const renamed = useNewFileName(job);
  return (
    <Sheet
      open={Boolean(jobId)}
      onOpenChange={(open) => !open && onClose()}
      title={job ? middleTruncate(renamed ?? job.file_name, 80) : "Loading…"}
      description={
        job ? (
          renamed ? (
            <>
              Conversion details · <span className="break-all">was {job.file_name}</span>
            </>
          ) : (
            "Conversion details"
          )
        ) : undefined
      }
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
