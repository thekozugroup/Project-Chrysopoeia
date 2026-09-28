"use client";

/**
 * File detail sheet: status and reasons, actions, what the file contains
 * (from the probe), and its conversion history.
 */

import { ArrowUpToLine, AudioLines, CircleMinus, Film, Captions, Play, RotateCcw } from "lucide-react";
import type { ReactNode } from "react";
import { SheetSection, savingsText } from "@/components/jobs";
import { FileStatusBadge, JobStateBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Badge, Callout, Detail, Meter, Skeleton } from "@/components/ui/display";
import { Sheet } from "@/components/ui/overlays";
import { useFileActions } from "@/lib/actions";
import {
  formatBitrate,
  formatBytes,
  formatClock,
  formatDateTime,
  formatRelative,
  middleTruncate,
} from "@/lib/format";
import { channelsLabel, HDR_LABEL, languageLabel, sourceCodecLabel } from "@/lib/labels";
import { useFile } from "@/lib/queries";
import { updateParams } from "@/lib/router";
import { useFileProgress } from "@/lib/store";
import type { FileDetail, MediaFile, StreamInfo } from "@/lib/types";

function StreamRow({ icon, title, meta, tags }: { icon: ReactNode; title: ReactNode; meta: ReactNode; tags?: ReactNode }) {
  return (
    <li className="flex gap-3 px-3.5 py-3">
      <span className="mt-0.5 text-muted [&_svg]:size-4">{icon}</span>
      <div className="min-w-0 flex-1">
        <p className="text-sm font-medium text-fg">{title}</p>
        <p className="mt-0.5 font-mono text-xs leading-relaxed break-words text-muted">{meta}</p>
      </div>
      {tags ? <div className="flex shrink-0 flex-wrap items-start justify-end gap-1">{tags}</div> : null}
    </li>
  );
}

function videoMeta(s: StreamInfo): string {
  return [
    s.codec,
    s.profile,
    s.width && s.height ? `${s.width}×${s.height}` : null,
    s.bit_depth ? `${s.bit_depth}-bit` : null,
    s.frame_rate ? `${Number(s.frame_rate.toFixed(3))} fps` : null,
    s.pix_fmt,
    s.bit_rate ? formatBitrate(s.bit_rate) : null,
  ]
    .filter(Boolean)
    .join(" · ");
}

function audioMeta(s: StreamInfo): string {
  return [
    s.codec,
    channelsLabel(s.channels, s.channel_layout),
    s.sample_rate ? `${(s.sample_rate / 1000).toFixed(1)} kHz` : null,
    s.bit_rate ? formatBitrate(s.bit_rate) : null,
  ]
    .filter(Boolean)
    .join(" · ");
}

