import { describe, expect, it } from "vitest";
import {
  convertAnywayDetails,
  countFailures,
  entryIsUnreadable,
  failedFilterLabel,
  failureGroup,
  failureNote,
  hdrSummary,
  hdrTechnical,
  isUnreadable,
  isUnreadableSource,
  jobStanding,
  keptAsConverted,
  newFileName,
  offersConvertAnyway,
  problemKind,
  reasonsFor,
  replaceLoss,
  retryableCount,
  setupFix,
  setupProblem,
  skipNote,
  skippedUnreadable,
  skipSummary,
  unreadableDetail,
} from "./outcomes";
import { serverLeftOutText, settlingText } from "./convertible";
import { PROBLEM_KINDS, type JobState, type MasteringDisplay, type ProblemKind } from "./types";

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
    const failed = (error: string, problem: ProblemKind) => ({ status: "failed" as const, error, problem });
    expect(
      countFailures([failed(TRUNCATED, "unreadable_source"), failed(FAKE, "unreadable_source"), failed(VISUAL, "verification")]),
    ).toEqual({
      unreadable: 2,
      conversion: 1,
      setup: 0,
      changed: 0,
    });
    expect(countFailures([failed(TRUNCATED, "unreadable_source")], 5)).toEqual({
      unreadable: 1,
      conversion: 4,
      setup: 0,
      changed: 0,
    });
  });
});

describe("problem codes", () => {
  const WORK = "Could not use the temp folder /temp: Permission denied (os error 13)";
  const failure = (problem: ProblemKind | null, error: string | null = "Something went wrong.") => ({
    problem,
    error,
  });

  it("trusts the server's code and never reads the sentence when there is one", () => {
    expect(problemKind(failure("disk_full", VISUAL))).toBe("disk_full");
    // A damaged-sounding sentence with a code that says otherwise.
    expect(failureGroup(failure("encoder", TRUNCATED))).toBe("conversion");
    expect(failureGroup(failure("unreadable_source", "Anything at all."))).toBe("unreadable");
    // A kind this UI doesn't know yet reads as "other": a failed conversion.
    expect(problemKind(failure("gremlins" as ProblemKind))).toBe("other");
  });

  it("never reads the sentence: an error without a code is \"other\"", () => {
    // The server sends a code with every error; none means one from before it had them.
    expect(problemKind(failure(null, TRUNCATED))).toBe("other");
    expect(problemKind(failure(null, WORK))).toBe("other");
    expect(problemKind(failure(null, null))).toBeNull();
    expect(failureGroup(failure(null, null))).toBe("conversion");
  });

  it("groups every kind", () => {
    const groups = Object.fromEntries(PROBLEM_KINDS.map((k) => [k, failureGroup(failure(k))]));
    expect(groups).toEqual({
      unreadable_source: "unreadable",
      work_folder: "setup",
      destination: "setup",
      disk_full: "setup",
      hardware_unavailable: "setup",
      encoder: "conversion",
      verification: "conversion",
      other: "conversion",
      source_changed: "changed",
    });
    expect(setupProblem(failure("work_folder"))).toBe("work_folder");
    expect(setupProblem(failure("encoder"))).toBeNull();
    expect(isUnreadable(failure("unreadable_source"))).toBe(true);
    expect(isUnreadable(failure("verification", TRUNCATED))).toBe(false);
  });

  it("gives each setup problem its fix and the setting where it's made", () => {
    expect(setupFix("work_folder").setting.path).toBe("/settings/output");
    // The work folder is at the bottom of Output: the link brings it into view.
    expect(setupFix("work_folder").setting.focus).toBe("temp_dir");
    expect(setupFix("disk_full").setting.focus).toBe("temp_dir");
    expect(setupFix("destination", "folder").setting.focus).toBe("output_folder");
    expect(setupFix("destination", "replace").setting.focus).toBeUndefined();
    expect(setupFix("disk_full").title).toBe("The disk is full");
    expect(setupFix("hardware_unavailable").setting).toEqual({ label: "Hardware settings", path: "/settings/hardware" });
    expect(setupFix("destination", "replace").fix).toMatch(/library folder.*read-write/);
    expect(setupFix("destination", "folder").fix).toMatch(/output folder/);
  });

  it("counts setup problems and changed files apart from failed conversions", () => {
    const f = (problem: ProblemKind) => ({ status: "failed" as const, error: "x", problem });
    expect(countFailures([f("work_folder"), f("disk_full"), f("source_changed"), f("encoder"), f("unreadable_source")])).toEqual(
      { unreadable: 1, conversion: 1, setup: 2, changed: 1 },
    );
    expect(retryableCount({ unreadable: 1, conversion: 1, setup: 2, changed: 1 })).toBe(4);
  });

  it("says each kind in a few words for list rows", () => {
    expect(failureNote(failure("unreadable_source"))).toBe("Looks damaged or isn't a video");
    expect(failureNote(failure("disk_full"))).toBe("The disk is full");
    // A moved or replaced file: the server's sentence says which.
    expect(failureNote(failure("source_changed"))).toBe("Something went wrong.");
    expect(failureNote(failure("source_changed", null))).toBe("Moved or changed during the conversion");
    expect(failureNote(failure("verification", VISUAL))).toBe(VISUAL);
    expect(failureNote(failure(null, null))).toBe("Failed");
  });
});

