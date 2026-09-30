import { describe, expect, it } from "vitest";
import {
  containerMatches,
  convertsAgain,
  formatNeedsWork,
  leftOutText,
  nothingToConvertText,
  planBulkConvert,
  repeatsForce,
} from "./convertible";
import type { MediaFile, TranscodeProfile } from "./types";

const balanced: TranscodeProfile = {
  goal: "balanced",
  video_codec: "hevc",
  audio_codec: "copy",
  container: "mkv",
  quality: "balanced",
  speed: "balanced",
  quality_override: null,
  max_height: null,
  subtitles: "keep",
  audio_languages: [],
  subtitle_languages: [],
  skip_efficient: true,
  min_savings_pct: 10,
};

const compatible: TranscodeProfile = {
  ...balanced,
  goal: "compatible",
  video_codec: "h264",
  audio_codec: "aac",
  container: "mp4",
  quality: "high",
  skip_efficient: false,
  min_savings_pct: null,
};

let next = 0;
function file(partial: Partial<MediaFile> = {}): MediaFile {
  next += 1;
  return {
    id: `file-${next}`,
    library_id: "lib",
    path: `/media/Clip ${next}.mkv`,
    relative_path: `Clip ${next}.mkv`,
    file_name: `Clip ${next}.mkv`,
    size_bytes: 1e8,
    modified_at: "2026-09-28T05:10:00Z",
    status: "pending",
    container: "matroska",
    video_codec: "h264",
    audio_codec: "aac",
    resolution: "1080p",
    hdr: null,
    duration_secs: 8,
    bit_rate: null,
    original_size_bytes: null,
    saved_bytes: null,
    skip_reason: null,
    error: null,
    problem: null,
    job_id: null,
    progress: null,
    scanned_at: "2026-09-28T05:10:00Z",
    updated_at: "2026-09-28T05:10:00Z",
    ...partial,
  };
}

describe("formatNeedsWork", () => {
  it("follows the efficiency rule when efficient files are skipped", () => {
    expect(formatNeedsWork(file({ video_codec: "h264" }), balanced)).toBe(true);
    expect(formatNeedsWork(file({ video_codec: "mpeg2video" }), balanced)).toBe(true);
    expect(formatNeedsWork(file({ video_codec: "hevc" }), balanced)).toBe(false);
    expect(formatNeedsWork(file({ video_codec: "av1" }), balanced)).toBe(false);
    expect(formatNeedsWork(file({ video_codec: "vp9" }), balanced)).toBe(false);
    expect(formatNeedsWork(file({ video_codec: "hevc" }), { ...balanced, video_codec: "av1" })).toBe(true);
  });

  it("otherwise converts anything that isn't the target codec in the target container", () => {
    expect(formatNeedsWork(file({ video_codec: "h264", container: "mov,mp4,m4a,3gp,3g2,mj2" }), compatible)).toBe(false);
    expect(formatNeedsWork(file({ video_codec: "h264", container: "matroska,webm" }), compatible)).toBe(true);
    expect(formatNeedsWork(file({ video_codec: "hevc", container: "mov" }), compatible)).toBe(true);
  });

  it("converts pictures larger than the library's size limit", () => {
    expect(formatNeedsWork(file({ video_codec: "hevc", resolution: "4K" }), { ...balanced, max_height: 1080 })).toBe(true);
    expect(formatNeedsWork(file({ video_codec: "hevc", resolution: "1080p" }), { ...balanced, max_height: 1080 })).toBe(false);
  });

  it("never converts a file without video", () => {
    expect(formatNeedsWork(file({ video_codec: null }), balanced)).toBe(false);
  });

  it("matches container families the way ffprobe names them", () => {
    expect(containerMatches("matroska,webm", "mkv")).toBe(true);
    expect(containerMatches("matroska,webm", "webm")).toBe(true);
    expect(containerMatches("mov,mp4,m4a,3gp,3g2,mj2", "mp4")).toBe(true);
    expect(containerMatches("mpegts", "mkv")).toBe(false);
    expect(containerMatches(null, "mp4")).toBe(false);
  });
});

describe("convertsAgain", () => {
  it("is false for a converted file already in the library's format (the worker would skip it)", () => {
    expect(convertsAgain(file({ status: "done", video_codec: "hevc" }), balanced)).toBe(false);
  });

  it("is true once the goal changed, or when the original is still the old format (output folder)", () => {
    expect(convertsAgain(file({ status: "done", video_codec: "hevc" }), { ...balanced, video_codec: "av1" })).toBe(true);
    expect(convertsAgain(file({ status: "done", video_codec: "h264" }), balanced)).toBe(true);
  });

  it("only applies to converted files, and doesn't hide the action without settings", () => {
    expect(convertsAgain(file({ status: "failed" }), balanced)).toBe(false);
    expect(convertsAgain(file({ status: "done", video_codec: "hevc" }), undefined)).toBe(true);
  });
});

