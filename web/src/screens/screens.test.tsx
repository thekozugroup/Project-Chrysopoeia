import { afterEach, describe, expect, it, vi } from "vitest";
import { folderVideos, libraryConflict, videoCount } from "@/components/folder-picker";
import { limitText, queueSentence } from "@/components/queue-controls";
import { NO_FAILURES, type FailureCounts } from "@/lib/outcomes";
import { api } from "@/lib/api";
import { FAILED_READ_MAX, failuresFrom, fetchFailedFiles, type Failures } from "@/lib/queries";
import type { ActivityEntry, HardwareInfo, Job, Library, MediaFile, ProblemKind, QueueState } from "@/lib/types";
import { finishedPercent } from "@/components/library-bar";
import { queueSummary } from "@/components/shell";
import { overviewStatus, problemsFrom, savedFromText } from "./overview";
import { logLevel } from "./queue";
import { automaticJobs, newLibraryDefaultsText, splitPatterns } from "./settings";

const queue = (partial: Partial<QueueState> = {}): QueueState => ({
  paused: false,
  running: 0,
  queued: 0,
  max_jobs: 2,
  max_jobs_auto: true,
  max_jobs_source: "auto",
  waiting_for_schedule: false,
  ...partial,
});

const noFailures: Failures = {
  ready: true,
  complete: true,
  total: NO_FAILURES,
  byLibrary: {},
  unsorted: {},
  unreadable: [],
  setup: {},
  changed: [],
  failedIds: new Set(),
  retry: {},
};

const counts = (unreadable: number, conversion: number, setup = 0, changed = 0): FailureCounts => ({
  unreadable,
  conversion,
  setup,
  changed,
});

function library(partial: Partial<Library> = {}): Library {
  return {
    id: "lib-a",
    name: "Movies",
    path: "/media/movies",
    enabled: true,
    profile: {
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
    },
    stats: {
      settling: 0,
      file_count: 4,
      total_bytes: 1,
      pending: 0,
      queued: 0,
      processing: 0,
      done: 2,
      skipped: 0,
      failed: 2,
      saved_bytes: 1,
    },
    scanning: false,
    last_scan_at: null,
    path_error: null,
    created_at: "2026-09-28T05:10:00Z",
    ...partial,
  };
}

describe("overview status sentence", () => {
  it("gives one answer for each state", () => {
    expect(overviewStatus(queue({ running: 2, queued: 3 }), undefined, null)).toMatchObject({
      text: "Converting 2 files",
      detail: "3 waiting",
    });
    expect(overviewStatus(queue({ paused: true, queued: 5 }), undefined, null)).toMatchObject({
      text: "Paused",
      detail: "5 waiting",
      resume: true,
    });
    expect(overviewStatus(queue(), undefined, null).text).toBe("All caught up");
    // Failed files are said, and a setup problem is never "All caught up".
    expect(overviewStatus(queue(), undefined, null, { failed: 2, blocked: false })).toMatchObject({
      text: "All caught up",
      detail: "2 files to review",
    });
    expect(overviewStatus(queue(), undefined, null, { failed: 7, blocked: true })).toMatchObject({
      text: "Waiting for a fix",
    });
    // Something converting still says so first.
    expect(overviewStatus(queue({ running: 1 }), undefined, null, { failed: 7, blocked: true }).text).toBe(
      "Converting 1 file",
    );
    expect(overviewStatus(queue(), undefined, "Looking through Movies: 4 videos found so far").text).toMatch(
      /^Looking through Movies/,
    );
  });
});

