import { describe, expect, it } from "vitest";
import {
  countFailures,
  failedFilterLabel,
  hdrSummary,
  hdrTechnical,
  isUnreadableSource,
  skipNote,
  skipSummary,
  unreadableDetail,
} from "./outcomes";
import { serverLeftOutText, settlingCount } from "./convertible";
import type { ActivityEntry, LibraryStats, MasteringDisplay } from "./types";

// Sentences the real server writes (chrysopoeia-scanner probe.rs, chrysopoeia-worker run.rs).
const TRUNCATED = "The original file appears damaged or incomplete (it stops after 0.1 s). It was left unchanged.";
const FAKE = "This file can't be read as a video: it has no MP4 index, so it's incomplete or not really a video.";
const PROBE_SAID = "This file can't be read as a video (ffprobe said: Weird failure).";
const VISUAL =
  "The new file didn't match the original: 1 of 4 checked moments looked different (similarity 0.61, 0.90 needed). The original was kept.";

describe("isUnreadableSource", () => {
  it("recognises the server's sentences for damaged or non-video originals", () => {
    expect(isUnreadableSource(TRUNCATED)).toBe(true);
    expect(isUnreadableSource(FAKE)).toBe(true);
    expect(isUnreadableSource(PROBE_SAID)).toBe(true);
    expect(isUnreadableSource("The disk reported a read error for this file, so it may be damaged.")).toBe(true);
  });

  it("leaves conversion failures, missing files and empty errors alone", () => {
    expect(isUnreadableSource(VISUAL)).toBe(false);
    expect(isUnreadableSource("Chrysopoeia doesn't have permission to read this file.")).toBe(false);
    expect(isUnreadableSource(null)).toBe(false);
    expect(isUnreadableSource("")).toBe(false);
  });

  it("pulls out the specific detail as its own sentence", () => {
    expect(unreadableDetail(TRUNCATED)).toBe("It stops after 0.1 s.");
    expect(unreadableDetail(FAKE)).toBe("It has no MP4 index, so it's incomplete or not really a video.");
    expect(unreadableDetail(PROBE_SAID)).toBe("Ffprobe said: Weird failure.");
    expect(unreadableDetail(VISUAL)).toBeNull();
  });

  it("counts failures by cause, treating any not listed as conversion failures", () => {
    const failed = (error: string) => ({ status: "failed" as const, error });
    expect(countFailures([failed(TRUNCATED), failed(FAKE), failed(VISUAL)])).toEqual({ unreadable: 2, conversion: 1 });
    expect(countFailures([failed(TRUNCATED)], 5)).toEqual({ unreadable: 1, conversion: 4 });
  });
});

describe("skipSummary", () => {
  it("names the size rule, once, with the library's minimum when it explains the skip", () => {
    expect(skipSummary("Only 6% smaller — kept the original", true, 10)).toEqual({
      title: "Kept the original",
      body: "The new file was 6% smaller, under this library's 10% minimum, so the original was kept.",
    });
    // A big saving under a high minimum is not "already efficient".
    expect(skipSummary("Only 74% smaller — kept the original", true, 90).body).toBe(
      "The new file was 74% smaller, under this library's 90% minimum, so the original was kept.",
    );
    // The minimum changed since, or isn't known: don't name a number that doesn't explain it.
    expect(skipSummary("Only 74% smaller — kept the original", true, 50).body).toBe(
      "The new file was 74% smaller, under this library's minimum, so the original was kept.",
    );
    expect(skipSummary("Only 6% smaller — kept the original", true).title).toBe("Kept the original");
    expect(skipSummary("The new file was 7% larger — kept the original", true)).toEqual({
      title: "Kept the original",
      body: "The converted file came out 7% larger, so the original was kept.",
    });
    expect(skipSummary("About the same size — kept the original", true).body).toBe(
      "The converted file came out about the same size, so the original was kept.",
    );
  });

  it("gives list rows a short reason beside their badge", () => {
    expect(skipNote("Only 6% smaller — kept the original", 10)).toBe("6% smaller (needs at least 10%)");
    expect(skipNote("Only 74% smaller — kept the original", 90)).toBe("74% smaller (needs at least 90%)");
    expect(skipNote("Only 74% smaller — kept the original", null)).toBe("74% smaller, not enough for this library");
    expect(skipNote("The new file was 7% larger — kept the original")).toBe("7% larger than the original");
    expect(skipNote("About the same size — kept the original")).toBe("About the same size as the original");
    expect(skipNote("Already HEVC")).toBe("Already HEVC");
    expect(skipNote("HDR video would lose its colours as H.264 — left unchanged")).toBe(
      "HDR video would lose its colours as H.264",
    );
    expect(skipNote(null)).toBeNull();
  });

  it("names the other kinds of skip plainly", () => {
    expect(skipSummary("Already HEVC", false).title).toBe("Already efficient");
    expect(skipSummary("Already H.264 in an MP4 or MOV file", false)).toEqual({
      title: "Already in this format",
      body: "It's already H.264 in an MP4 or MOV file, the format this library converts to.",
    });
    expect(skipSummary("Audio-only file — nothing to convert", false).title).toBe("Nothing to convert");
    expect(skipSummary("Skipped by you", false).title).toBe("Skipped by you");
    expect(skipSummary("HDR video would lose its colours as H.264 — left unchanged", false)).toEqual({
      title: "Left unchanged",
      body: "HDR video would lose its colours as H.264.",
    });
  });
});

