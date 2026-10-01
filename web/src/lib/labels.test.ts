import { describe, expect, it } from "vitest";
import { GOAL_SUMMARY, QUALITY_HELP, QUALITY_LABEL, RECOMMENDED_QUALITY, SPEED_LABEL } from "./labels";

describe("goal cards", () => {
  it("tells the truth about what Plays everywhere leaves unchanged", () => {
    const text = GOAL_SUMMARY.compatible;
    // Replacing originals as MP4 would lose these tracks, so such files are left unchanged.
    expect(text).toContain("When replacing originals");
    expect(text).toMatch(/picture or styled subtitles or attached fonts are left unchanged\.$/);
    // It no longer says styled subtitles are "left as they are" (they aren't kept, they're left alone only when replacing).
    expect(text).not.toMatch(/left as they are/);
  });

  it("keeps every card to a short, jargon-free paragraph", () => {
    for (const text of Object.values(GOAL_SUMMARY)) {
      expect(text.length).toBeLessThan(150);
      expect(text).not.toMatch(/\b(?:H\.?26[45]|HEVC|AV1|MP4|MKV|ASS|PGS)\b/);
    }
  });
});

describe("option names", () => {
  it("names the quality levels for what they are, and says which one is recommended", () => {
    expect(Object.values(QUALITY_LABEL)).toEqual(["Smallest", "Small", "Standard", "High", "Best quality"]);
    expect(RECOMMENDED_QUALITY).toBe("balanced");
    expect(QUALITY_HELP[RECOMMENDED_QUALITY]).toMatch(/^Recommended\./);
    // Only one level carries the recommendation.
    expect(Object.values(QUALITY_HELP).filter((text) => /recommended/i.test(text))).toHaveLength(1);
    expect(Object.values(QUALITY_LABEL)).not.toContain("Recommended");
  });

  it("never gives two options of one screen the same name", () => {
    // The quality scale ends in "Smaller files"; the speed option must not repeat it.
    expect(Object.values(SPEED_LABEL)).not.toContain("Smaller files");
    expect(new Set(Object.values(SPEED_LABEL)).size).toBe(Object.values(SPEED_LABEL).length);
  });
});