describe("needs your attention", () => {
  it("is empty when nothing needs fixing", () => {
    expect(problemsFrom([library({ stats: { ...library().stats, failed: 0 } })], noFailures, undefined)).toEqual([]);
  });

  it("groups failures by cause, each with one action to the filtered list", () => {
    const failures: Failures = {
      ...noFailures,
      total: counts(4, 1),
      byLibrary: { "lib-a": counts(4, 1) },
    };
    const problems = problemsFrom([library()], failures, undefined);
    expect(problems.map((p) => [p.title, p.action?.label])).toEqual([
      ["4 files can't be read", "Review"],
      ["1 file couldn't be converted", "Review"],
    ]);
    expect(problems[0].tone).toBe("warning");
    expect(problems[0].action?.href).toBe("#/library/lib-a?status=failed");
  });

  it("gives each library its own row when a cause spans libraries, so Review shows every file it counted", () => {
    const failures: Failures = {
      ...noFailures,
      total: counts(3, 1),
      byLibrary: { "lib-a": counts(2, 1), "lib-b": counts(1, 0) },
    };
    const problems = problemsFrom([library(), library({ id: "lib-b", name: "Shows" })], failures, undefined);
    expect(problems.map((p) => [p.title, p.action?.href])).toEqual([
      ["Movies: 2 files can't be read", "#/library/lib-a?status=failed"],
      ["Shows: 1 file can't be read", "#/library/lib-b?status=failed"],
      // One library only: no name needed.
      ["1 file couldn't be converted", "#/library/lib-a?status=failed"],
    ]);
    expect(new Set(problems.map((p) => p.key)).size).toBe(problems.length);
  });

  it("says hardware needs a fix once, and folders that can't be read", () => {
    const hw = {
      hints: [
        { level: "warning", title: "NVIDIA GPU found, but it can't be used", detail: "", fix: null },
        { level: "info", title: "A tip", detail: "", fix: null },
      ],
    } as unknown as HardwareInfo;
    const problems = problemsFrom([library({ path_error: "The folder is missing." })], { ...noFailures, ready: false }, hw);
    expect(problems.map((p) => p.title)).toEqual(["Hardware setup needs a fix", "Movies: the folder can't be read"]);
  });

  const failedFile = (id: string, libraryId: string, problem: ProblemKind | null, error = "It went wrong."): MediaFile =>
    ({ id, library_id: libraryId, status: "failed", error, problem }) as MediaFile;

  it("gives each setup problem one row across libraries, with the server's words, its setting and Try again for all", () => {
    const libs = [library(), library({ id: "lib-b", name: "Shows" })];
    const files = [
      failedFile("w1", "lib-a", "work_folder"),
      failedFile("w2", "lib-b", "work_folder"),
      failedFile("d1", "lib-b", "disk_full"),
      failedFile("e1", "lib-a", "encoder"),
      failedFile("c1", "lib-a", "source_changed", "The file is no longer there. It may have been moved or deleted."),
    ];
    const problems = problemsFrom(libs, failuresFrom(libs, files), undefined, { output_mode: "replace" });
    expect(problems.map((p) => [p.title, p.action?.label ?? null, p.retry ?? null])).toEqual([
      [
        "The work folder can't be used",
        "Work folder settings",
        { requests: [{ action: "retry_failed", ids: ["w1", "w2"] }], count: 2 },
      ],
      ["The disk is full", "Work folder settings", { requests: [{ action: "retry_failed", ids: ["d1"] }], count: 1 }],
      ["1 file couldn't be converted", "Review", null],
      [
        "1 file changed or moved while it was being converted",
        null,
        { requests: [{ action: "retry_failed", ids: ["c1"] }], count: 1 },
      ],
    ]);
    // The server's sentence is the fix; the generic one isn't repeated beside it.
    expect(problems[0].detail).toBe("2 files couldn't be converted because of it.");
    expect(problems[0].reasons).toEqual(["It went wrong."]);
    // Straight to the work folder, at the bottom of Output.
    expect(problems[0].action?.href).toBe("#/settings/output?focus=temp_dir");
    expect(problems[3].reasons).toEqual(["The file is no longer there. It may have been moved or deleted."]);
    expect(problems[3].tone).toBe("info");
  });

  it("never explains a name clash as a permission problem", () => {
    const libs = [library()];
    const denied =
      "Chrysopoeia doesn't have permission to write in /media/locked, so the new file couldn't be put there and the original was kept.";
    const clash = 'A file named "dup.mkv" is already next to the original, so the new file can\'t take its name.';
    const files = [
      failedFile("p1", "lib-a", "destination", denied),
      failedFile("p2", "lib-a", "destination", denied),
      failedFile("n1", "lib-a", "destination", clash),
    ];
    const [row] = problemsFrom(libs, failuresFrom(libs, files), undefined, { output_mode: "replace" });
    expect(row.title).toBe("Finished files can't be saved");
    expect(row.detail).toBe("3 files couldn't be converted because of it.");
    expect(row.detail).not.toMatch(/read-write|PUID/);
    expect(row.reasons).toEqual([`2 files: ${denied}`, `1 file: ${clash}`]);
    // Without any sentence (an unusual server), the generic fix stands in.
    const [bare] = problemsFrom(libs, failuresFrom(libs, [failedFile("x", "lib-a", "destination", "")]), undefined, {
      output_mode: "replace",
    });
    expect(bare.reasons).toEqual([]);
    expect(bare.detail).toMatch(/^1 file couldn't be converted because of it\. Chrysopoeia can't write to the library folder/);
  });

  it("counts, groups and retries every failed file, not one page of them", () => {
    const libs = [library({ stats: { ...library().stats, failed: 6_200 } })];
    const read = Array.from({ length: FAILED_READ_MAX }, (_, i) => failedFile(`w${i}`, "lib-a", "work_folder"));
    const failures = failuresFrom(libs, read, 6_200);
    expect(failures.complete).toBe(false);
    expect(failures.unsorted["lib-a"]).toBe(1_200);
    // Past the files read, "Try again" selects by status: every failed file.
    expect(failures.retry["lib-a"]).toEqual({ request: { action: "retry_failed", library: "lib-a" }, count: 6_200 });
    const problems = problemsFrom(libs, failures, undefined);
    expect(problems.map((p) => [p.title, p.detail])).toEqual([
      ["The work folder can't be used", "At least 5,000 files couldn't be converted because of it."],
      // Not "couldn't be converted" for files whose cause isn't known.
      [
        "1,200 more files couldn't be converted",
        "There are too many failed files to sort them all here. The list shows why each one failed.",
      ],
    ]);
    expect(problems[0].retry).toEqual({ requests: [{ action: "retry_failed" }], count: 6_200 });
    // Everything read: by id, all of them.
    const all = failuresFrom([library({ stats: { ...library().stats, failed: 520 } })], read.slice(0, 520), 520);
    expect(all.complete).toBe(true);
    expect(problemsFrom(libs, all, undefined)[0].retry?.count).toBe(520);
  });

  const keptJob = (id: string, fileId: string, problem: ProblemKind, error = "It went wrong."): Job =>
    ({ id, file_id: fileId, library_id: "lib-a", state: "failed", error, problem }) as Job;

  it("shows a setup problem that stopped second conversions of converted files, and tries them again", () => {
    const libs = [library({ stats: { ...library().stats, failed: 0 } })];
    const failures = failuresFrom(libs, []);
    const kept = { work_folder: [keptJob("j1", "f1", "work_folder"), keptJob("j2", "f2", "work_folder")] };
    const [row] = problemsFrom(libs, failures, undefined, undefined, kept);
    expect(row.title).toBe("The work folder can't be used");
    expect(row.detail).toBe("2 converted files couldn't be converted again because of it. They're still as they were.");
    expect(row.retry).toEqual({ requests: [{ action: "queue", ids: ["f1", "f2"] }], count: 2 });
    // With failed files of the same cause: one row, one Try again for both.
    const withFailed = failuresFrom([library()], [failedFile("w1", "lib-a", "work_folder")]);
    const [both] = problemsFrom([library()], withFailed, undefined, undefined, kept);
    expect(both.detail).toBe(
      "1 file couldn't be converted because of it, and 2 converted files couldn't be converted again.",
    );
    expect(both.retry?.requests).toEqual([
      { action: "retry_failed", ids: ["w1"] },
      { action: "queue", ids: ["f1", "f2"] },
    ]);
    // A "Convert anyway" conversion is queued the same way again, not left out by the goal.
    const forced = { ...keptJob("j3", "f3", "disk_full"), force: true };
    const [full] = problemsFrom(libs, failures, undefined, undefined, { disk_full: [forced] });
    expect(full.retry).toEqual({ requests: [], forced: ["f3"], count: 1 });
  });

  it("puts files the chosen hardware couldn't convert with the hardware tips", () => {
    const libs = [library()];
    const files = [failedFile("h1", "lib-a", "hardware_unavailable"), failedFile("h2", "lib-a", "hardware_unavailable")];
    const failures = failuresFrom(libs, files);
    const alone = problemsFrom(libs, failures, undefined);
    expect(alone.map((p) => [p.title, p.action?.href, p.retry?.requests])).toEqual([
      ["The hardware you chose isn't working", "#/settings/hardware", [{ action: "retry_failed", ids: ["h1", "h2"] }]],
    ]);
    const hw = { hints: [{ level: "error", title: "No working NVIDIA encoder", detail: "", fix: null }] } as unknown as HardwareInfo;
    const merged = problemsFrom(libs, failures, hw);
    expect(merged).toHaveLength(1);
    expect(merged[0].detail).toBe("No working NVIDIA encoder. 2 files couldn't be converted because of it.");
    expect(merged[0].retry).toEqual({ requests: [{ action: "retry_failed", ids: ["h1", "h2"] }], count: 2 });
  });

  it("goes by the code, never the sentence: an error without one is a failed conversion", () => {
    const libs = [library()];
    const files = [
      failedFile("u1", "lib-a", null, "The original file appears damaged or incomplete (it stops after 0.1 s)."),
      failedFile("x1", "lib-a", null, "Could not use the temp folder /temp: Permission denied"),
    ];
    const failures = failuresFrom(libs, files);
    expect(failures.byLibrary["lib-a"]).toEqual(counts(0, 2));
    expect(failures.retry["lib-a"]).toEqual({ request: { action: "retry_failed", ids: ["u1", "x1"] }, count: 2 });
  });
});

