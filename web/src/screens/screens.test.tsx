import { describe, expect, it } from "vitest";
import { videoCount } from "@/components/folder-picker";
import { limitText, queueSentence } from "@/components/queue-controls";
import type { Failures } from "@/lib/queries";
import type { HardwareInfo, Library, QueueState } from "@/lib/types";
import { overviewStatus, problemsFrom } from "./overview";
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
  total: { unreadable: 0, conversion: 0 },
  byLibrary: {},
  unreadable: [],
  retryIds: {},
};

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
      total: { unreadable: 4, conversion: 1 },
      byLibrary: { "lib-a": { unreadable: 4, conversion: 1 } },
    };
    const problems = problemsFrom([library()], failures, undefined);
    expect(problems.map((p) => [p.title, p.action.label])).toEqual([
      ["4 files can't be read", "Review"],
      ["1 file couldn't be converted", "Review"],
    ]);
    expect(problems[0].tone).toBe("warning");
    expect(problems[0].action.href).toBe("#/library/lib-a?status=failed");
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
});
