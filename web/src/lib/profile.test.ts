import { describe, expect, it } from "vitest";
import {
  FACTORY_DEFAULT_GOAL,
  defaultsCustomized,
  defaultsPresetGoal,
  newLibraryStart,
  normalizeProfile,
  parseLanguages,
  parseQualityOverride,
  profileForGoal,
  profileForNewLibrary,
  sameProfile,
} from "./profile";
import type { Presets } from "./types";

describe("normalizeProfile", () => {
  it("keeps valid combinations as they are", () => {
    const { profile, notes } = normalizeProfile(undefined, profileForGoal("balanced"));
    expect(profile).toEqual(profileForGoal("balanced"));
    expect(notes).toEqual([]);
  });

  it("moves HEVC out of WebM into MKV and says so", () => {
    const { profile, notes } = normalizeProfile(undefined, {
      ...profileForGoal("save_space"),
      video_codec: "hevc",
      container: "webm",
    });
    expect(profile.container).toBe("mkv");
    expect(notes[0]).toMatch(/WebM can't hold HEVC/);
  });

  it("picks an audio codec the container can hold", () => {
    const { profile, notes } = normalizeProfile(undefined, {
      ...profileForGoal("compatible"),
      audio_codec: "flac",
    });
    expect(profile.container).toBe("mp4");
    expect(profile.audio_codec).toBe("aac");
    expect(notes).toHaveLength(1);
  });

  it("lowers a raw quality value the new codec's encoders would reject", () => {
    const { profile, notes } = normalizeProfile(undefined, {
      ...profileForGoal("balanced"),
      quality_override: 58,
    });
    expect(profile.quality_override).toBe(51);
    expect(notes[0]).toMatch(/up to 51/);
    expect(normalizeProfile(undefined, { ...profileForGoal("save_space"), quality_override: 58 }).profile.quality_override).toBe(58);
  });

  it("caps minimum savings at 90%", () => {
    expect(normalizeProfile(undefined, { ...profileForGoal("save_space"), min_savings_pct: 95 }).profile.min_savings_pct).toBe(90);
  });
});

describe("parseQualityOverride", () => {
  it("accepts empty (automatic) and in-range whole numbers", () => {
    expect(parseQualityOverride("", "hevc")).toEqual({ value: null, error: null });
    expect(parseQualityOverride(" 24 ", "hevc")).toEqual({ value: 24, error: null });
    expect(parseQualityOverride("63", "av1")).toEqual({ value: 63, error: null });
  });

  it("rejects values the encoder would refuse", () => {
    expect(parseQualityOverride("55", "hevc").error).toMatch(/0 to 51/);
    expect(parseQualityOverride("64", "av1").error).toMatch(/0 to 63/);
    expect(parseQualityOverride("-1", "h264").error).not.toBeNull();
    expect(parseQualityOverride("2.5", "h264").error).not.toBeNull();
    expect(parseQualityOverride("abc", "vp9").error).not.toBeNull();
  });
});

describe("parseLanguages", () => {
  it("splits, lowercases and de-duplicates codes", () => {
    expect(parseLanguages("eng, JPN;eng  fr")).toEqual({ codes: ["eng", "jpn", "fr"], error: null });
    expect(parseLanguages("   ")).toEqual({ codes: [], error: null });
  });

  it("explains which entries are not language codes", () => {
    const result = parseLanguages("english, xx1");
    expect(result.error).toMatch(/Not valid: english, xx1/);
  });
});

describe("profiles for new libraries", () => {
  const stock = profileForGoal("save_space");
  const custom = { ...profileForGoal("balanced"), max_height: 720, audio_languages: ["eng"], min_savings_pct: 20 };

  it("compares profiles regardless of key order", () => {
    const reordered = Object.fromEntries(Object.entries(stock).reverse()) as typeof stock;
    expect(sameProfile(stock, reordered)).toBe(true);
    expect(sameProfile(stock, { ...stock, quality: "high" })).toBe(false);
  });

  it("knows whether the defaults were changed from what a fresh install has", () => {
    // A fresh install's defaults are the Save space preset (core `TranscodeProfile::default`).
    expect(FACTORY_DEFAULT_GOAL).toBe("save_space");
    expect(defaultsCustomized(undefined, profileForGoal(FACTORY_DEFAULT_GOAL))).toBe(false);
    expect(defaultsCustomized(undefined, stock)).toBe(false);
    expect(defaultsCustomized(undefined, custom)).toBe(true);
    expect(defaultsCustomized(undefined, { ...stock, goal: "custom" })).toBe(true);
    expect(defaultsCustomized(undefined, { ...stock, quality: "high" })).toBe(true);
  });

  it("counts a goal-only change, even to another plain preset, as a change", () => {
    // Saving Plays everywhere (or Balanced, or Archive) as the default is a choice.
    for (const goal of ["balanced", "compatible", "archive"] as const) {
      expect(defaultsCustomized(undefined, profileForGoal(goal))).toBe(true);
    }
  });

  it("compares with the server's preset for the factory goal when it sent one", () => {
    const presets = {
      goals: [{ goal: "save_space", title: "Save space", summary: "", profile: { ...stock, quality: "small" } }],
    } as unknown as Presets;
    expect(defaultsCustomized(presets, { ...stock, quality: "small" })).toBe(false);
    expect(defaultsCustomized(presets, stock)).toBe(true);
  });

  it("finds the goal whose preset the defaults are exactly", () => {
    expect(defaultsPresetGoal(undefined, profileForGoal("compatible"))).toBe("compatible");
    expect(defaultsPresetGoal(undefined, stock)).toBe("save_space");
    expect(defaultsPresetGoal(undefined, custom)).toBeNull();
    expect(defaultsPresetGoal(undefined, { ...profileForGoal("compatible"), max_height: 1080 })).toBeNull();
    expect(defaultsPresetGoal(undefined, { ...stock, goal: "custom" })).toBeNull();
  });

  describe("how Add library starts", () => {
    it("keeps a fresh install's behaviour: the hardware suggests the goal, with no extra card", () => {
      expect(newLibraryStart(undefined, stock, "balanced")).toEqual({ choice: "balanced", defaultsCard: false });
      expect(newLibraryStart(undefined, stock, "save_space")).toEqual({ choice: "save_space", defaultsCard: false });
      // Settings not loaded yet.
      expect(newLibraryStart(undefined, undefined, "balanced")).toEqual({ choice: "balanced", defaultsCard: false });
    });

    it("follows defaults saved as another goal, whatever the hardware suggests", () => {
      expect(newLibraryStart(undefined, profileForGoal("compatible"), "balanced")).toEqual({
        choice: "compatible",
        defaultsCard: false,
      });
      expect(newLibraryStart(undefined, profileForGoal("compatible"), "save_space").choice).toBe("compatible");
    });

    it("gives defaults that are no goal's preset a card of their own, chosen from the start", () => {
      expect(newLibraryStart(undefined, custom, "balanced")).toEqual({ choice: "defaults", defaultsCard: true });
      expect(newLibraryStart(undefined, { ...stock, goal: "custom" }, "balanced")).toEqual({
        choice: "defaults",
        defaultsCard: true,
      });
    });
  });

  it("uses the defaults as they are when chosen", () => {
    expect(profileForNewLibrary(undefined, custom, "defaults")).toEqual(custom);
  });

  it("keeps the defaults' track and resolution choices with another goal", () => {
    const profile = profileForNewLibrary(undefined, custom, "compatible");
    expect(profile.goal).toBe("compatible");
    expect(profile.video_codec).toBe("h264");
    expect(profile.container).toBe("mp4");
    expect(profile.max_height).toBe(720);
    expect(profile.audio_languages).toEqual(["eng"]);
    // Thresholds belong to the goal's preset.
    expect(profile.min_savings_pct).toBeNull();
  });
});