describe("reading every failed file", () => {
  afterEach(() => vi.restoreAllMocks());

  const page = (offset: number, limit: number, total: number) => ({
    items: Array.from({ length: Math.max(0, Math.min(limit, total - offset)) }, (_, i) => ({
      id: `f${offset + i}`,
      library_id: "lib-a",
      status: "failed",
      error: "x",
      problem: "work_folder",
    })) as MediaFile[],
    total,
  });

  it("reads page after page until it has them all", async () => {
    const files = vi.spyOn(api, "files").mockImplementation(async (q) => page(q.offset ?? 0, q.limit ?? 100, 1_204));
    const result = await fetchFailedFiles();
    expect(files).toHaveBeenCalledTimes(3);
    expect(result.items).toHaveLength(1_204);
    expect(result.total).toBe(1_204);
  });

  it("stops at its limit and says how many there are", async () => {
    const files = vi.spyOn(api, "files").mockImplementation(async (q) => page(q.offset ?? 0, q.limit ?? 100, 12_000));
    const result = await fetchFailedFiles();
    expect(files).toHaveBeenCalledTimes(FAILED_READ_MAX / 500);
    expect(result.items).toHaveLength(FAILED_READ_MAX);
    expect(result.total).toBe(12_000);
  });
});

describe("files at once", () => {
  const recommended = { total: 1, reason: "1 at once: 4 CPU cores" };

  it("agrees with the queue, and names MAX_JOBS when the container set it", () => {
    expect(automaticJobs(queue({ max_jobs: 2, max_jobs_source: "env" }), recommended)).toMatchObject({
      count: 2,
      fromEnv: true,
    });
    expect(automaticJobs(queue({ max_jobs: 3, max_jobs_source: "auto" }), recommended)).toMatchObject({
      count: 3,
      fromEnv: false,
      description: recommended.reason,
    });
    // A saved number is in force: "Automatic" would mean the hardware's number.
    expect(automaticJobs(queue({ max_jobs: 6, max_jobs_auto: false, max_jobs_source: "settings" }), recommended).count).toBe(1);
    expect(limitText(queue({ max_jobs: 2, max_jobs_source: "env" }))).toBe("up to 2 at once (set by MAX_JOBS)");
  });

  it("only mentions the limit while files are converting", () => {
    expect(queueSentence(queue(), undefined)).toBe("Nothing to convert right now.");
    expect(queueSentence(queue({ running: 1 }), undefined)).toBe("Converting 1 file, up to 2 at once.");
  });
});

