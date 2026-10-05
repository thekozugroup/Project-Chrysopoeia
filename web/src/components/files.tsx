"use client";

/**
 * File detail sheet: what happened to the file, its size, length and
 * picture, its tracks in plain words and its conversion history. Codec
 * names and raw stream data sit in one closed "Technical details".
 */

import { ArrowUpToLine, AudioLines, CircleMinus, Film, Captions, Play, RotateCcw } from "lucide-react";
import type { ReactNode } from "react";
import {
  ConvertAgainAction,
  ConvertAnywayButton,
  FailureCallout,
  SkipUnreadableButton,
  SheetSection,
  noSpaceFreed,
  savingsText,
} from "@/components/jobs";
import { WasName } from "@/components/file-name";
import { FileStatusBadge, JobStateBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Badge, Callout, Detail, Disclosure, Meter, Skeleton } from "@/components/ui/display";
import { Sheet } from "@/components/ui/overlays";
import { useFileActions } from "@/lib/actions";
import { convertsAgain, repeatsForce, skipFollowsSettings } from "@/lib/convertible";
import {
  formatBitrate,
  formatBytes,
  formatClock,
  formatDateTime,
  formatRelative,
  middleTruncate,
} from "@/lib/format";
import { channelsLabel, JOB_STAGE_LABEL, languageLabel, skippedByUser, sourceCodecLabel } from "@/lib/labels";
import {
  hdrSummary,
  hdrTechnical,
  isUnreadable,
  jobStanding,
  newFileName,
  skippedUnreadable,
  skipSummary,
} from "@/lib/outcomes";
import { useFile, useLibrary, useSettings } from "@/lib/queries";
import { openSheet } from "@/lib/router";
import { useFileLive } from "@/lib/store";
import type { FileDetail, Job, MediaFile, StreamInfo, TranscodeProfile } from "@/lib/types";
import { useRetained } from "@/lib/utils";

/**
 * One track. The name, the codec and the channels are always shown in full;
 * the optional title a release gave the track (often the whole release name)
 * is a line of its own that is cut short instead, so it can never push the
 * codec out of sight. Its full text is the tooltip.
 */
function StreamRow({
  icon,
  title,
  meta,
  note,
  tags,
}: {
  icon: ReactNode;
  title: ReactNode;
  meta?: ReactNode;
  /** The track's own title, when it has one. */
  note?: string | null;
  tags?: ReactNode;
}) {
  return (
    <li className="flex gap-3 px-3.5 py-3">
      <span className="mt-0.5 text-muted [&_svg]:size-4">{icon}</span>
      <div className="min-w-0 flex-1">
        <p className="text-sm text-fg">{title}</p>
        {meta ? <p className="mt-0.5 text-[0.8125rem] leading-snug text-muted">{meta}</p> : null}
        {note ? (
          <p className="mt-0.5 truncate text-[0.8125rem] leading-snug text-muted" title={note}>
            {note}
          </p>
        ) : null}
      </div>
      {tags ? <div className="flex shrink-0 flex-wrap items-start justify-end gap-1">{tags}</div> : null}
    </li>
  );
}

/** Raw ffprobe facts about a video stream, for the technical details. */
function videoMeta(s: StreamInfo): string {
  return [
    s.codec,
    s.profile,
    s.width && s.height ? `${s.width}×${s.height}` : null,
    s.bit_depth ? `${s.bit_depth}-bit` : null,
    s.frame_rate ? `${Number(s.frame_rate.toFixed(3))} fps` : null,
    s.pix_fmt,
    s.color_transfer,
    s.bit_rate ? formatBitrate(s.bit_rate) : null,
  ]
    .filter(Boolean)
    .join(" · ");
}

