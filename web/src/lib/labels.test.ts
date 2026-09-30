import { describe, expect, it } from "vitest";
import { QUALITY_HELP, QUALITY_LABEL, RECOMMENDED_QUALITY, SPEED_LABEL } from "./labels";

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