describe("HDR in plain words", () => {
  it("says the format and the peak brightness", () => {
    const mastering_display: MasteringDisplay = {
      red: [0.708, 0.292],
      green: [0.17, 0.797],
      blue: [0.131, 0.046],
      white_point: [0.3127, 0.329],
      max_luminance: 1000,
      min_luminance: 0.005,
    };
    const stream = { hdr: "hdr10" as const, mastering_display, content_light: { max_cll: 1000, max_fall: 400 } };
    expect(hdrSummary(stream)).toBe("HDR10 · 1,000 nits peak");
    expect(hdrTechnical(stream)).toMatch(/0\.005–1000 cd\/m²; MaxCLL 1000 · MaxFALL 400$/);
    expect(hdrSummary({ hdr: "dolby_vision", dolby_vision_without_base_layer: true })).toBe(
      "Dolby Vision · no standard HDR layer",
    );
    expect(hdrSummary({ hdr: null })).toBeNull();
    expect(hdrTechnical({})).toBeNull();
  });
});

describe("round-3 counts", () => {
  const stats = (settling?: number): LibraryStats => ({
    file_count: 0,
    total_bytes: 0,
    pending: 0,
    queued: 0,
    processing: 0,
    done: 0,
    skipped: 0,
    failed: 0,
    saved_bytes: 0,
    settling,
  });
  const feed: ActivityEntry[] = [
    {
      id: 1,
      at: "2026-09-28T05:10:00Z",
      level: "info",
      message: "Scanned Movies: 0 files, 3 still being copied (checked again when they're finished)",
      file_id: null,
      job_id: null,
      library_id: "lib",
    },
  ];

  it("uses the server's settling count, and reads the scan summary only without it", () => {
    expect(settlingCount({ id: "lib", stats: stats(5) }, feed)).toBe(5);
    // 0 is the server's answer (the files settled), not "unknown".
    expect(settlingCount({ id: "lib", stats: stats(0) }, feed)).toBe(0);
    expect(settlingCount({ id: "lib", stats: stats(undefined) }, feed)).toBe(3);
    expect(settlingCount({ id: "lib", stats: stats(0) }, [])).toBe(0);
  });

  it("names what the server left out of a bulk convert", () => {
    expect(serverLeftOutText(2)).toBe("2 files were left out because this library's settings skip them.");
    expect(serverLeftOutText(1)).toBe("1 file was left out because this library's settings skip it.");
    expect(serverLeftOutText(0)).toBeNull();
    expect(serverLeftOutText(undefined)).toBeNull();
  });
});

describe("failedFilterLabel", () => {
  it("names the failed filter after what it lists", () => {
    expect(failedFilterLabel({ unreadable: 2, conversion: 0 })).toBe("Can't be read");
    expect(failedFilterLabel({ unreadable: 0, conversion: 3 })).toBe("Failed");
    expect(failedFilterLabel({ unreadable: 1, conversion: 1 })).toBe("Needs review");
    expect(failedFilterLabel(undefined)).toBe("Needs review");
  });
});
