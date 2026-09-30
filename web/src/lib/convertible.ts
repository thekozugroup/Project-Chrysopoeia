/**
 * Whether queueing a file would actually convert it. The worker decides
 * again when a job starts (`decide` in crates/chrysopoeia-worker/src/plan.rs)
 * and skips files the library's settings leave alone, so the UI must not
 * offer "Convert" for them: the file would be skipped at once and the user
 * told it worked. These helpers mirror the worker's rules as far as a file's
 * summary allows.
 */

import { plural } from "./format";
import { skippedByUser } from "./labels";
import { isUnreadable } from "./outcomes";
import type { Container, Job, MediaFile, TranscodeProfile, VideoCodec } from "./types";

/**
 * Skip reasons that come from the library's settings, which "Convert anyway"
 * can set aside: already efficient ("Already HEVC"), already in the target
 * format, or not enough smaller ("Only 4% smaller — kept the original",
 * "The new file was 7% larger — kept the original"). Safety skips (Dolby
 * Vision without a standard layer, HDR to H.264, unreadable picture size or
 * audio) always apply, so they don't match.
 */
const SETTINGS_SKIP = /^already\b|\d+(?:\.\d+)?\s*% (?:smaller|larger)|kept the original/i;

/**
 * Whether the library's settings decided to skip this file and could decide
 * otherwise: it has real video, the skip wasn't the user's own, and the
 * reason is one of the settings' rules rather than a safety one.
 */
export function skipFollowsSettings(file: MediaFile): boolean {
  if (file.status !== "skipped" || skippedByUser(file.skip_reason)) return false;
  const hasVideo = file.probe
    ? file.probe.streams.some((s) => s.kind === "video" && !s.is_attached_pic)
    : Boolean(file.video_codec);
  return hasVideo && (file.duration_secs ?? 0) >= 1 && SETTINGS_SKIP.test(file.skip_reason?.trim() ?? "");
}

/**
 * Whether trying a file again should repeat "Convert anyway": its latest
 * conversion was queued that way and ended without a result (it failed, for
 * instance on a full disk, or was stopped). Queued plainly, the library's
 * rules would apply this time and could keep the original, undoing the
 * user's choice. The server doesn't carry the choice over by itself.
 */
export function repeatsForce(latest: Pick<Job, "force" | "state"> | null | undefined): boolean {
  return Boolean(latest?.force) && (latest?.state === "failed" || latest?.state === "cancelled");
}

/** Efficiency rank of an ffprobe codec name (core `source_efficiency_rank`). */
export function sourceEfficiencyRank(codec: string): number {
  switch (codec.toLowerCase()) {
    case "av1":
      return 4;
    case "hevc":
    case "h265":
    case "vp9":
    case "vvc":
    case "h266":
      return 3;
    case "h264":
    case "vp8":
      return 2;
    default:
      return 1;
  }
}

/** Efficiency rank of a target codec (core `VideoCodec::efficiency_rank`). */
export function targetEfficiencyRank(codec: VideoCodec): number {
  return codec === "av1" ? 4 : codec === "h264" ? 2 : 3;
}

/** The target codec an ffprobe codec name is (core `VideoCodec::from_probe_name`). */
export function videoCodecFromProbe(name: string): VideoCodec | null {
  switch (name.toLowerCase()) {
    case "av1":
    case "libdav1d":
    case "libaom-av1":
      return "av1";
    case "hevc":
    case "h265":
      return "hevc";
    case "h264":
    case "avc":
    case "avc1":
      return "h264";
    case "vp9":
      return "vp9";
    default:
      return null;
  }
}

/**
 * Whether ffprobe's container name belongs to the target's family. ffprobe
 * can't tell MKV from WebM or MP4 from MOV, so the worker matches families.
 */
export function containerMatches(probeContainer: string | null, target: Container): boolean {
  const name = (probeContainer ?? "").split(",")[0].trim().toLowerCase();
  return target === "mp4" ? name === "mov" || name === "mp4" : name === "matroska" || name === "webm";
}

/** Short side of each resolution class the server names files with. */
const RESOLUTION_LINES: Record<string, number> = {
  "8K": 4320,
  "4K": 2160,
  "1440p": 1440,
  "1080p": 1080,
  "720p": 720,
  "576p": 576,
  "480p": 480,
  SD: 360,
};

function largerThanLimit(resolution: string | null, maxHeight: number | null): boolean {
  if (!maxHeight || !resolution) return false;
  const lines = RESOLUTION_LINES[resolution];
  return lines !== undefined && lines > maxHeight;
}

/**
 * Whether the library's settings would convert a file in this format: its
 * codec is less efficient than the target (or, when efficient files aren't
 * skipped, it isn't already the target codec in the target container), or
 * its picture is larger than the library's size limit.
 */
export function formatNeedsWork(
  file: Pick<MediaFile, "video_codec" | "container" | "resolution">,
  profile: TranscodeProfile,
): boolean {
  const codec = file.video_codec;
  if (!codec) return false;
  if (largerThanLimit(file.resolution, profile.max_height)) return true;
  if (profile.skip_efficient) return sourceEfficiencyRank(codec) < targetEfficiencyRank(profile.video_codec);
  return !(videoCodecFromProbe(codec) === profile.video_codec && containerMatches(file.container, profile.container));
}

/**
 * Whether "Convert again" on a converted file would do anything. The file's
 * format is what was written last time, so this is only true after the
 * library's goal changed (or, when finished files go to a separate folder,
 * because the original is converted again).
 */