/** Raw ffprobe facts about an audio stream, for the technical details. */
function audioMeta(s: StreamInfo): string {
  return [
    s.codec,
    s.channel_layout ?? (s.channels ? `${s.channels} ch` : null),
    s.sample_rate ? `${(s.sample_rate / 1000).toFixed(1)} kHz` : null,
    s.bit_rate ? formatBitrate(s.bit_rate) : null,
  ]
    .filter(Boolean)
    .join(" · ");
}

/**
 * The saving shown for a converted file. The server's own figure wins: it
 * records 0 when nothing was freed (a hard-linked file, whose other links
 * still hold the old data), and then no saving is claimed.
 */
export function doneSavings(
  file: Pick<MediaFile, "original_size_bytes" | "saved_bytes" | "size_bytes">,
): ReturnType<typeof savingsText> {
  if (file.original_size_bytes === null || file.saved_bytes === 0) return null;
  const output = file.saved_bytes !== null ? file.original_size_bytes - file.saved_bytes : file.size_bytes;
  return savingsText(file.original_size_bytes, output);
}

/** "English audio", or just "Audio" when the track has no language tag. */
export function trackName(kind: "audio" | "subtitles", language: string | null): string {
  const known = language && language.toLowerCase() !== "und" ? languageLabel(language) : null;
  return known ? `${known} ${kind}` : kind === "audio" ? "Audio" : "Subtitles";
}

/** The codec in plain words: "E-AC-3", "HEVC", "PGS"; DTS says which kind ("DTS-HD MA"). */
export function trackCodec(s: Pick<StreamInfo, "codec" | "profile">): string {
  if (s.codec.toLowerCase() === "dts" && s.profile) return s.profile;
  return sourceCodecLabel(s.codec);
}

/** The line under a track's name: its codec first, then what else is worth knowing. */
export function trackDetails(s: StreamInfo): string {
  const parts: (string | null)[] = [trackCodec(s)];
  if (s.kind === "video") {
    parts.push(
      s.width && s.height ? `${s.width}×${s.height}` : null,
      s.frame_rate ? `${Math.round(s.frame_rate * 100) / 100} frames a second` : null,
      s.interlaced ? "interlaced" : null,
    );
  } else if (s.kind === "audio") {
    parts.push(channelsLabel(s.channels, s.channel_layout));
  }
  return parts.filter((p): p is string => Boolean(p) && p !== "—").join(" · ");
}

/** The picture in plain words: "4K · HDR10 · 1,000 nits peak". */
export function pictureText(file: Pick<MediaFile, "resolution" | "hdr">, video: StreamInfo | undefined): string {
  const hdr = video ? hdrSummary(video) : file.hdr ? hdrSummary({ hdr: file.hdr }) : null;
  return [file.resolution, hdr ?? (video || file.resolution ? "SDR" : null)].filter(Boolean).join(" · ") || "—";
}

function Tracks({ detail }: { detail: FileDetail }) {
  const probe = detail.file.probe;
  if (!probe) return null;
  const video = probe.streams.filter((s) => s.kind === "video" && !s.is_attached_pic);
  const audio = probe.streams.filter((s) => s.kind === "audio");
  const subs = probe.streams.filter((s) => s.kind === "subtitle");
  return (
    <ul className="divide-y divide-line rounded-lg border border-line">
      {video.map((s) => (
        <StreamRow key={s.index} icon={<Film aria-hidden />} title="Video" meta={trackDetails(s)} note={s.title} />
      ))}
      {audio.map((s) => (
        <StreamRow
          key={s.index}
          icon={<AudioLines aria-hidden />}
          title={trackName("audio", s.language)}
          meta={trackDetails(s)}
          note={s.title}
          tags={s.is_default ? <Badge>Default</Badge> : null}
        />
      ))}
      {subs.map((s) => (
        <StreamRow
          key={s.index}
          icon={<Captions aria-hidden />}
          title={trackName("subtitles", s.language)}
          meta={trackDetails(s)}
          note={s.title}
          tags={
            <>
              {s.is_forced ? <Badge>Forced</Badge> : null}
              {s.is_default ? <Badge>Default</Badge> : null}
            </>
          }
        />
      ))}
      {video.length + audio.length + subs.length === 0 ? (
        <li className="px-3.5 py-3 text-sm text-muted">No audio, video or subtitle tracks were found.</li>
      ) : null}
    </ul>
  );
}

