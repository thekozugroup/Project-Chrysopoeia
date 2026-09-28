import { describe, expect, it } from "vitest";
import { ApiError } from "./api";
import { profileForGoal } from "./profile";
import { changedKeys, errorsFrom, settingForField } from "./settings-form";
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
