import { describe, expect, it } from "vitest";
import { ApiError } from "./api";
import { profileForGoal } from "./profile";
import {
  changedKeys,
  errorsFrom,
  profileFieldOf,
  saveBarMessage,
  sectionFor,
  settingForField,
} from "./settings-form";
import type { Settings } from "./types";

const base: Settings = {
  auto_queue: true,
  watch_folders: true,
  rescan_interval_hours: 24,
  max_jobs: null,
  hardware: "auto",
  cpu_fallback: true,
  validation: "standard",
  output_mode: "replace",
  output_folder: null,
  temp_dir: null,
  keep_file_dates: true,
  low_priority: true,
  active_hours: null,
  ignore_patterns: [],
  min_file_size_mb: 50,
  default_profile: profileForGoal("save_space"),
  onboarded: true,
};

const invalid = (message: string) => new ApiError(400, "invalid_settings", message);

describe("changedKeys", () => {
  it("lists only what changed", () => {
    expect(changedKeys(base, { ...base })).toEqual([]);
    expect(changedKeys({ ...base, max_jobs: 3, active_hours: { start: 1, end: 7 } }, base).sort()).toEqual([
      "active_hours",
      "max_jobs",
    ]);
  });

  it("compares the default profile field by field", () => {
    const reordered = Object.fromEntries(Object.entries(base.default_profile).reverse()) as Settings["default_profile"];
    expect(changedKeys({ ...base, default_profile: reordered }, base)).toEqual([]);
    expect(changedKeys({ ...base, default_profile: { ...reordered, quality: "best" } }, base)).toEqual([
      "default_profile",
    ]);
  });
});

describe("a folder picked again as it was", () => {
  const saved: Settings = { ...base, output_mode: "folder", output_folder: "/out", temp_dir: "/work" };

  it("is no change: saving it again wouldn't take the drive there now (Settings offers that on its own)", () => {
    expect(changedKeys({ ...saved, output_folder: "/out" }, saved)).toEqual([]);
    expect(changedKeys({ ...saved, temp_dir: "/work" }, saved)).toEqual([]);
    expect(changedKeys({ ...saved, output_folder: "/elsewhere" }, saved)).toEqual(["output_folder"]);
  });
});

describe("errorsFrom", () => {
  it("maps the server's invalid_settings messages to their field", () => {
    expect(errorsFrom(invalid("Choose an output folder, or switch back to replacing the originals."))).toHaveProperty(
      "output_folder",
    );
    expect(
      errorsFrom(invalid("The output folder can't be inside the library Movies, or Chrysopoeia would convert its own results.")),
    ).toHaveProperty("output_folder");
    expect(
      errorsFrom(invalid("Chrysopoeia can't write to the temporary folder /temp. Check its permissions.")),
    ).toHaveProperty("temp_dir");
    expect(errorsFrom(invalid("Jobs at once must be between 1 and 32."))).toHaveProperty("max_jobs");
    expect(errorsFrom(invalid("Active hours must be whole hours from 0 to 23."))).toHaveProperty("active_hours");
    expect(errorsFrom(invalid('"**/[x" isn\'t a valid ignore pattern: unclosed class'))).toHaveProperty(
      "ignore_patterns",
    );
  });

  it("uses the setting named in a type error", () => {
    expect(errorsFrom(invalid('The value for "rescan_interval_hours" isn\'t valid: expected u32'))).toEqual({
      rescan_interval_hours: 'The value for "rescan_interval_hours" isn\'t valid: expected u32',
    });
  });

  it("falls back to the only key sent, then to a general message", () => {
    expect(errorsFrom(invalid("Something odd."), ["validation"])).toEqual({ validation: "Something odd." });
    expect(errorsFrom(invalid("Something odd."), ["validation", "hardware"])).toEqual({ general: "Something odd." });
    expect(errorsFrom(new ApiError(0, "network_error", "Can't reach the Chrysopoeia server."), ["max_jobs"])).toEqual({
      general: "Can't reach the Chrysopoeia server.",
    });
    expect(errorsFrom(new Error("boom"))).toEqual({ general: "boom" });
  });

  it("still honours field-specific codes", () => {
    expect(errorsFrom(new ApiError(400, "temp_dir_not_writable", "Nope."))).toEqual({ temp_dir: "Nope." });
  });

  it("puts the error on the field the server names, before any guessing", () => {
    // The wording alone would point at output_folder; the server knows better.
    const err = new ApiError(
      400,
      "invalid_settings",
      "The output folder can't be the temporary folder.",
      "temp_dir",
    );
    expect(errorsFrom(err, ["output_folder", "temp_dir"])).toEqual({
      temp_dir: "The output folder can't be the temporary folder.",
    });
  });

  it("maps a field inside the default profile to the profile", () => {
    const err = new ApiError(400, "invalid_settings", "That quality value is too high for AV1.", "default_profile.quality_override");
    expect(errorsFrom(err)).toEqual({ default_profile: "That quality value is too high for AV1." });
  });

  it("falls back to the old heuristics when the field is unknown", () => {
    const err = new ApiError(400, "invalid_settings", "Active hours must be whole hours from 0 to 23.", "schedule");
    expect(errorsFrom(err)).toHaveProperty("active_hours");
  });
});

