"use client";

/**
 * Status vocabulary: every state has an icon, a word and a tone, so colour
 * is never the only signal.
 */

import {
  Ban,
  CircleCheck,
  CircleDashed,
  CircleMinus,
  CircleX,
  Clock,
  Cpu,
  FileWarning,
  LoaderCircle,
  MonitorPlay,
  ShieldCheck,
  TriangleAlert,
} from "lucide-react";
import type { ReactNode } from "react";
import { Badge, type Tone } from "@/components/ui/display";
import { Tooltip } from "@/components/ui/overlays";
import { CHECK_STATUS_LABEL, FILE_STATUS_LABEL, HW_API_LABEL, JOB_STAGE_LABEL, JOB_STATE_LABEL } from "@/lib/labels";
import { isUnreadableSource } from "@/lib/outcomes";
import type { CheckStatus, FileStatus, HwApi, Job, JobState } from "@/lib/types";
import { cn } from "@/lib/utils";

const FILE_STATUS_STYLE: Record<FileStatus, { tone: Tone; icon: ReactNode }> = {
  pending: { tone: "neutral", icon: <CircleDashed aria-hidden /> },
  queued: { tone: "info", icon: <Clock aria-hidden /> },
  processing: { tone: "accent", icon: <LoaderCircle className="spin" aria-hidden /> },
  done: { tone: "success", icon: <CircleCheck aria-hidden /> },
  skipped: { tone: "neutral", icon: <CircleMinus aria-hidden /> },
  failed: { tone: "danger", icon: <CircleX aria-hidden /> },
};

/** Icon for a file status, for filter chips and compact rows. */
export function fileStatusIcon(status: FileStatus): ReactNode {
  return FILE_STATUS_STYLE[status].icon;
}

/** A failure that is the original's fault: amber, with the fix outside the app. */
function CantBeReadBadge() {
  return (
    <Badge tone="warning" icon={<FileWarning aria-hidden />}>
      Can&apos;t be read
    </Badge>
  );
}

export function FileStatusBadge({
  status,
  progress,
  error,
}: {
  status: FileStatus;
  progress?: number | null;
  /** The file's error, to tell a damaged original from a failed conversion. */
  error?: string | null;
}) {
  if (status === "failed" && isUnreadableSource(error)) return <CantBeReadBadge />;
  const style = FILE_STATUS_STYLE[status];
  const label =
    status === "processing" && progress !== null && progress !== undefined
      ? `${FILE_STATUS_LABEL[status]} ${Math.round(progress)}%`
      : FILE_STATUS_LABEL[status];
  return (
    <Badge tone={style.tone} icon={style.icon} className="tabular">
      {label}
    </Badge>
  );
}

const JOB_STATE_STYLE: Record<JobState, { tone: Tone; icon: ReactNode }> = {
  queued: { tone: "info", icon: <Clock aria-hidden /> },
  running: { tone: "accent", icon: <LoaderCircle className="spin" aria-hidden /> },
  done: { tone: "success", icon: <CircleCheck aria-hidden /> },
  skipped: { tone: "neutral", icon: <CircleMinus aria-hidden /> },
  failed: { tone: "danger", icon: <CircleX aria-hidden /> },
  cancelled: { tone: "neutral", icon: <Ban aria-hidden /> },
};

/**
 * Outcome of a job. Done and verified jobs read "Verified"; a skipped job
 * reads "Kept original" when a new file was made and thrown away (the size
 * rule), and "Skipped" when the file never needed work.
 */
export function JobStateBadge({
  job,
}: {
  job: Pick<Job, "state" | "stage" | "validation" | "output_size" | "error">;
}) {
  if (job.state === "failed" && isUnreadableSource(job.error)) return <CantBeReadBadge />;
  if (job.state === "done" && job.validation?.passed) {
    return (
      <Badge tone="success" icon={<ShieldCheck aria-hidden />}>
        Verified
      </Badge>
    );
  }
  const style = JOB_STATE_STYLE[job.state];
  const label =
    job.state === "running"
      ? JOB_STAGE_LABEL[job.stage]
      : job.state === "skipped" && job.output_size !== null
        ? "Kept original"
        : JOB_STATE_LABEL[job.state];
  return (
    <Badge tone={style.tone} icon={style.icon}>
      {label}
    </Badge>
  );
}

const CHECK_STYLE: Record<CheckStatus, { className: string; icon: ReactNode }> = {
  pass: { className: "text-success", icon: <CircleCheck aria-hidden /> },
  warn: { className: "text-warning", icon: <TriangleAlert aria-hidden /> },
  fail: { className: "text-danger", icon: <CircleX aria-hidden /> },
  skipped: { className: "text-muted", icon: <CircleMinus aria-hidden /> },
};

/** Icon for one verification check, with a hidden word for screen readers. */
export function CheckIcon({ status }: { status: CheckStatus }) {
  const style = CHECK_STYLE[status];
  return (
    <span className={cn("mt-0.5 shrink-0 [&_svg]:size-[1.125rem]", style.className)}>
      {style.icon}
      <span className="sr-only">{CHECK_STATUS_LABEL[status]}:</span>
    </span>
  );
}

/** Which hardware did the work: "NVIDIA GPU" with the encoder name as detail. */
export function EncoderBadge({ api, encoder }: { api: HwApi | null; encoder: string | null }) {
  if (!api && !encoder) return null;
  const hardware = api && api !== "software";
  const text = api ? HW_API_LABEL[api] : "Encoder";
  const badge = (
    <span
      tabIndex={encoder ? 0 : undefined}
      className={cn(
        "inline-flex h-6 shrink-0 items-center gap-1.5 rounded-full border px-2.5 text-xs font-medium whitespace-nowrap [&_svg]:size-3.5",
        hardware ? "border-accent-ink/35 text-accent-ink" : "border-line-strong/60 text-muted",
      )}
    >
      {hardware ? <MonitorPlay aria-hidden /> : <Cpu aria-hidden />}
      {text}
      {encoder ? <span className="sr-only"> (encoder {encoder})</span> : null}
    </span>
  );
  return encoder ? (
    <Tooltip
      content={
        <span>
          Encoder <code className="font-mono text-xs">{encoder}</code>
        </span>
      }
    >
      {badge}
    </Tooltip>
  ) : (
    badge
  );
}