describe("keptAsConverted", () => {
  const job = (id: string, state: JobState, created_at: string) => ({ id, state, created_at });
  const done = job("d", "done", "2026-09-20T10:00:00Z");

  it("recognises a second conversion that left the converted file in place", () => {
    const again = job("a", "skipped", "2026-09-27T10:00:00Z");
    expect(keptAsConverted(again, { file: { status: "done" }, jobs: [again, done] })).toBe(true);
    const failed = job("f", "failed", "2026-09-27T10:00:00.5Z");
    expect(keptAsConverted(failed, { file: { status: "done" }, jobs: [failed, done] })).toBe(true);
    const stopped = job("s", "cancelled", "2026-09-27T10:00:00Z");
    expect(keptAsConverted(stopped, { file: { status: "done" }, jobs: [stopped, done] })).toBe(true);
  });

  it("reads the server's sentences once each, most common first", () => {
    const e = (error: string | null) => ({ error });
    expect(reasonsFor([e("A"), e("B"), e("A"), e(null), e("C"), e(" A ")], 2)).toEqual({
      reasons: [
        { text: "A", count: 3 },
        { text: "B", count: 1 },
      ],
      rest: 1,
    });
    expect(reasonsFor([e(null)])).toEqual({ reasons: [], rest: 0 });
  });

  it("leaves first attempts, finished conversions and unknown files alone", () => {
    // It failed first; a later conversion is what made the file converted.
    const first = job("f", "failed", "2026-09-10T10:00:00Z");
    expect(keptAsConverted(first, { file: { status: "done" }, jobs: [done, first] })).toBe(false);
    // The file isn't converted.
    const skipped = job("s", "skipped", "2026-09-27T10:00:00Z");
    expect(keptAsConverted(skipped, { file: { status: "skipped" }, jobs: [skipped] })).toBe(false);
    expect(keptAsConverted(done, { file: { status: "done" }, jobs: [done] })).toBe(false);
    expect(keptAsConverted(skipped, undefined)).toBe(false);
    // Older than the ten jobs listed: can't tell.
    const newer = Array.from({ length: 10 }, (_, i) => job(`n${i}`, "cancelled", `2026-09-28T0${i}:00:00Z`));
    expect(keptAsConverted(job("old", "skipped", "2026-09-01T00:00:00Z"), { file: { status: "done" }, jobs: newer })).toBe(
      false,
    );
  });
});

