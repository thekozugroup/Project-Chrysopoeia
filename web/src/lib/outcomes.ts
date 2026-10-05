/**
 * Plain readings of what happened to a file: why a failure happened (the
 * original can't be read, something in the setup needs fixing, the
 * conversion failed, or the file changed meanwhile), what a skip means, and
 * what a video's HDR data says.
 *
 * Failures are grouped by the server's `problem` code, which it sends with
 * every error (an error recorded before it had codes reads as `other`).
 */

import { formatCount, formatPercent, plural } from "./format";
import { HDR_LABEL, skippedByUser } from "./labels";
import {
  PROBLEM_KINDS,
  type ActivityEntry,
  type Job,
  type MediaFile,
  type OutputMode,
  type ProblemKind,
  type StreamInfo,
} from "./types";

/**
 * Sentences the server uses when the original itself can't be read:
 * `szalinski-scanner` probe errors ("This file can't be read as a video:
 * …", "The disk reported a read error…") and the worker's "The original
 * file appears damaged or incomplete (it stops after 0.1 s)." Only for an
 * activity entry that carries no `problem` code (one from a server older
 * than the field); every other reading goes by the code.
 */
const UNREADABLE = [
  /can't be read as a video/i,
  /appears damaged or incomplete/i,
  /reported a read error/i,
  /header is damaged/i,
  /not really a video/i,
];

/** Whether a sentence says the original is damaged or isn't a video (the reading for entries without a code). */
export function isUnreadableSource(error: string | null | undefined): boolean {
  const text = error?.trim();
  return Boolean(text && UNREADABLE.some((re) => re.test(text)));
}

/**
 * Whether an activity entry is about a damaged original or a file that isn't
 * a video: by its `problem` code when the server sent one (null means no
 * known cause), and by its sentence only when the field is missing, as
 * from a server older than it.
 */
export function entryIsUnreadable(entry: Pick<ActivityEntry, "message" | "problem">): boolean {
  if (entry.problem !== undefined) return entry.problem === "unreadable_source";
  return isUnreadableSource(entry.message);
}

/** A failed file or job: its sentence and its code. */
export interface Failure {
  error: string | null;
  problem: ProblemKind | null;
}

/**
 * The kind of a failure: the server's code (a kind this UI doesn't know
 * reads as `other`, as does an error without one). `null` when there's no
 * failure at all.
 */
export function problemKind(item: Failure): ProblemKind | null {
  const code = item.problem;
  if (typeof code === "string" && code) return PROBLEM_KINDS.includes(code) ? code : "other";
  return item.error?.trim() ? "other" : null;
}

/** Setup problems: fixed once in the setup, then every file tried again. */
export type SetupProblem = "work_folder" | "destination" | "disk_full" | "hardware_unavailable";

export const SETUP_PROBLEMS: readonly SetupProblem[] = ["hardware_unavailable", "work_folder", "disk_full", "destination"];

/**
 * How a failure reads:
 * - `unreadable`: the original is damaged or isn't a video ("Can't be
 *   read", amber); trying again can't help.
 * - `setup`: the work folder, the destination, free space or the chosen
 *   hardware; fixed once, then every file tried again.
 * - `conversion`: the encoder or the checks failed ("Couldn't be
 *   converted"), or a cause that isn't known.
 * - `changed`: the file was moved, deleted or replaced while it was being
 *   converted; nothing was replaced.
 */
export type FailureGroup = "unreadable" | "setup" | "conversion" | "changed";

export function failureGroup(item: Failure): FailureGroup {
  const kind = problemKind(item);
  if (kind === "unreadable_source") return "unreadable";
  if (kind === "source_changed") return "changed";
  if (kind && (SETUP_PROBLEMS as readonly string[]).includes(kind)) return "setup";
  return "conversion";
}

/** The setup problem behind a failure, or `null` when it's another kind. */
export function setupProblem(item: Failure): SetupProblem | null {
  const kind = problemKind(item);
  return kind && (SETUP_PROBLEMS as readonly string[]).includes(kind) ? (kind as SetupProblem) : null;
}

/** Whether a failure means the original can't be read (see `failureGroup`). */
export function isUnreadable(item: Failure): boolean {
  return failureGroup(item) === "unreadable";
}