function Streams({ detail }: { detail: FileDetail }) {
  const probe = detail.file.probe;
  if (!probe) {
    return <p className="text-sm text-muted">This file hasn&apos;t been analysed yet.</p>;
  }
  const video = probe.streams.filter((s) => s.kind === "video" && !s.is_attached_pic);
  const audio = probe.streams.filter((s) => s.kind === "audio");
  const subs = probe.streams.filter((s) => s.kind === "subtitle");
  const attachments = probe.streams.filter((s) => s.kind === "attachment" || s.is_attached_pic).length;
  return (
    <>
      <ul className="divide-y divide-line rounded-lg border border-line">
        {video.map((s) => (
          <StreamRow
            key={s.index}
            icon={<Film aria-hidden />}
            title={`Video · ${sourceCodecLabel(s.codec)}`}
            meta={videoMeta(s)}
            tags={
              <>
                {s.hdr ? <Badge tone="accent">{HDR_LABEL[s.hdr]}</Badge> : null}
                {s.interlaced ? <Badge>Interlaced</Badge> : null}
              </>
            }
          />
        ))}
        {audio.map((s) => (
          <StreamRow
            key={s.index}
            icon={<AudioLines aria-hidden />}
            title={`${languageLabel(s.language)} audio${s.title ? ` · ${s.title}` : ""}`}
            meta={audioMeta(s)}
            tags={s.is_default ? <Badge>Default</Badge> : null}
          />
        ))}
        {subs.map((s) => (
          <StreamRow
            key={s.index}
            icon={<Captions aria-hidden />}
            title={`${languageLabel(s.language)} subtitles${s.title ? ` · ${s.title}` : ""}`}
            meta={`${s.codec} · ${sourceCodecLabel(s.codec)}`}
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
      <p className="mt-2 text-[0.8125rem] text-muted">
        {probe.format_long_name ?? sourceCodecLabel(probe.container)}
        {probe.chapters ? ` · ${probe.chapters} chapters` : ""}
        {attachments ? ` · ${attachments} attachments` : ""}
      </p>
    </>
  );
}

function StatusExplanation({ file }: { file: MediaFile }) {
  const live = useFileProgress(file.id);
  if (file.status === "failed") {
    return (
      <Callout tone="danger" title="The last attempt failed">
        <p>{file.error ?? "ffmpeg stopped with an error."}</p>
        <p className="mt-1.5">
          {/original/i.test(file.error ?? "") ? "" : "The original is untouched. "}You can try again, or skip the file.
        </p>
      </Callout>
    );
  }
  if (file.status === "skipped") {
    return (
      <Callout tone="info" title="Left as it is">
        {file.skip_reason ?? "No conversion needed."}
      </Callout>
    );
  }
  if (file.status === "processing") {
    const progress = live ?? file.progress ?? 0;
    return (
      <div className="rounded-lg border border-line p-4">
        <p className="text-sm font-medium text-fg">Converting now</p>
        <Meter className="mt-2.5" value={progress} label="Conversion progress" live />
        <p className="mt-1.5 text-[0.8125rem] text-muted tabular">{Math.round(progress)}%</p>
      </div>
    );
  }
  if (file.status === "done") {
    const saved = file.original_size_bytes !== null ? savingsText(file.original_size_bytes, file.size_bytes) : null;
    return (
      <Callout tone="success" title="Converted and verified">
        {saved ? saved.text : "The new file replaced the original after passing its checks."}
      </Callout>
    );
  }
  return null;
}

function FileActions({ file }: { file: MediaFile }) {
  const { queue, skip } = useFileActions();
  const canQueue = file.status !== "queued" && file.status !== "processing";
  const canSkip = file.status === "pending" || file.status === "queued" || file.status === "failed";
  const queueLabel =
    file.status === "failed"
      ? "Try again"
      : file.status === "done"
        ? "Convert again"
        : file.status === "skipped"
          ? "Convert anyway"
          : "Convert";
  return (
    <>
      {canSkip ? (
        <Button variant="quiet" size="sm" onClick={() => skip.mutate(file)} loading={skip.isPending} className="mr-auto">
          <CircleMinus aria-hidden />
          Skip
        </Button>
      ) : null}
      {canQueue ? (
        <>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => queue.mutate({ file, priority: 100 })}
            disabled={queue.isPending}
          >
            <ArrowUpToLine aria-hidden />
            Convert next
          </Button>
          <Button variant="primary" size="sm" onClick={() => queue.mutate({ file })} loading={queue.isPending}>
            {file.status === "failed" || file.status === "done" ? <RotateCcw aria-hidden /> : <Play aria-hidden />}
            {queueLabel}
          </Button>
        </>
      ) : null}
    </>
  );
}

/** Detail sheet for one file. */
export function FileSheet({ fileId, onClose }: { fileId: string | null; onClose: () => void }) {
  const query = useFile(fileId);
  const detail = query.data;
  const file = detail?.file;
  return (
    <Sheet
      open={Boolean(fileId)}
      onOpenChange={(open) => !open && onClose()}
      title={file ? middleTruncate(file.file_name, 80) : "Loading…"}
      description={file ? <span className="font-mono text-xs break-all">{file.relative_path}</span> : undefined}
      footer={file ? <FileActions file={file} /> : undefined}
    >
      {detail && file ? (
        <>
          <div className="mb-5 flex flex-wrap items-center gap-2">
            <FileStatusBadge status={file.status} progress={file.progress} />
            <span className="text-[0.8125rem] text-muted">Updated {formatRelative(file.updated_at)}</span>
          </div>
          <StatusExplanation file={file} />

          <SheetSection title="File">
            <dl className="divide-y divide-line">
              <Detail label="Size">{formatBytes(file.size_bytes)}</Detail>
              {file.original_size_bytes !== null ? (
                <Detail label="Original size">{formatBytes(file.original_size_bytes)}</Detail>
              ) : null}
              <Detail label="Length">{formatClock(file.duration_secs)}</Detail>
              <Detail label="Bitrate">{formatBitrate(file.bit_rate)}</Detail>
              <Detail label="Resolution">{file.resolution ?? "—"}</Detail>
              <Detail label="Modified">{formatDateTime(file.modified_at)}</Detail>
              <Detail label="Location" mono>
                {file.path}
              </Detail>
            </dl>
          </SheetSection>

          <SheetSection title="Tracks">
            <Streams detail={detail} />
          </SheetSection>

          {detail.jobs.length ? (
            <SheetSection title="History">
              <ul className="divide-y divide-line rounded-lg border border-line">
                {detail.jobs.map((job) => {
                  const savings = savingsText(job.input_size, job.output_size);
                  return (
                    <li key={job.id}>
                      <button
                        type="button"
                        onClick={() => updateParams({ file: null, job: job.id }, { replace: false })}
                        className="flex w-full flex-wrap items-center gap-x-3 gap-y-1 px-3.5 py-3 text-left hover:bg-raised"
                      >
                        <JobStateBadge job={job} />
                        <span className="text-[0.8125rem] text-muted">
                          {formatRelative(job.finished_at ?? job.started_at ?? job.created_at)}
                        </span>
                        {savings && job.state === "done" ? (
                          <span className="ml-auto text-[0.8125rem] text-fg tabular">{savings.text}</span>
                        ) : null}
                      </button>
                    </li>
                  );
                })}
              </ul>
            </SheetSection>
          ) : null}
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
