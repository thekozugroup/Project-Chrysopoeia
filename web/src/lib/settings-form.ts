/**
 * Helpers for the Settings form: which settings changed (so a save sends
 * only those), and which field an API error is about.
 */

import { ApiError, errorMessage } from "./api";
import { sameProfile } from "./profile";
import type { Settings, TranscodeProfile } from "./types";

/**
 * The sections of the Settings screen. Verification lives in Output
 * ("Checks before replacing"); old `#/settings/verification` links land
 * there.
 */
export type SectionId = "processing" | "output" | "hardware" | "advanced";

export const SECTION_LABEL: Record<SectionId, string> = {
  processing: "Processing",
  output: "Output",
  hardware: "Hardware",
  advanced: "Advanced",
};

/** The section a `#/settings/<name>` address shows, including retired names. */
export function sectionFor(name: string | undefined): SectionId {
  if (name === "verification") return "output";
  return name && name in SECTION_LABEL ? (name as SectionId) : "processing";
}

/** Which section shows each setting, to point at it from the save bar. */
export const SECTION_OF: Partial<Record<keyof Settings, SectionId>> = {
  max_jobs: "processing",
  active_hours: "processing",
  low_priority: "processing",
  auto_queue: "processing",
  watch_folders: "processing",
  rescan_interval_hours: "processing",
  output_mode: "output",
  output_folder: "output",
  keep_file_dates: "output",
  temp_dir: "output",
  validation: "output",
  hardware: "hardware",
  cpu_fallback: "hardware",
  ignore_patterns: "advanced",
  min_file_size_mb: "advanced",
  default_profile: "advanced",
};

/** Errors to show, by setting, plus `general` for anything else. */
export type FieldErrors = Partial<Record<keyof Settings | "general", string>>;

/** Top-level keys whose values differ. `default_profile` is compared field by field. */
export function changedKeys(a: Settings, b: Settings): (keyof Settings)[] {
  const keys = new Set([...Object.keys(a), ...Object.keys(b)] as (keyof Settings)[]);
  return [...keys].filter((key) =>
    key === "default_profile"
      ? !sameProfile(a.default_profile, b.default_profile)
      : JSON.stringify(a[key]) !== JSON.stringify(b[key]),
  );
}

/** Words in an error message that name the setting it is about. */
const MESSAGE_FIELDS: [RegExp, keyof Settings][] = [
  [/output folder/i, "output_folder"],
  [/temporary folder|work folder|temp(orary)? dir/i, "temp_dir"],
  [/active hours/i, "active_hours"],
  [/jobs at once|files at once/i, "max_jobs"],
  [/ignore pattern/i, "ignore_patterns"],
  [/smaller than|minimum file size/i, "min_file_size_mb"],
  [/rescan/i, "rescan_interval_hours"],
];

/** Every top-level setting, to recognise a `field` the server names. */
const SETTING_KEYS: readonly (keyof Settings)[] = [
  "auto_queue",
  "watch_folders",
  "rescan_interval_hours",
  "max_jobs",
  "hardware",
  "cpu_fallback",
  "validation",
  "output_mode",
  "output_folder",
  "temp_dir",
  "keep_file_dates",
  "low_priority",
  "active_hours",
  "ignore_patterns",
  "min_file_size_mb",
  "default_profile",
  "onboarded",
];

/**
 * The setting a server-named `field` belongs to: `temp_dir` as is, and
 * anything inside the default profile (`default_profile.quality_override`)
 * on `default_profile`.
 */