describe("ignore patterns", () => {
  it("keeps the built-in rules out of the text box", () => {
    expect(splitPatterns(["**/.*", "**/@eaDir/**", "**/Extras/**", "**/#recycle/**", "**/*.partial~"])).toEqual({
      builtIn: ["**/.*", "**/@eaDir/**", "**/#recycle/**", "**/*.partial~"],
      own: ["**/Extras/**"],
    });
  });
});

describe("folder picker counts", () => {
  it("counts videos, with a plus when the server stopped counting", () => {
    expect(videoCount(12)).toBe("12 videos");
    expect(videoCount(1)).toBe("1 video");
    expect(videoCount(1000, true)).toBe("1,000+ videos");
    expect(videoCount(0)).toBeNull();
    expect(videoCount(null)).toBeNull();
  });

  it("puts the chosen folder's own count on its button, including none", () => {
    expect(folderVideos(1204)).toBe("1,204 videos");
    expect(folderVideos(2000, true)).toBe("2,000+ videos");
    expect(folderVideos(0)).toBe("no videos");
  });
});

describe("finished share", () => {
  it("counts what the bar draws: originals that can't be read are set aside", () => {
    const stats = { ...library().stats, file_count: 5, done: 2, skipped: 1, failed: 2 };
    expect(finishedPercent(stats)).toBe(60);
    expect(finishedPercent(stats, 2)).toBe(100);
    expect(finishedPercent({ ...stats, file_count: 2, done: 0, skipped: 0 }, 2)).toBe(0);
  });
});

