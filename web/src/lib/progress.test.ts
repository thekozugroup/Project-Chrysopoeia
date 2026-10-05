import { describe, expect, it } from "vitest";
import { clampOffset, framesElapsedText, knownOverall, overallProgress, readProgress } from "./progress";

describe("readProgress", () => {
  const transcoding = { stage: "transcoding" as const, progress: 0, frames: 366, elapsed_secs: 400 };

  it("has no share to show when the server says it isn't known, so never 0%", () => {
    expect(readProgress({ ...transcoding, progress_basis: "unknown" })).toEqual({
      kind: "unknown",
      frames: 366,
      elapsedSecs: 400,
    });
    expect(knownOverall({ ...transcoding, progress_basis: "unknown" })).toBeNull();
  });

  it("labels an estimate from frames as one", () => {
    const reading = readProgress({ ...transcoding, progress: 50, progress_basis: "frames" });
    expect(reading).toEqual({ kind: "estimated", overall: overallProgress("transcoding", 50), frames: 366 });
  });

  it("reads a measured share, a server that doesn't say, and other stages as they are", () => {
    expect(readProgress({ ...transcoding, progress: 50, progress_basis: "time" })).toEqual({
      kind: "measured",
      overall: overallProgress("transcoding", 50),
    });
    expect(readProgress({ stage: "transcoding", progress: 50 })).toEqual({
      kind: "measured",
      overall: overallProgress("transcoding", 50),
    });
    // Only converting has a basis; checking quality has its own share.
    expect(readProgress({ stage: "verifying", progress: 40, progress_basis: "unknown" }).kind).toBe("measured");
    expect(knownOverall({ stage: "verifying", progress: 0 })).toBe(85);
  });
});

describe("framesElapsedText", () => {
  it("says the frames and the time spent", () => {
    expect(framesElapsedText(1017, 400)).toBe("1,017 frames · 7 min elapsed");
    expect(framesElapsedText(1, 30)).toBe("1 frame · 30 s elapsed");
    expect(framesElapsedText(null, 30)).toBe("30 s elapsed");
    expect(framesElapsedText(12, null)).toBe("12 frames");
    expect(framesElapsedText(null, null)).toBe("Converting");
  });
});

describe("overallProgress", () => {
  it("never goes backwards across stages", () => {
    const samples: [Parameters<typeof overallProgress>[0], number][] = [
      ["preparing", 0],
      ["preparing", 100],
      ["transcoding", 0],
      ["transcoding", 50],
      ["transcoding", 100],
      ["verifying", 0],
      ["verifying", 60],
      ["verifying", 100],
      ["finalizing", 0],
      ["finalizing", 100],
    ];
    const values = samples.map(([stage, p]) => overallProgress(stage, p));
    for (let i = 1; i < values.length; i += 1) expect(values[i]).toBeGreaterThanOrEqual(values[i - 1]);
    expect(values[0]).toBe(0);
    expect(values[values.length - 1]).toBe(100);
  });

  it("weights converting most", () => {
    expect(overallProgress("transcoding", 50)).toBeCloseTo(43.5);
    expect(overallProgress("verifying", 0)).toBe(85);
  });

  it("clamps odd input", () => {
    expect(overallProgress("transcoding", 140)).toBe(85);
    expect(overallProgress("transcoding", -5)).toBe(2);
    expect(overallProgress("transcoding", Number.NaN)).toBe(2);
    expect(overallProgress("waiting", 50)).toBe(0);
  });
});

describe("clampOffset", () => {
  it("leaves valid offsets alone", () => {
    expect(clampOffset(0, 10, 50)).toBeNull();
    expect(clampOffset(50, 120, 50)).toBeNull();
  });

  it("moves past-the-end offsets to the last page", () => {
    expect(clampOffset(100, 9, 100)).toBe(0);
    expect(clampOffset(150, 120, 50)).toBe(100);
    expect(clampOffset(100, 100, 50)).toBe(50);
  });

  it("goes to the first page when the list is empty", () => {
    expect(clampOffset(50, 0, 50)).toBe(0);
  });
});