/**
 * Whether a file the user skipped is one whose original can't be read
 * ("Skip this file" on a damaged original): it was never read as a video,
 * or its latest conversion found it damaged. Converting it can't work until
 * it's replaced, so only "Try again" is worth offering.
 */
export function skippedUnreadable(
  file: Pick<MediaFile, "status" | "skip_reason" | "video_codec">,
  latest?: (Pick<Job, "state"> & Failure) | null,
): boolean {
  if (file.status !== "skipped" || !skippedByUser(file.skip_reason)) return false;
  if (!file.video_codec) return true;
  return latest?.state === "failed" && isUnreadable(latest);
}

/**
 * The name a conversion gave the file, when it isn't the original's: the
 * goal's format has another extension ("Old Home Video.avi" became "Old
 * Home Video.mkv"). The job records it (`output_name`, which is `null`
 * while the name stayed the same); for a job from a server that doesn't,
 * it's read from the file instead, and only while this job's result is
 * what the file is now. `null` when the name didn't change, isn't known, or
 * the result went to a separate folder (the original is still where it was).
 */
export function newFileName(
  job: Pick<Job, "id" | "state" | "file_name"> & Partial<Pick<Job, "output_name">>,
  file: Pick<MediaFile, "job_id" | "file_name"> | null | undefined,
  outputMode: OutputMode | undefined,
): string | null {
  if (job.state !== "done" || outputMode !== "replace") return null;
  if (job.output_name !== undefined) {
    return job.output_name && job.output_name !== job.file_name ? job.output_name : null;
  }
  if (!file || file.job_id !== job.id) return null;
  return file.file_name !== job.file_name ? file.file_name : null;
}

/** What a setup problem is and exactly how to fix it. */
export interface SetupFix {
  /** The cause in a few words, as a title: "The work folder can't be used". */
  title: string;
  /** The exact fix, in a sentence or two. */
  fix: string;
  /**
   * Where the fix is made: the settings screen, and the setting on it to
   * bring into view (`?focus=`), when it isn't at the top.
   */
  setting: { label: string; path: string; focus?: SettingFocus };
}

/** Settings a problem's link can bring into view (see `SetupFix.setting`). */
export type SettingFocus = "temp_dir" | "output_folder";

/**
 * The cause and fix for a setup problem. `outputMode` picks the fix for a
 * destination that can't be written: the library folder when originals are
 * replaced, the output folder otherwise.
 */
export function setupFix(kind: SetupProblem, outputMode?: OutputMode): SetupFix {
  switch (kind) {
    case "work_folder":
      return {
        title: "The work folder can't be used",
        fix: "Make sure the work folder exists and Szalinski can write to it (in Docker, the PUID/PGID user needs write access), or choose another one.",
        setting: { label: "Work folder settings", path: "/settings/output", focus: "temp_dir" },
      };
    case "destination":
      return outputMode === "folder"
        ? {
            title: "Finished files can't be saved",
            fix: "Szalinski can't write to the output folder. Make sure the PUID/PGID user can write to it, or choose another one.",
            setting: { label: "Output settings", path: "/settings/output", focus: "output_folder" },
          }
        : {
            title: "Finished files can't be saved",
            fix: "Szalinski can't write to the library folder. Map it into the container read-write (not read-only) and make sure the PUID/PGID user can write to it.",
            setting: { label: "Output settings", path: "/settings/output" },
          };
    case "disk_full":
      return {
        title: "The disk is full",
        fix: "Free up space on the drive, or choose a work folder on a drive with more room.",
        setting: { label: "Work folder settings", path: "/settings/output", focus: "temp_dir" },
      };
    case "hardware_unavailable":
      return {
        title: "The hardware you chose isn't working",
        fix: "Fix the GPU setup, choose Automatic, or turn on “If the GPU fails, try the CPU”.",
        setting: { label: "Hardware settings", path: "/settings/hardware" },
      };
  }
}

/**
 * The title for a file that was moved, deleted or replaced while it was
 * being converted (`source_changed`). The server's own sentence says which.
 */
export const CHANGED_TITLE = "The file changed or moved while it was being converted";

/** What a `source_changed` failure means when the server gave no sentence. */
export const CHANGED_FALLBACK = "It was moved, deleted or replaced during the conversion, so nothing was replaced.";

/** One of the server's sentences for a group of failures, and how many share it. */
export interface Reason {
  text: string;
  count: number;
}