describe("log tone", () => {
  const entry = (
    level: "error" | "warning" | "info" | "success",
    message: string,
    problem?: ProblemKind | null,
  ): Pick<ActivityEntry, "level" | "message" | "problem"> => ({ level, message, ...(problem === undefined ? {} : { problem }) });

  it("shows a damaged original as a warning, like its badge, and leaves other failures red", () => {
    expect(logLevel(entry("error", "Failed Truncated.mkv: x", "unreadable_source"))).toBe("warning");
    expect(logLevel(entry("error", "Clip.mkv failed its visual check. The original was kept.", "verification"))).toBe("error");
    expect(logLevel(entry("error", "Failed Clip.mkv: x", null))).toBe("error");
    expect(logLevel(entry("info", "Scanned Movies: 3 files", null))).toBe("info");
    // A code on a quieter entry doesn't make it louder or quieter.
    expect(logLevel(entry("warning", "Left Clip.mkv unchanged", "unreadable_source"))).toBe("warning");
  });

  it("goes by the code, not by what the sentence sounds like", () => {
    const damaged = "Failed Clip.mkv: The original file appears damaged or incomplete (it stops after 0.1 s).";
    // Sounds like a damaged original, but the server says it was something else.
    expect(logLevel(entry("error", damaged, "encoder"))).toBe("error");
    expect(logLevel(entry("error", damaged, null))).toBe("error");
    // And a damaged original is amber however the server words it.
    expect(logLevel(entry("error", "Couldn't open Clip.mkv.", "unreadable_source"))).toBe("warning");
  });

  it("reads the sentence only for an entry from a server that sends no code", () => {
    expect(
      logLevel(entry("error", "Failed Truncated.mkv: The original file appears damaged or incomplete (it stops after 0.1 s).")),
    ).toBe("warning");
    expect(logLevel(entry("error", "Clip.mkv failed its visual check. The original was kept."))).toBe("error");
  });
});

describe("the frame's queue pill", () => {
  it("says a fix is needed, not \"Watching\", once a setup problem stops conversions", () => {
    expect(queueSummary(queue(), undefined, false, true)).toMatchObject({ text: "Needs a fix", tone: "blocked" });
    expect(queueSummary(queue(), undefined, false, false)).toMatchObject({ text: "Watching for new files", tone: "idle" });
    // Work in progress still comes first.
    expect(queueSummary(queue({ running: 1 }), undefined, false, true).text).toBe("Converting 1 file");
    expect(queueSummary(queue({ queued: 3 }), undefined, false, true).text).toBe("3 files waiting");
  });
});

describe("the space saved", () => {
  it("counts every converted file behind the total, naming the ones that grew", () => {
    expect(savedFromText({ files: 5, larger: 1 })).toBe("from 5 converted files (1 came out larger)");
    expect(savedFromText({ files: 1, larger: 0 })).toBe("from 1 converted file");
  });
});

describe("defaults for new libraries", () => {
  it("says what Add library really starts with", () => {
    expect(newLibraryDefaultsText(false, "balanced")).toMatch(
      /^Until you change these, Add library suggests the goal that suits this machine \(Balanced\)/,
    );
    expect(newLibraryDefaultsText(true, "balanced")).toMatch(/^New libraries start with these settings\./);
  });

  it("names the goal when the defaults were saved as one, and stops suggesting the hardware's", () => {
    // Saving Plays everywhere used to leave "Until you change these … (Balanced)" on screen.
    const text = newLibraryDefaultsText(true, "balanced", "compatible");
    expect(text).toMatch(/^New libraries start with Plays everywhere\./);
    expect(text).not.toMatch(/Until you change these|Balanced/);
    // Untouched defaults still say what Add library suggests, whatever goal they match.
    expect(newLibraryDefaultsText(false, "balanced", "save_space")).toMatch(/^Until you change these/);
  });
});

describe("picking a folder for a new library", () => {
  const libs = [
    { name: "Movies", path: "/media/Movies" },
    { name: "TV", path: "/media/TV/" },
  ];

  it("says why a folder can't be used before the goal step", () => {
    expect(libraryConflict("/media/Movies", libs)).toBe("This folder is already the library “Movies”.");
    expect(libraryConflict("/media/TV", libs)).toBe("This folder is already the library “TV”.");
    expect(libraryConflict("/media/Movies/Classics", libs)).toBe("This folder is inside “Movies”, which is already a library.");
    expect(libraryConflict("/media", libs)).toMatch(/^This folder contains the library “Movies”\./);
  });

  it("allows folders beside a library, even with a similar name", () => {
    expect(libraryConflict("/media/Movies 4K", libs)).toBeNull();
    expect(libraryConflict("/media/Music", libs)).toBeNull();
    expect(libraryConflict("/media/Movies", [])).toBeNull();
  });
});