/** Container, bitrate and every stream in ffprobe's own terms. */
function FileTechnical({ file }: { file: MediaFile }) {
  const probe = file.probe;
  const streams = probe?.streams ?? [];
  const attachments = streams.filter((s) => s.kind === "attachment" || s.is_attached_pic).length;
  const hdr = streams
    .filter((s) => s.kind === "video" && !s.is_attached_pic)
    .map((s) => hdrTechnical(s))
    .find(Boolean);
  return (
    <Disclosure className="mt-7">
      <dl className="-my-2 divide-y divide-line">
        <Detail label="Container" mono>
          {probe?.format_long_name ?? (file.container ? sourceCodecLabel(file.container) : "—")}
        </Detail>
        <Detail label="Bitrate" mono>
          {formatBitrate(file.bit_rate)}
        </Detail>
        {probe?.chapters ? <Detail label="Chapters">{probe.chapters}</Detail> : null}
        {attachments ? <Detail label="Attachments">{attachments}</Detail> : null}
        <Detail label="Modified">{formatDateTime(file.modified_at)}</Detail>
        {hdr ? (
          <Detail label="HDR metadata" mono>
            {hdr}
          </Detail>
        ) : null}
      </dl>
      {streams.length ? (
        <ul className="flex flex-col gap-1.5 font-mono text-xs leading-relaxed text-muted" aria-label="Streams">
          {streams.map((s) => (
            <li key={s.index} className="break-words">
              <span className="text-fg">
                #{s.index} {s.kind ?? "data"}
              </span>{" "}
              {s.kind === "video" ? videoMeta(s) : s.kind === "audio" ? audioMeta(s) : [s.codec, s.language].filter(Boolean).join(" · ")}
            </li>
          ))}
        </ul>
      ) : null}
    </Disclosure>
  );
}

function StatusExplanation({ file, jobs }: { file: MediaFile; jobs: Job[] }) {
  const live = useFileLive(file.id);
  const { library } = useLibrary(file.library_id);
  const latest = jobs[0];
  if (file.status === "failed") return <FailureCallout failure={file} title="The last attempt failed" />;
  if (skippedUnreadable(file, latest)) {
    return (
      <Callout tone="info" title="Skipped by you">
        <p>
          It can&apos;t be read (it looks damaged or isn&apos;t a video), so it&apos;s left alone. Replace it with a good
          copy and the new one is picked up automatically.
        </p>
      </Callout>
    );
  }
  if (file.status === "skipped") {
    // "Kept the original" when a new file was made and thrown away (the size
    // rule); everything else never needed work.
    const keptOriginal = latest?.state === "skipped" && latest.output_size !== null;
    const summary = skipSummary(file.skip_reason, keptOriginal, library?.profile.min_savings_pct);
    return (
      <Callout tone="info" title={summary.title}>
        <p>{summary.body}</p>
      </Callout>
    );
  }
  if (file.status === "processing") {
    return (
      <div className="rounded-lg border border-line p-4">
        <p className="text-sm font-medium text-fg">Converting now</p>
        {live ? (
          <>
            <Meter className="mt-2.5" value={live.overall} label="Conversion progress, whole file" live />
            <p className="mt-1.5 text-[0.8125rem] text-muted tabular">
              {Math.round(live.overall)}% · {JOB_STAGE_LABEL[live.stage]}
            </p>
          </>
        ) : (
          <p className="mt-1 text-[0.8125rem] text-muted">Progress appears here in a moment.</p>
        )}
      </div>
    );
  }
  if (file.status === "done") {
    const saved = doneSavings(file);
    // Jobs are newest first; the latest finished one says whether it was checked.
    const last = jobs.find((j) => j.state === "done");
    const verified = Boolean(last?.validation?.passed);
    return (
      <Callout tone="success" title={verified ? "Converted and verified" : "Converted"}>
        {saved
          ? saved.text
          : verified
            ? "The new file passed its checks before it was kept."
            : "Checks were off for this conversion."}
      </Callout>
    );
  }
  return null;
}