export function settingForField(field: string | null | undefined): keyof Settings | null {
  if (!field) return null;
  const top = field.split(/[.[]/, 1)[0] as keyof Settings;
  return SETTING_KEYS.includes(top) ? top : null;
}

/** Every profile field, to recognise `profile.quality` or `default_profile.max_height`. */
const PROFILE_KEYS: readonly (keyof TranscodeProfile)[] = [
  "goal",
  "video_codec",
  "audio_codec",
  "container",
  "quality",
  "speed",
  "quality_override",
  "max_height",
  "subtitles",
  "audio_languages",
  "subtitle_languages",
  "skip_efficient",
  "min_savings_pct",
];

/**
 * The profile control a server-named `field` is about, when it names one
 * inside `prefix` (`profile` for a library, `default_profile` in Settings):
 * `profile.quality` → `quality`. `null` otherwise.
 */
export function profileFieldOf(
  field: string | null | undefined,
  prefix: "profile" | "default_profile",
): keyof TranscodeProfile | null {
  if (!field?.startsWith(`${prefix}.`)) return null;
  const key = field.slice(prefix.length + 1).split(/[.[]/, 1)[0] as keyof TranscodeProfile;
  return PROFILE_KEYS.includes(key) ? key : null;
}

/** Errors for the profile editor's controls, by field. */
export type ProfileErrors = Partial<Record<keyof TranscodeProfile, string>>;

/**
 * Put an API error on the field it is about. Newer servers name it in the
 * error's `field`; otherwise (every validation failure is
 * `invalid_settings`) the field is found from, in order, a field-specific
 * code, the setting named in `"value for \"key\""`, the words of the
 * message, or the only key that was sent.
 */
export function errorsFrom(err: unknown, sent: (keyof Settings)[] = []): FieldErrors {
  const message = errorMessage(err);
  if (!(err instanceof ApiError) || err.status >= 500 || err.isNetwork) return { general: message };
  const fromServer = settingForField(err.field);
  if (fromServer) return { [fromServer]: message };
  const code = err.code;
  const byCode: [string, keyof Settings][] = [
    ["output", "output_folder"],
    ["temp", "temp_dir"],
    ["hours", "active_hours"],
    ["jobs", "max_jobs"],
    ["pattern", "ignore_patterns"],
    ["file_size", "min_file_size_mb"],
  ];
  for (const [part, field] of byCode) {
    if (code.includes(part)) return { [field]: message };
  }
  const named = /value for "([a-z_]+)"/i.exec(message)?.[1] as keyof Settings | undefined;
  if (named) return { [named]: message };
  for (const [pattern, field] of MESSAGE_FIELDS) {
    if (pattern.test(message)) return { [field]: message };
  }
  if (sent.length === 1 && code !== "unknown_setting") return { [sent[0]]: message };
  return { general: message };
}

/** Settings whose section shows their error under the field itself. */
function errorShownInline(field: keyof Settings, draft: Settings, profileFieldShown: boolean): boolean {
  switch (field) {
    case "default_profile":
      return profileFieldShown;
    case "max_jobs":
    case "active_hours":
    case "ignore_patterns":
    case "min_file_size_mb":
      return true;
    case "output_folder":
      return draft.output_mode === "folder";
    case "temp_dir":
      return draft.temp_dir !== null;
    default:
      return false;
  }
}

/**
 * What the save bar says, and whether saving is blocked. A server error that
 * is already shown (and announced) under its field on screen is only pointed
 * at, quietly; one in another section is spelled out with the section's
 * name. Invalid text in Advanced blocks saving from every section, since
 * it would otherwise be dropped without a word.
 */
export function saveBarMessage({
  errors,
  draft,
  section,
  advancedValid,
  profileFieldShown = false,
}: {
  errors: FieldErrors;
  draft: Settings;
  section: SectionId;
  advancedValid: boolean;
  /** The `default_profile` error names a control the editor shows it under. */
  profileFieldShown?: boolean;
}): { message: string | null; role: "alert" | "status"; blocked: boolean } {
  const outputMissing = draft.output_mode === "folder" && !draft.output_folder;
  const tempMissing = draft.temp_dir !== null && draft.temp_dir === "";
  const blockedMessage =
    outputMissing || tempMissing
      ? section === "output"
        ? "Choose a folder to save."
        : `Choose ${outputMissing ? "the output folder" : "the work folder"} in Output to save.`
      : !advancedValid
        ? section === "advanced"
          ? "Fix the highlighted fields to save."
          : "Fix the highlighted setting in Advanced to save."
        : null;
  const blocked = blockedMessage !== null;
  if (errors.general) return { message: errors.general, role: "alert", blocked };
  const first = Object.entries(errors).find(([key]) => key !== "general") as [keyof Settings, string] | undefined;
  if (first) {
    const [field, message] = first;
    const fieldSection = SECTION_OF[field];
    if (fieldSection === section && errorShownInline(field, draft, profileFieldShown)) {
      return { message: "Fix the highlighted setting to save.", role: "status", blocked };
    }
    if (fieldSection && fieldSection !== section) {
      return { message: `${message} (${SECTION_LABEL[fieldSection]})`, role: "alert", blocked };
    }
    return { message, role: "alert", blocked };
  }
  return { message: blockedMessage, role: "status", blocked };
}