describe("planBulkConvert", () => {
  it("sends only files that would convert and counts the rest by reason", () => {
    const pending = file();
    const failed = file({ status: "failed" });
    const userSkipped = file({ status: "skipped", skip_reason: "Skipped by you" });
    const settingsSkipped = file({ status: "skipped", video_codec: "hevc", skip_reason: "Already HEVC" });
    const audioOnly = file({ status: "skipped", video_codec: null, skip_reason: "Audio-only file — nothing to convert" });
    const doneSame = file({ status: "done", video_codec: "hevc" });
    const doneOld = file({ status: "done", video_codec: "h264" });
    const queued = file({ status: "queued" });
    const plan = planBulkConvert(
      [pending, failed, userSkipped, settingsSkipped, audioOnly, doneSame, doneOld, queued],
      balanced,
    );
    expect(plan.ids).toEqual([pending.id, failed.id, userSkipped.id, doneOld.id]);
    expect(plan).toMatchObject({ again: 1, settings: 1, converted: 1, unconvertible: 1, damaged: 0, busy: 1 });
  });

  it("leaves out failed files whose original can't be read (trying again can't help)", () => {
    const truncated = file({
      status: "failed",
      error: "The original file appears damaged or incomplete (it stops after 0.1 s). It was left unchanged.",
      problem: "unreadable_source",
    });
    const fake = file({
      status: "failed",
      video_codec: null,
      error: "This file can't be read as a video: it has no MP4 index.",
      problem: "unreadable_source",
    });
    const conversion = file({ status: "failed", error: "The new file didn't match the original.", problem: "verification" });
    const kept = file({ status: "skipped", video_codec: "hevc", skip_reason: "Already HEVC" });
    const plan = planBulkConvert([truncated, fake, conversion, kept], balanced);
    expect(plan.ids).toEqual([conversion.id]);
    expect(plan).toMatchObject({ damaged: 2, settings: 1 });
    // Only damaged files selected: nothing to convert, and it says why.
    const onlyDamaged = planBulkConvert([truncated], balanced);
    expect(onlyDamaged.ids).toEqual([]);
    expect(nothingToConvertText(onlyDamaged)).toBe("This file can't be read (it looks damaged or isn't a video).");
    expect(leftOutText(plan)).toMatch(/2 files can't be read \(they look damaged or aren't videos\)\./);
  });

  it("describes what was left out", () => {
    const none = { settings: 0, converted: 0, unconvertible: 0, busy: 0 };
    expect(leftOutText(none)).toBeNull();
    expect(leftOutText({ ...none, settings: 1 })).toMatch(/^1 file was left out because this library's settings skip it\./);
    expect(leftOutText({ ...none, settings: 2, unconvertible: 1 })).toMatch(/^2 files were left out .* 1 file can't be converted/);
    expect(leftOutText({ ...none, converted: 2 })).toBe("2 files are already converted to this library's format.");
  });

  it("explains a selection with nothing to convert", () => {
    const none = { settings: 0, converted: 0, unconvertible: 0, busy: 0 };
    expect(nothingToConvertText({ ...none, settings: 2 })).toBe(
      "These files are skipped by this library's settings. To convert one anyway, open it and choose Convert anyway.",
    );
    expect(nothingToConvertText({ ...none, converted: 1 })).toBe(
      "This file is already converted to this library's format. To convert it again, change the library's goal first.",
    );
    expect(nothingToConvertText({ ...none, busy: 3 })).toBe("These files are already in the queue.");
    expect(nothingToConvertText({ ...none, converted: 1, unconvertible: 1 })).toBe(
      "Nothing to convert: 1 file is already converted to this library's format, 1 file can't be converted. To convert them again, change the library's goal first.",
    );
  });
});

describe("repeatsForce", () => {
  it("repeats Convert anyway for a conversion that failed or was stopped", () => {
    expect(repeatsForce({ force: true, state: "failed" })).toBe(true);
    expect(repeatsForce({ force: true, state: "cancelled" })).toBe(true);
  });

  it("doesn't for plain conversions, finished ones or no conversion at all", () => {
    expect(repeatsForce({ force: false, state: "failed" })).toBe(false);
    // Converted: converting it again follows the library's (new) goal.
    expect(repeatsForce({ force: true, state: "done" })).toBe(false);
    // Kept the original anyway (a safety rule): nothing to repeat.
    expect(repeatsForce({ force: true, state: "skipped" })).toBe(false);
    expect(repeatsForce(undefined)).toBe(false);
  });
});
