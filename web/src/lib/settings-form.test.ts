import { describe, expect, it } from "vitest";
import { ApiError } from "./api";
import { profileForGoal } from "./profile";
import { changedKeys, errorsFrom } from "./settings-form";
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
});