/**
 * The server's sentences for a group of failures, most common first, at
 * most `max` of them; `rest` counts the failures whose sentence isn't
 * shown. The server's sentence names the exact cause and fix ("A file named
 * \"dup.mkv\" is already next to the original…", "The work folder /temp
 * can't be created because…"), so it's shown instead of a generic fix,
 * and failures of one kind with different causes are never merged into one
 * explanation.
 */
export function reasonsFor(items: { error: string | null }[], max = 2): { reasons: Reason[]; rest: number } {
  const counts = new Map<string, number>();
  let without = 0;
  for (const item of items) {
    const text = item.error?.trim();
    if (text) counts.set(text, (counts.get(text) ?? 0) + 1);
    else without += 1;
  }
  const sorted = [...counts.entries()].map(([text, count]) => ({ text, count })).sort((a, b) => b.count - a.count);
  const reasons = sorted.slice(0, max);
  const shown = reasons.reduce((sum, r) => sum + r.count, 0);
  return { reasons, rest: items.length - shown - without };
}

/**
 * The specific part of a source problem, as its own sentence: "It stops
 * after 0.1 s." or "It has no MP4 index, so it's incomplete or not really a
 * video." `null` when the server gave no detail beyond the verdict.
 */
export function unreadableDetail(error: string | null | undefined): string | null {
  const text = error?.trim() ?? "";
  const stops = /\(it stops after ([^)]+)\)/i.exec(text);
  if (stops) return `It stops after ${stops[1]}.`;
  const colon = /can't be read as a video:\s*(.+)$/i.exec(text);
  if (colon) {
    const rest = colon[1].trim().replace(/\.?$/, ".");
    return rest.charAt(0).toUpperCase() + rest.slice(1);
  }
  const said = /can't be read as a video \((.+)\)\.?$/i.exec(text);
  if (said) return `${said[1].charAt(0).toUpperCase()}${said[1].slice(1).replace(/\.?$/, ".")}`;
  return null;
}

/** Failed files split by `failureGroup`. */
export interface FailureCounts {
  /** The original is damaged or isn't a video. */
  unreadable: number;
  /** The conversion or its checks failed (or the cause isn't known). */
  conversion: number;
  /** The setup needs a fix (work folder, destination, disk space, hardware). */
  setup: number;
  /** The file changed while it was being converted. */
  changed: number;
}

export const NO_FAILURES: FailureCounts = { unreadable: 0, conversion: 0, setup: 0, changed: 0 };

/**
 * Count failed files by cause. `total` is the number of failed files the
 * server reported; any not in `files` (a long list cut short) count as
 * conversion failures, the cautious reading.
 */
export function countFailures(
  files: (Pick<MediaFile, "status"> & Failure)[],
  total = files.length,
): FailureCounts {
  const counts = { ...NO_FAILURES };
  const failed = files.filter((f) => f.status === "failed");
  for (const file of failed) counts[failureGroup(file)] += 1;
  counts.conversion += Math.max(0, total - failed.length);
  return counts;
}

/** Failed files that aren't damaged originals: trying them again can help. */
export function retryableCount(counts: FailureCounts): number {
  return counts.conversion + counts.setup + counts.changed;
}

/**
 * The words for a library's "failed" filter, matching the rows it lists:
 * "Can't be read" when every failed file is a damaged original, "Needs a
 * fix" when every one waits on a setup fix (like their badges), "Failed"
 * when none is a damaged original, "Needs review" when it's a mix with
 * damaged originals (or the split isn't known yet).
 */
export function failedFilterLabel(
  counts: FailureCounts | undefined,
): "Can't be read" | "Needs a fix" | "Failed" | "Needs review" {
  if (!counts) return "Needs review";
  if (counts.unreadable > 0 && retryableCount(counts) === 0) return "Can't be read";
  if (counts.setup > 0 && counts.setup === counts.unreadable + retryableCount(counts)) return "Needs a fix";
  if (counts.unreadable === 0) return "Failed";
  return "Needs review";
}

/**
 * A failure in a few words for a list row, beside its badge: "Looks
 * damaged or isn't a video", "The disk is full", or the server's own
 * sentence for a failed conversion.
 */