/** Whether the file sheet has anything to offer in its footer. */
function hasActions(file: MediaFile, profile: TranscodeProfile | undefined): boolean {
  switch (file.status) {
    case "processing":
      return false;
    case "done":
      return convertsAgain(file, profile);
    case "skipped":
      return skippedByUser(file.skip_reason) || skipFollowsSettings(file);
    default:
      return true;
  }
}

function FileActions({ file, jobs }: { file: MediaFile; jobs: Job[] }) {
  const { queue, skip } = useFileActions();
  const { library } = useLibrary(file.library_id);
  // Its latest conversion was "Convert anyway" and didn't finish: queue it that way again.
  const force = repeatsForce(jobs[0]);
  if (file.status === "done") {
    return (
      <ConvertAgainAction
        file={file}
        profile={library?.profile}
        onConfirm={() => queue.mutate({ file, force })}
        loading={queue.isPending}
      />
    );
  }
  if (file.status === "skipped" && !skippedByUser(file.skip_reason)) {
    // Queueing it normally would reach the same verdict; "Convert anyway"
    // sets the library's rules aside for this one file.
    return skipFollowsSettings(file) ? <ConvertAnywayButton file={file} /> : null;
  }
  const unreadable = file.status === "failed" && isUnreadable(file);
  if (unreadable || skippedUnreadable(file, jobs[0])) {
    // Converting a damaged original can't work; trying again reads it afresh (once it's replaced, say).
    return (
      <>
        <Button
          variant="secondary"
          size="sm"
          onClick={() => queue.mutate({ file, force })}
          loading={queue.isPending}
          className={unreadable ? "mr-auto" : undefined}
          needsServer
        >
          <RotateCcw aria-hidden />
          Try again
        </Button>
        {unreadable ? <SkipUnreadableButton file={file} /> : null}
      </>
    );
  }
  const canSkip = file.status === "pending" || file.status === "queued" || file.status === "failed";
  const canQueue = file.status !== "queued";
  return (
    <>
      {canSkip ? (
        <Button
          variant="quiet"
          size="sm"
          onClick={() => skip.mutate(file)}
          loading={skip.isPending}
          className="mr-auto"
          needsServer
        >
          <CircleMinus aria-hidden />
          Skip
        </Button>
      ) : null}
      {canQueue && file.status !== "failed" ? (
        <Button
          variant="secondary"
          size="sm"
          onClick={() => queue.mutate({ file, next: true, force })}
          disabled={queue.isPending}
          needsServer
        >
          <ArrowUpToLine aria-hidden />
          Convert next
        </Button>
      ) : null}
      {canQueue ? (
        <Button variant="primary" size="sm" onClick={() => queue.mutate({ file, force })} loading={queue.isPending} needsServer>
          {file.status === "failed" ? <RotateCcw aria-hidden /> : <Play aria-hidden />}
          {file.status === "failed" ? "Try again" : "Convert"}
        </Button>
      ) : null}
    </>
  );
}

/** The status badge in the sheet's header, with live whole-file progress. */
function SheetStatus({ file }: { file: MediaFile }) {
  const live = useFileLive(file.id);
  return (
    <FileStatusBadge
      status={file.status}
      error={file.error}
      problem={file.problem}
      progress={file.status === "processing" ? (live?.overall ?? null) : null}
    />
  );
}