describe("settingForField", () => {
  it("knows every top-level setting and nothing else", () => {
    expect(settingForField("temp_dir")).toBe("temp_dir");
    expect(settingForField("ignore_patterns[2]")).toBe("ignore_patterns");
    expect(settingForField("default_profile.audio_languages")).toBe("default_profile");
    expect(settingForField("colour")).toBeNull();
    expect(settingForField("")).toBeNull();
    expect(settingForField(null)).toBeNull();
  });
});

describe("saveBarMessage", () => {
  const folderError = "The output folder can't be inside the library Smoke media.";
  const folderDraft: Settings = { ...base, output_mode: "folder", output_folder: "/media/out" };

  it("only points at a field error that is announced next to the field on screen", () => {
    expect(saveBarMessage({ errors: { output_folder: folderError }, draft: folderDraft, section: "output", advancedValid: true })).toEqual({
      message: "Fix the highlighted setting to save.",
      role: "status",
      blocked: false,
    });
  });

  it("spells out an error from another section, with its name", () => {
    expect(saveBarMessage({ errors: { output_folder: folderError }, draft: folderDraft, section: "processing", advancedValid: true })).toEqual({
      message: `${folderError} (Output)`,
      role: "alert",
      blocked: false,
    });
    // A setting without its own error line keeps the full message.
    expect(
      saveBarMessage({ errors: { rescan_interval_hours: "Rescan interval is too long." }, draft: base, section: "processing", advancedValid: true }).message,
    ).toBe("Rescan interval is too long.");
    expect(saveBarMessage({ errors: { general: "Server error." }, draft: base, section: "output", advancedValid: true }).role).toBe("alert");
  });

  it("keeps blocking while Advanced holds invalid text, from any section", () => {
    expect(saveBarMessage({ errors: {}, draft: base, section: "advanced", advancedValid: false })).toEqual({
      message: "Fix the highlighted fields to save.",
      role: "status",
      blocked: true,
    });
    expect(saveBarMessage({ errors: {}, draft: base, section: "processing", advancedValid: false })).toEqual({
      message: "Fix the highlighted setting in Advanced to save.",
      role: "status",
      blocked: true,
    });
  });

  it("names the missing folder when its section isn't on screen", () => {
    const missing: Settings = { ...base, output_mode: "folder", output_folder: null };
    expect(saveBarMessage({ errors: {}, draft: missing, section: "output", advancedValid: true }).message).toBe("Choose a folder to save.");
    expect(saveBarMessage({ errors: {}, draft: missing, section: "hardware", advancedValid: true })).toMatchObject({
      message: "Choose the output folder in Output to save.",
      blocked: true,
    });
    expect(saveBarMessage({ errors: {}, draft: { ...base, temp_dir: "" }, section: "advanced", advancedValid: true }).message).toBe(
      "Choose the work folder in Output to save.",
    );
    expect(saveBarMessage({ errors: {}, draft: base, section: "output", advancedValid: true })).toEqual({
      message: null,
      role: "status",
      blocked: false,
    });
  });
});

describe("nested profile fields", () => {
  it("finds the profile control a server field names", () => {
    expect(profileFieldOf("profile.quality", "profile")).toBe("quality");
    expect(profileFieldOf("profile.max_height", "profile")).toBe("max_height");
    expect(profileFieldOf("default_profile.quality_override", "default_profile")).toBe("quality_override");
    expect(profileFieldOf("default_profile.audio_languages[0]", "default_profile")).toBe("audio_languages");
    expect(profileFieldOf("profile.nonsense", "profile")).toBeNull();
    expect(profileFieldOf("default_profile.quality", "profile")).toBeNull();
    expect(profileFieldOf("name", "profile")).toBeNull();
    expect(profileFieldOf(null, "profile")).toBeNull();
  });

  it("points at a default-profile error shown under its control, and spells it out elsewhere", () => {
    const errors = { default_profile: 'The value for "default_profile.quality" isn\'t valid.' };
    expect(saveBarMessage({ errors, draft: base, section: "advanced", advancedValid: true, profileFieldShown: true })).toEqual({
      message: "Fix the highlighted setting to save.",
      role: "status",
      blocked: false,
    });
    expect(saveBarMessage({ errors, draft: base, section: "advanced", advancedValid: true }).role).toBe("alert");
    expect(
      saveBarMessage({ errors, draft: base, section: "output", advancedValid: true, profileFieldShown: true }).message,
    ).toBe(`${errors.default_profile} (Advanced)`);
  });

  it("puts a nested default-profile error on the default profile", () => {
    const err = new ApiError(400, "invalid_settings", "Pick a quality.", "default_profile.quality");
    expect(errorsFrom(err, ["default_profile"])).toEqual({ default_profile: "Pick a quality." });
  });
});

describe("sectionFor", () => {
  it("sends old verification links to Output, and anything unknown to Processing", () => {
    expect(sectionFor("verification")).toBe("output");
    expect(sectionFor("hardware")).toBe("hardware");
    expect(sectionFor("nope")).toBe("processing");
    expect(sectionFor(undefined)).toBe("processing");
  });
});