export function failureNote(item: Failure): string {
  switch (failureGroup(item)) {
    case "unreadable":
      return "Looks damaged or isn't a video";
    case "changed":
      // The server's sentence says whether it was moved, deleted or replaced.
      return item.error?.trim() || "Moved or changed during the conversion";
    case "setup": {
      const kind = setupProblem(item);
      return kind ? setupFix(kind).title : (item.error ?? "Failed");
    }
    default:
      return item.error?.trim() || "Failed";
  }
}

/**
 * What a skip for a track the new container can't hold says (the worker's
 * `replace_loss`): "MP4 can't hold this file's 2 picture-based subtitles and
 * 1 subtitle font, so it was left unchanged. To convert it, …; Convert
 * anyway converts it without them". Replacing the original would lose
 * those tracks for good, so the file is left alone.
 */
export interface ReplaceLoss {
  /** The container that can't hold them, as the server names it ("MP4"). */
  container: string;
  /** What would be lost: "2 picture-based subtitles and 1 subtitle font". */
  lost: string;
  /** "it" for one thing, "them" for more. */
  them: "it" | "them";
}

const REPLACE_LOSS = /^(\S+) can't hold this file's (.+?), so it was left unchanged\b/i;

/** The loss behind a skip, or `null` when the reason is about something else. */
export function replaceLoss(reason: string | null | undefined): ReplaceLoss | null {
  const match = REPLACE_LOSS.exec(reason?.trim() ?? "");
  if (!match) return null;
  const lost = match[2].trim();
  // "2 picture-based subtitles and 1 subtitle font": count the things lost.
  const things = lost.split(/\s+and\s+|,\s*/).reduce((sum, part) => sum + (Number.parseInt(part, 10) || 1), 0);
  return { container: match[1], lost, them: things === 1 ? "it" : "them" };
}

/** Skip reasons that end "Convert anyway converts it …": the file is left alone until the user chooses otherwise. */
const OFFERS_CONVERT_ANYWAY = /\bConvert anyway converts it\b/i;

/** Whether the server's own skip reason offers "Convert anyway" (a lost track, a shared original). */
export function offersConvertAnyway(reason: string | null | undefined): boolean {
  return OFFERS_CONVERT_ANYWAY.test(reason?.trim() ?? "");
}

/**
 * What "Convert anyway" will do, one short paragraph at a time, for the
 * confirmation. A skip for a track that can't be kept says plainly what is
 * left out and that it can't be brought back; another skip the server
 * words itself shows its own sentence. `null` for the library's own rules
 * (already efficient, not smaller enough), which the confirmation words
 * itself.
 */
export function convertAnywayDetails(reason: string | null | undefined): string[] | null {
  const loss = replaceLoss(reason);
  if (loss) {
    const they = loss.them === "it" ? "it is" : "they are";
    return [
      `${loss.container} can't hold this file's ${loss.lost}.`,
      `Converting it anyway leaves ${loss.them} out of the new file. The original is replaced, so ${they} gone for good.`,
      `To keep ${loss.them}, save converted files to a separate folder instead (Settings › Output).`,
    ];
  }
  const text = reason?.trim() ?? "";
  if (!offersConvertAnyway(text)) return null;
  // The server's sentence without its closing "It was left unchanged; Convert anyway …".
  const said = text.replace(/\s*(?:So )?it was left unchanged[;.,]?\s*Convert anyway converts it.*$/i, "").trim();
  const cut = said || text.replace(/[;.]?\s*Convert anyway converts it.*$/i, "").trim();
  return cut ? [cut.replace(/[.,;]?$/, ".")] : null;
}

/** The one name for "a new file was made and thrown away" (the badge says "Kept original"). */
const KEPT_TITLE = "Kept the original";

/**
 * A skip in a few words for a list row, beside its badge: "6% smaller
 * (needs at least 10%)", "7% larger than the original", "Already HEVC",
 * "MP4 can't hold its 2 picture-based subtitles". `null` without a reason.
 */
export function skipNote(reason: string | null | undefined, minSavingsPct?: number | null): string | null {
  const text = reason?.trim() ?? "";
  if (!text) return null;
  const loss = replaceLoss(text);
  if (loss) return `${loss.container} can't hold its ${loss.lost}`;
  const verdict = sizeVerdict(text);
  if (verdict?.kind === "smaller") {
    // In the words of the library's own setting ("At least 10% smaller").
    return typeof minSavingsPct === "number" && verdict.pct < minSavingsPct
      ? `${formatPercent(verdict.pct)} smaller (needs at least ${formatPercent(minSavingsPct)})`
      : `${formatPercent(verdict.pct)} smaller, not enough for this library`;
  }
  if (verdict?.kind === "larger") return `${formatPercent(verdict.pct)} larger than the original`;
  if (verdict?.kind === "same") return "About the same size as the original";
  return text.replace(/\s*[—-]\s*(?:left unchanged|kept the original)\.?$/i, "");
}