/** Detail sheet for one file. */
export function FileSheet({ fileId, onClose }: { fileId: string | null; onClose: () => void }) {
  // Keep showing the last file while the sheet animates closed.
  const shownId = useRetained(fileId);
  const query = useFile(shownId, Boolean(fileId));
  const detail = query.data;
  const file = detail?.file;
  const { library } = useLibrary(file?.library_id);
  const settings = useSettings();
  const video = file?.probe?.streams.find((s) => s.kind === "video" && !s.is_attached_pic);
  return (
    <Sheet
      open={Boolean(fileId)}
      onOpenChange={(open) => !open && onClose()}
      title={file ? middleTruncate(file.file_name, 80) : "Loading…"}
      description={file ? <span className="font-mono text-xs break-all">{file.relative_path}</span> : undefined}
      footer={file && detail && hasActions(file, library?.profile) ? <FileActions file={file} jobs={detail.jobs} /> : undefined}
    >
      {detail && file ? (
        <>
          <div className="mb-5 flex flex-wrap items-center gap-2">
            <SheetStatus file={file} />
            <span className="text-[0.8125rem] text-muted">Updated {formatRelative(file.updated_at)}</span>
          </div>
          <StatusExplanation file={file} jobs={detail.jobs} />

          <SheetSection title="File">
            <dl className="divide-y divide-line">
              <Detail label="Size">
                {formatBytes(file.size_bytes)}
                {file.original_size_bytes !== null && file.status === "done" ? (
                  <span className="text-muted"> (was {formatBytes(file.original_size_bytes)})</span>
                ) : null}
              </Detail>
              {file.duration_secs ? <Detail label="Length">{formatClock(file.duration_secs)}</Detail> : null}
              {video || file.resolution ? <Detail label="Picture">{pictureText(file, video)}</Detail> : null}
              <Detail label="Location" mono>
                {file.path}
              </Detail>
            </dl>
          </SheetSection>

          {file.probe ? (
            <SheetSection title="Tracks">
              <Tracks detail={detail} />
            </SheetSection>
          ) : null}

          {detail.jobs.length ? (
            <SheetSection title="History">
              <ul className="divide-y divide-line rounded-lg border border-line">
                {detail.jobs.map((job) => {
                  // The server's figure for what was freed beats the sizes (0 for a hard-linked original).
                  const savings = savingsText(job.input_size, job.output_size, job.freed_bytes);
                  const renamed = newFileName(job, undefined, settings.data?.output_mode);
                  return (
                    <li key={job.id}>
                      <button
                        type="button"
                        onClick={() => openSheet({ file: null, job: job.id })}
                        className="flex w-full flex-wrap items-center gap-x-3 gap-y-1 px-3.5 py-3 text-left hover:bg-raised pointer-coarse:min-h-11"
                      >
                        <JobStateBadge job={job} standing={jobStanding(job, detail)} />
                        <span className="text-[0.8125rem] text-muted">
                          {formatRelative(job.finished_at ?? job.started_at ?? job.created_at)}
                        </span>
                        {savings && job.state === "done" ? (
                          <span className="ml-auto text-[0.8125rem] text-fg tabular">{savings.text}</span>
                        ) : job.state === "done" && noSpaceFreed(job) ? (
                          <span className="ml-auto text-[0.8125rem] text-muted">No space freed</span>
                        ) : null}
                        {renamed ? <WasName name={job.file_name} className="w-full" /> : null}
                      </button>
                    </li>
                  );
                })}
              </ul>
            </SheetSection>
          ) : null}

          <FileTechnical file={file} />
        </>
      ) : query.error ? (
        <Callout tone="danger" title="Couldn't load this file">
          It may have been removed from disk and from the library.
        </Callout>
      ) : (
        <div className="space-y-4">
          <Skeleton className="h-6 w-32" />
          <Skeleton className="h-40 w-full" />
          <Skeleton className="h-32 w-full" />
        </div>
      )}
    </Sheet>
  );
}
