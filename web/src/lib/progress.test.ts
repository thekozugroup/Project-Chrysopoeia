import { describe, expect, it } from "vitest";
import { clampOffset, overallProgress } from "./progress";

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