/** Title and one sentence for a skipped file or job. */
export interface SkipSummary {
  title: string;
  body: string;
}

/** The size rule's verdict, read from the worker's reason ("Only 6% smaller — kept the original"). */
type SizeVerdict = { kind: "smaller"; pct: number } | { kind: "larger"; pct: number } | { kind: "same" };

function sizeVerdict(reason: string): SizeVerdict | null {
  const smaller = /only (\d+(?:\.\d+)?)\s*% smaller/i.exec(reason);
  if (smaller) return { kind: "smaller", pct: Number(smaller[1]) };
  const larger = /(\d+(?:\.\d+)?)\s*% larger/i.exec(reason);
  if (larger) return { kind: "larger", pct: Number(larger[1]) };
  if (/about the same size/i.test(reason)) return { kind: "same" };
  return null;
}

/**
 * "under this library's 90% minimum" when the library's current minimum
 * explains the skip, else "under this library's minimum" (the minimum may
 * have changed since, or isn't known here).
 */
function underMinimum(pct: number, minSavingsPct: number | null | undefined): string {
  return typeof minSavingsPct === "number" && pct < minSavingsPct
    ? `under this library's ${formatPercent(minSavingsPct)} minimum`
    : "under this library's minimum";
}

/**
 * What a skip means, said once. The server's reasons ("Only 4% smaller —
 * kept the original", "Already HEVC") are short verdicts; this turns them
 * into a title and a sentence that don't repeat each other. A size-rule
 * skip names the rule, since a file 74% smaller isn't "already efficient":
 * it fell short of the library's minimum saving.
 */
export function skipSummary(
  reason: string | null | undefined,
  keptOriginal: boolean,
  minSavingsPct?: number | null,
): SkipSummary {
  const text = reason?.trim() ?? "";
  if (skippedByUser(text)) {
    return { title: "Skipped by you", body: "It's left as it is. You can convert it whenever you like." };
  }
  const loss = replaceLoss(text);
  if (loss) {
    return {
      title: "Left unchanged",
      body: `${loss.container} can't hold this file's ${loss.lost}, so it was left as it is. Replacing the original would lose ${loss.them} for good.`,
    };
  }
  const verdict = sizeVerdict(text);
  if (verdict?.kind === "smaller") {
    return {
      title: KEPT_TITLE,
      body: `The new file was ${formatPercent(verdict.pct)} smaller, ${underMinimum(verdict.pct, minSavingsPct)}, so the original was kept.`,
    };
  }
  if (verdict?.kind === "larger") {
    return {
      title: KEPT_TITLE,
      body: `The converted file came out ${formatPercent(verdict.pct)} larger, so the original was kept.`,
    };
  }
  if (verdict?.kind === "same") {
    return { title: KEPT_TITLE, body: "The converted file came out about the same size, so the original was kept." };
  }
  if (keptOriginal) {
    return { title: KEPT_TITLE, body: text || "The new file wasn't worth keeping, so the original was kept." };
  }
  if (/^already\b/i.test(text)) {
    if (/\bin an? .+ file\b/i.test(text)) {
      return {
        title: "Already in this format",
        body: `${text.replace(/^already\s+/i, "It's already ").replace(/\.$/, "")}, the format this library converts to.`,
      };
    }
    return {
      title: "Already efficient",
      body: `${text.replace(/\.?$/, ".")} Converting it wouldn't make it meaningfully smaller.`,
    };
  }
  if (/audio-only|nothing to convert/i.test(text)) {
    return { title: "Nothing to convert", body: "It has no video, so there's nothing to convert." };
  }
  if (/left unchanged|left alone|left as it is/i.test(text)) {
    return { title: "Left unchanged", body: text.replace(/\s*[—-]\s*left unchanged\.?$/i, "").replace(/\.?$/, ".") };
  }
  return { title: "Skipped", body: text || "No conversion needed." };
}

