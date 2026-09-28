import { describe, expect, it } from "vitest";
import { folderVideos, videoCount } from "@/components/folder-picker";
import { limitText, queueSentence } from "@/components/queue-controls";
import { NO_FAILURES, type FailureCounts } from "@/lib/outcomes";
import { failuresFrom, type Failures } from "@/lib/queries";
import type { HardwareInfo, Library, MediaFile, ProblemKind, QueueState } from "@/lib/types";
import { finishedPercent } from "@/components/library-bar";
import { overviewStatus, problemsFrom } from "./overview";
import { logLevel } from "./queue";
import { automaticJobs, splitPatterns } from "./settings";

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
  total: NO_FAILURES,
  byLibrary: {},
  unreadable: [],
  setup: {},
  changed: [],
  retryIds: {},
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

  it("gives each setup problem one row across libraries, with its fix, its setting and Try again for all", () => {
    const libs = [library(), library({ id: "lib-b", name: "Shows" })];
    const files = [
      failedFile("w1", "lib-a", "work_folder"),
      failedFile("w2", "lib-b", "work_folder"),
      failedFile("d1", "lib-b", "disk_full"),
      failedFile("e1", "lib-a", "encoder"),
      failedFile("c1", "lib-a", "source_changed"),
    ];
    const problems = problemsFrom(libs, failuresFrom(libs, files, false), undefined, { output_mode: "replace" });
    expect(problems.map((p) => [p.title, p.action?.label ?? null, p.retry ?? null])).toEqual([
      ["The work folder can't be used", "Work folder settings", ["w1", "w2"]],
      ["The disk is full", "Work folder settings", ["d1"]],
      ["1 file couldn't be converted", "Review", null],
      ["1 file changed while it was being converted", null, ["c1"]],
    ]);
    expect(problems[0].detail).toMatch(/^2 files couldn't be converted because of it\. Make sure the work folder exists/);
    expect(problems[0].action?.href).toBe("#/settings/output");
    expect(problems[3].tone).toBe("info");
  });

  it("puts files the chosen hardware couldn't convert with the hardware tips", () => {
    const libs = [library()];
    const files = [failedFile("h1", "lib-a", "hardware_unavailable"), failedFile("h2", "lib-a", "hardware_unavailable")];
    const failures = failuresFrom(libs, files, false);
    const alone = problemsFrom(libs, failures, undefined);
    expect(alone.map((p) => [p.title, p.action?.href, p.retry])).toEqual([
      ["The hardware you chose isn't working", "#/settings/hardware", ["h1", "h2"]],
    ]);
    const hw = { hints: [{ level: "error", title: "No working NVIDIA encoder", detail: "", fix: null }] } as unknown as HardwareInfo;
    const merged = problemsFrom(libs, failures, hw);
    expect(merged).toHaveLength(1);
    expect(merged[0].detail).toBe("No working NVIDIA encoder. 2 files couldn't be converted because of it.");
    expect(merged[0].retry).toEqual(["h1", "h2"]);
  });

  it("reads damaged originals from their sentence when an older server sends no code", () => {
    const libs = [library()];
    const files = [
      failedFile("u1", "lib-a", null, "The original file appears damaged or incomplete (it stops after 0.1 s)."),
      failedFile("x1", "lib-a", null, "Could not use the temp folder /temp: Permission denied"),
    ];
    const failures = failuresFrom(libs, files, false);
    expect(failures.byLibrary["lib-a"]).toEqual(counts(1, 1));
    expect(failures.retryIds["lib-a"]).toEqual(["x1"]);
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
  const entry = (level: "error" | "warning" | "info" | "success", message: string) => ({ level, message });
  it("shows a damaged original as a warning, like its badge, and leaves other failures red", () => {
    expect(
      logLevel(entry("error", "Failed Truncated.mkv: The original file appears damaged or incomplete (it stops after 0.1 s).")),
    ).toBe("warning");
    expect(logLevel(entry("error", "Clip.mkv failed its visual check. The original was kept."))).toBe("error");
    expect(logLevel(entry("info", "Scanned Movies: 3 files"))).toBe("info");
  });
});