describe("jobStanding", () => {
  const job = (id: string, state: JobState, created_at: string) => ({ id, state, created_at });
  const done = job("d", "done", "2026-09-20T10:00:00Z");
  const failed = job("f", "failed", "2026-09-10T10:00:00Z");

  it("sees that a file has moved past an old failure, so it's never tried again unasked", () => {
    // Failed first, converted later: the failure is history.
    expect(jobStanding(failed, { file: { status: "done" }, jobs: [done, failed] })).toBe("converted");
    // Converted again later after a kept attempt: that attempt is history too.
    const again = job("a", "failed", "2026-09-25T10:00:00Z");
    const redone = job("r", "done", "2026-09-26T10:00:00Z");
    expect(jobStanding(again, { file: { status: "done" }, jobs: [redone, again, done] })).toBe("converted");
    expect(jobStanding(failed, { file: { status: "queued" }, jobs: [failed] })).toBe("queued");
    expect(jobStanding(failed, { file: { status: "processing" }, jobs: [failed] })).toBe("queued");
  });

  it("keeps a failure current while the file is still failed, and waits for the file", () => {
    expect(jobStanding(failed, { file: { status: "failed" }, jobs: [failed] })).toBe("current");
    expect(jobStanding(failed, undefined)).toBe("unknown");
    expect(jobStanding(done, undefined)).toBe("current");
    // A conversion the list no longer holds (history trimmed) came before a listed job.
    expect(jobStanding(failed, { file: { status: "done" }, jobs: [failed] })).toBe("kept");
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
  it("says how many files are still being copied", () => {
    expect(settlingText(3)).toBe("Waiting for 3 files to finish copying");
    expect(settlingText(1)).toBe("Waiting for 1 file to finish copying");
  });

  it("names what the server left out of a bulk convert", () => {
    expect(serverLeftOutText(2)).toBe("2 files were left out because this library's settings skip them.");
    expect(serverLeftOutText(1)).toBe("1 file was left out because this library's settings skip it.");
    expect(serverLeftOutText(0)).toBeNull();
    expect(serverLeftOutText(undefined)).toBeNull();
  });
});

describe("newFileName", () => {
  const job = { id: "j1", state: "done" as const, file_name: "Old Home Video.avi" };
  const file = (file_name: string, job_id: string | null = "j1") => ({ file_name, job_id });

  it("names the converted file when the conversion changed its extension", () => {
    expect(newFileName(job, file("Old Home Video.mkv"), "replace")).toBe("Old Home Video.mkv");
  });

  it("goes by the name the job recorded, without reading the file", () => {
    const recorded = { ...job, output_name: "Old Home Video.mkv" };
    expect(newFileName(recorded, undefined, "replace")).toBe("Old Home Video.mkv");
    // Even when the file has been renamed since (converted again), this job's result had that name.
    expect(newFileName(recorded, file("Old Home Video.mp4", "j2"), "replace")).toBe("Old Home Video.mkv");
    // Recorded as unchanged: the file is never asked, whatever its name says.
    expect(newFileName({ ...job, output_name: null }, file("Old Home Video.mkv"), "replace")).toBeNull();
    expect(newFileName({ ...job, output_name: job.file_name }, undefined, "replace")).toBeNull();
    // Saved to a separate folder: the library's file keeps its name.
    expect(newFileName(recorded, undefined, "folder")).toBeNull();
    expect(newFileName({ ...recorded, state: "failed" }, undefined, "replace")).toBeNull();
  });

  it("says nothing when the name is the same, the file moved on, or the result went elsewhere", () => {
    expect(newFileName(job, file("Old Home Video.avi"), "replace")).toBeNull();
    // A later conversion made the file what it is now.
    expect(newFileName(job, file("Old Home Video.mp4", "j2"), "replace")).toBeNull();
    // Saved to a separate folder: the original is still where it was.
    expect(newFileName(job, file("Old Home Video.mkv"), "folder")).toBeNull();
    expect(newFileName(job, undefined, "replace")).toBeNull();
    expect(newFileName({ ...job, state: "failed" }, file("Old Home Video.mkv"), "replace")).toBeNull();
    // Settings not loaded yet.
    expect(newFileName(job, file("Old Home Video.mkv"), undefined)).toBeNull();
  });
});

// The worker's sentence for a goal whose container can't hold some of a file's tracks (plan::replace_loss).
const LOSS =
  "MP4 can't hold this file's 2 picture-based subtitles and 1 subtitle font, so it was left unchanged. To convert it, choose an MKV goal or save converted files to a separate folder; Convert anyway converts it without them";
const LOSS_ONE =
  "MP4 can't hold this file's 1 attached file, so it was left unchanged. To convert it, choose an MKV goal or save converted files to a separate folder; Convert anyway converts it without them";
// The worker's sentence for a file with other hard links (SHARED_ORIGINAL).
const SHARED =
  "This file has another hard link (for example a torrent that is still seeding), so replacing it would use more space instead of saving it. It was left unchanged; Convert anyway converts it all the same";

describe("a skip for tracks the new container can't hold", () => {
  it("reads what would be lost, and how many things that is", () => {
    expect(replaceLoss(LOSS)).toEqual({
      container: "MP4",
      lost: "2 picture-based subtitles and 1 subtitle font",
      them: "them",
    });
    expect(replaceLoss(LOSS_ONE)).toEqual({ container: "MP4", lost: "1 attached file", them: "it" });
    expect(replaceLoss(LOSS.replace("MP4", "WebM"))?.container).toBe("WebM");
    expect(replaceLoss("Already HEVC")).toBeNull();
    expect(replaceLoss(SHARED)).toBeNull();
    expect(replaceLoss(null)).toBeNull();
  });

  it("is a skip the server says Convert anyway can set aside", () => {
    expect(offersConvertAnyway(LOSS)).toBe(true);
    expect(offersConvertAnyway(SHARED)).toBe(true);
    expect(offersConvertAnyway("Already HEVC")).toBe(false);
    expect(offersConvertAnyway("Dolby Vision profile 5 can't be converted without losing its colours — left unchanged")).toBe(
      false,
    );
    expect(offersConvertAnyway(null)).toBe(false);
  });

  it("tells the user plainly what Convert anyway leaves out, and that it's gone for good", () => {
    const details = convertAnywayDetails(LOSS);
    expect(details).toEqual([
      "MP4 can't hold this file's 2 picture-based subtitles and 1 subtitle font.",
      "Converting it anyway leaves them out of the new file. The original is replaced, so they are gone for good.",
      "To keep them, save converted files to a separate folder instead (Settings › Output).",
    ]);
    expect(convertAnywayDetails(LOSS_ONE)?.[1]).toBe(
      "Converting it anyway leaves it out of the new file. The original is replaced, so it is gone for good.",
    );
    // No sentence is cut off, and none shows the server's closing advice twice.
    expect(details?.join(" ")).not.toMatch(/Convert anyway converts it|was left unchanged/);
  });

  it("shows the server's own sentence for another skip it words itself, and nothing for the library's rules", () => {
    expect(convertAnywayDetails(SHARED)).toEqual([
      "This file has another hard link (for example a torrent that is still seeding), so replacing it would use more space instead of saving it.",
    ]);
    expect(convertAnywayDetails("Already HEVC")).toBeNull();
    expect(convertAnywayDetails("Only 4% smaller — kept the original")).toBeNull();
    expect(convertAnywayDetails(null)).toBeNull();
  });

  it("says it briefly in a list and fully in the file's sheet, in plain words", () => {
    expect(skipNote(LOSS)).toBe("MP4 can't hold its 2 picture-based subtitles and 1 subtitle font");
    expect(skipSummary(LOSS, false)).toEqual({
      title: "Left unchanged",
      body: "MP4 can't hold this file's 2 picture-based subtitles and 1 subtitle font, so it was left as it is. Replacing the original would lose them for good.",
    });
    expect(skipSummary(LOSS_ONE, false).body).toMatch(/would lose it for good\.$/);
  });
});

describe("entryIsUnreadable", () => {
  const damaged = "Failed Truncated.mkv: The original file appears damaged or incomplete (it stops after 0.1 s).";

  it("goes by the entry's code", () => {
    expect(entryIsUnreadable({ message: "Failed A.mkv: x", problem: "unreadable_source" })).toBe(true);
    expect(entryIsUnreadable({ message: "Failed A.mkv: x", problem: "verification" })).toBe(false);
    // A code without a damaged cause wins over a sentence that sounds like one.
    expect(entryIsUnreadable({ message: damaged, problem: "encoder" })).toBe(false);
    expect(entryIsUnreadable({ message: damaged, problem: null })).toBe(false);
  });

  it("reads the sentence only when the server sent no code at all", () => {
    expect(entryIsUnreadable({ message: damaged })).toBe(true);
    expect(entryIsUnreadable({ message: "Clip.mkv failed its visual check." })).toBe(false);
  });
});

describe("skippedUnreadable", () => {
  const skipped = { status: "skipped" as const, skip_reason: "Skipped by you", video_codec: "h264" };

  it("recognises a damaged original the user skipped", () => {
    // Never read as a video at all.
    expect(skippedUnreadable({ ...skipped, video_codec: null })).toBe(true);
    // Read, but its last conversion found it damaged.
    expect(skippedUnreadable(skipped, { state: "failed", error: TRUNCATED, problem: "unreadable_source" })).toBe(true);
  });

  it("leaves other skips alone", () => {
    expect(skippedUnreadable(skipped)).toBe(false);
    expect(skippedUnreadable(skipped, { state: "failed", error: VISUAL, problem: "verification" })).toBe(false);
    expect(skippedUnreadable({ ...skipped, skip_reason: "Already HEVC", video_codec: null })).toBe(false);
    expect(skippedUnreadable({ ...skipped, status: "failed", video_codec: null })).toBe(false);
  });
});

describe("failedFilterLabel", () => {
  it("names the failed filter after what it lists", () => {
    const counts = (unreadable: number, conversion: number, setup = 0, changed = 0) => ({
      unreadable,
      conversion,
      setup,
      changed,
    });
    expect(failedFilterLabel(counts(2, 0))).toBe("Can't be read");
    expect(failedFilterLabel(counts(0, 3))).toBe("Failed");
    expect(failedFilterLabel(counts(1, 1))).toBe("Needs review");
    // A setup problem or a changed file is no damaged original either.
    expect(failedFilterLabel(counts(2, 0, 1))).toBe("Needs review");
    expect(failedFilterLabel(counts(0, 0, 0, 2))).toBe("Failed");
    // Every one waits on a setup fix: named like their badges.
    expect(failedFilterLabel(counts(0, 0, 3))).toBe("Needs a fix");
    expect(failedFilterLabel(counts(0, 1, 3))).toBe("Failed");
    expect(failedFilterLabel(undefined)).toBe("Needs review");
  });
});