export function convertsAgain(file: MediaFile, profile: TranscodeProfile | undefined): boolean {
  if (file.status !== "done") return false;
  // Without the library's settings, don't hide the action.
  if (!profile) return true;
  return formatNeedsWork(file, profile);
}

/** Which selected files a bulk "Convert" sends, and which it leaves out and why. */
export interface BulkConvertPlan {
  /** Files that will be queued. */
  ids: string[];
  /** Of those, files that were already converted (a second pass). */
  again: number;
  /** Left out: the library's settings skip them. */
  settings: number;
  /** Left out: already converted to the library's current format. */
  converted: number;
  /** Left out: audio-only, too short or unreadable; never converted. */
  unconvertible: number;
  /**
   * Left out: failed because the original is damaged or isn't a video.
   * Converting it again can't succeed; the fix is replacing the file.
   */
  damaged: number;
  /** Left out: already queued or converting. */
  busy: number;
}

export function planBulkConvert(files: MediaFile[], profile: TranscodeProfile | undefined): BulkConvertPlan {
  const plan: BulkConvertPlan = { ids: [], again: 0, settings: 0, converted: 0, unconvertible: 0, damaged: 0, busy: 0 };
  for (const file of files) {
    switch (file.status) {
      case "queued":
      case "processing":
        plan.busy += 1;
        break;
      case "skipped":
        if (skippedByUser(file.skip_reason)) plan.ids.push(file.id);
        else if (skipFollowsSettings(file)) plan.settings += 1;
        else plan.unconvertible += 1;
        break;
      case "done":
        if (convertsAgain(file, profile)) {
          plan.ids.push(file.id);
          plan.again += 1;
        } else plan.converted += 1;
        break;
      case "failed":
        if (isUnreadable(file)) plan.damaged += 1;
        else plan.ids.push(file.id);
        break;
      default:
        plan.ids.push(file.id);
    }
  }
  return plan;
}

type LeftOut = Pick<BulkConvertPlan, "settings" | "converted" | "unconvertible" | "busy"> &
  Partial<Pick<BulkConvertPlan, "damaged">>;

/** What a bulk "Convert" left out, in a sentence or two, or `null` when nothing was. */
export function leftOutText(plan: LeftOut): string | null {
  const parts: string[] = [];
  if (plan.settings) {
    const it = plan.settings === 1 ? "it" : "them";
    parts.push(
      `${plural(plan.settings, "file was", "files were")} left out because this library's settings skip ${it}. To convert one anyway, open it and choose Convert anyway.`,
    );
  }
  if (plan.converted) {
    parts.push(`${plural(plan.converted, "file is", "files are")} already converted to this library's format.`);
  }
  if (plan.unconvertible) {
    parts.push(`${plural(plan.unconvertible, "file", "files")} can't be converted (each file says why).`);
  }
  if (plan.damaged) {
    parts.push(
      `${plural(plan.damaged, "file", "files")} can't be read (${plan.damaged === 1 ? "it looks damaged or isn't a video" : "they look damaged or aren't videos"}).`,
    );
  }
  if (plan.busy) {
    parts.push(`${plural(plan.busy, "file is", "files are")} already in the queue.`);
  }
  return parts.length ? parts.join(" ") : null;
}

/**
 * What the server's own count of left-out files means (bulk "queue" with
 * ids answers `left_out`): files its settings skip that the selection
 * didn't know about yet.
 */
export function serverLeftOutText(leftOut: number | undefined): string | null {
  if (!leftOut) return null;
  return `${plural(leftOut, "file was", "files were")} left out because this library's settings skip ${leftOut === 1 ? "it" : "them"}.`;
}

/** Why none of the selected files can be converted, when that's the case. */
export function nothingToConvertText(plan: LeftOut): string {
  const groups = [
    plan.settings ? { n: plan.settings, one: "is skipped by this library's settings", many: "are skipped by this library's settings" } : null,
    plan.converted
      ? { n: plan.converted, one: "is already converted to this library's format", many: "are already converted to this library's format" }
      : null,
    plan.unconvertible ? { n: plan.unconvertible, one: "can't be converted", many: "can't be converted" } : null,
    plan.damaged
      ? {
          n: plan.damaged,
          one: "can't be read (it looks damaged or isn't a video)",
          many: "can't be read (they look damaged or aren't videos)",
        }
      : null,
    plan.busy ? { n: plan.busy, one: "is already in the queue", many: "are already in the queue" } : null,
  ].filter((g): g is { n: number; one: string; many: string } => g !== null);
  if (groups.length === 0) return "Nothing selected can be converted.";
  const total = groups.reduce((sum, g) => sum + g.n, 0);
  const hint = plan.converted
    ? ` To convert ${total === 1 ? "it" : "them"} again, change the library's goal first.`
    : plan.settings
      ? ` To convert ${total === 1 ? "it" : "one"} anyway, open it and choose Convert anyway.`
      : "";
  if (groups.length === 1) {
    const [g] = groups;
    return `${g.n === 1 ? "This file" : "These files"} ${g.n === 1 ? g.one : g.many}.${hint}`;
  }
  return `Nothing to convert: ${groups.map((g) => `${plural(g.n, "file")} ${g.n === 1 ? g.one : g.many}`).join(", ")}.${hint}`;
}

/** "Waiting for 3 files to finish copying" (see `settlingCount`). */
export function settlingText(copying: number): string {
  return `Waiting for ${plural(copying, "file")} to finish copying`;
}
