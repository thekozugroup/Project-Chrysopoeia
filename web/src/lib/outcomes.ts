/**
 * Plain readings of what happened to a file: whether a failure is the
 * original's fault (damaged, or not a video at all) rather than the
 * conversion's, what a skip means, and what a video's HDR data says.
 *
 * The server doesn't send a machine-readable reason for failures yet, so
 * the source problems are recognised from the sentences it writes for them
 * (the scanner's probe errors and the worker's damaged-source message).
 */

import { formatCount, formatPercent, plural } from "./format";
import { HDR_LABEL, skippedByUser } from "./labels";
import type { MediaFile, StreamInfo } from "./types";

/**
 * Sentences the server uses when the original itself can't be read:
 * `chrysopoeia-scanner` probe errors ("This file can't be read as a video:
 * …", "The disk reported a read error…") and the worker's "The original
 * file appears damaged or incomplete (it stops after 0.1 s)."
 */
const UNREADABLE = [
  /can't be read as a video/i,
  /appears damaged or incomplete/i,
  /reported a read error/i,
  /header is damaged/i,
  /not really a video/i,
];

/**
 * Whether a failure means the original is damaged or isn't a video. Trying
 * again can't help; the fix is replacing the file.
 */
export function isUnreadableSource(error: string | null | undefined): boolean {
  const text = error?.trim();
  return Boolean(text && UNREADABLE.some((re) => re.test(text)));
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

/** Failed files split by cause. */
export interface FailureCounts {
  /** The original is damaged or isn't a video. */
  unreadable: number;
  /** Everything else: the conversion or its checks failed. */
  conversion: number;
}

/**
 * Count failed files by cause. `total` is the number of failed files the
 * server reported; any not in `files` (a long list cut short) count as
 * conversion failures, the cautious reading.
 */
export function countFailures(files: Pick<MediaFile, "status" | "error">[], total = files.length): FailureCounts {
  const failed = files.filter((f) => f.status === "failed");
  const unreadable = failed.filter((f) => isUnreadableSource(f.error)).length;
  return { unreadable, conversion: Math.max(0, Math.max(total, failed.length) - unreadable) };
}

/**
 * The words for a library's "failed" filter, matching the rows it lists:
 * "Can't be read" when every failed file is a damaged original, "Failed"
 * when every one is a failed conversion, "Needs review" when it's both (or
 * the split isn't known yet).
 */
export function failedFilterLabel(counts: FailureCounts | undefined): "Can't be read" | "Failed" | "Needs review" {
  if (!counts) return "Needs review";
  if (counts.unreadable > 0 && counts.conversion === 0) return "Can't be read";
  if (counts.unreadable === 0) return "Failed";
  return "Needs review";
}

/** The one name for "a new file was made and thrown away" (the badge says "Kept original"). */
const KEPT_TITLE = "Kept the original";

/**
 * A skip in a few words for a list row, beside its badge: "6% smaller
 * (needs at least 10%)", "7% larger than the original", "Already HEVC".
 * `null` without a reason.
 */
export function skipNote(reason: string | null | undefined, minSavingsPct?: number | null): string | null {
  const text = reason?.trim() ?? "";
  if (!text) return null;
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