/**
 * The HDR of a video stream in plain words: "HDR10 · 1,000 nits peak",
 * "Dolby Vision". `null` for SDR video.
 */
export function hdrSummary(stream: Pick<StreamInfo, "hdr" | "mastering_display" | "content_light" | "dolby_vision_without_base_layer">): string | null {
  if (!stream.hdr) return null;
  const parts: string[] = [HDR_LABEL[stream.hdr]];
  if (stream.dolby_vision_without_base_layer) parts.push("no standard HDR layer");
  const peak = stream.mastering_display?.max_luminance;
  if (peak && Number.isFinite(peak) && peak > 0) parts.push(`${formatCount(Math.round(peak))} nits peak`);
  return parts.join(" · ");
}

/** The raw HDR10 numbers, for the technical details. `null` when there are none. */
export function hdrTechnical(stream: Pick<StreamInfo, "mastering_display" | "content_light">): string | null {
  const parts: string[] = [];
  const md = stream.mastering_display;
  if (md) {
    const xy = (p: [number, number]) => `${p[0].toFixed(4)},${p[1].toFixed(4)}`;
    parts.push(
      `Mastering display R ${xy(md.red)} G ${xy(md.green)} B ${xy(md.blue)} WP ${xy(md.white_point)}, ${md.min_luminance}–${md.max_luminance} cd/m²`,
    );
  }
  const cl = stream.content_light;
  if (cl) parts.push(`MaxCLL ${cl.max_cll} · MaxFALL ${cl.max_fall}`);
  return parts.length ? parts.join("; ") : null;
}

/** "2 files can't be read" / "1 file can't be read". */
export function cantBeReadText(n: number): string {
  return `${plural(n, "file")} can't be read`;
}

/** The badge and title for a second conversion that left the converted file as it was. */
export const KEPT_CONVERTED = "Kept as converted";

/** How many jobs `GET /files/{id}` lists at most (newest first). */
const FILE_JOBS_LISTED = 10;

/**
 * What a finished job that didn't convert its file (skipped, failed or
 * stopped) means now, read from its file:
 * - `kept`: a second conversion that left an already converted file as it
 *   was (the server keeps a converted file converted when a new attempt
 *   doesn't replace it). It reads "Kept as converted", never "Skipped" or
 *   "Kept original": the file on disk is the converted one.
 * - `converted`: the file has been converted since (a later attempt
 *   worked). The old outcome is history: nothing to try again.
 * - `queued`: the file is queued or converting again right now.
 * - `current`: nothing has happened since; the job's own outcome stands.
 * - `unknown`: the file's details haven't loaded yet. Rows show no action
 *   until they have, so a converted file is never queued again unasked.
 */
export type JobStanding = "kept" | "converted" | "queued" | "current" | "unknown";

type StandingJob = Pick<Job, "id" | "state" | "created_at">;

/** A file with its latest jobs (`GET /files/{id}`), newest first. */
export interface FileJobs {
  file: Pick<MediaFile, "status">;
  jobs: StandingJob[];
}

/** Where `job` stands now (see `JobStanding`); `detail` is its file with its latest jobs. */
export function jobStanding(job: StandingJob, detail: FileJobs | undefined): JobStanding {
  if (job.state !== "skipped" && job.state !== "failed" && job.state !== "cancelled") return "current";
  if (!detail) return "unknown";
  const status = detail.file.status;
  if (status === "queued" || status === "processing") return "queued";
  if (status !== "done") return "current";
  const created = Date.parse(job.created_at);
  const conversions = detail.jobs.filter((j) => j.id !== job.id && j.state === "done");
  // A conversion queued after this job is what made the file converted.
  if (conversions.some((j) => Date.parse(j.created_at) > created)) return "converted";
  if (conversions.some((j) => Date.parse(j.created_at) < created)) return "kept";
  // The conversion isn't among the jobs listed. When this job is (or every
  // job is listed), anything after it would be too: the conversion came
  // first. Otherwise it can't be told; the file is converted now either way.
  const listed = detail.jobs.some((j) => j.id === job.id) || detail.jobs.length < FILE_JOBS_LISTED;
  return listed ? "kept" : "converted";
}

/** Whether a finished job left an already converted file as it was (see `jobStanding`). */
export function keptAsConverted(job: StandingJob, detail: FileJobs | undefined): boolean {
  return jobStanding(job, detail) === "kept";
}
