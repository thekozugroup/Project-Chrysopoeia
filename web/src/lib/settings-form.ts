/**
 * Helpers for the Settings form: which settings changed (so a save sends
 * only those), and which field an API error is about.
 */

import { ApiError, errorMessage } from "./api";
import { sameProfile } from "./profile";
import type { Settings } from "./types";

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

/**
 * Put an API error on the field it is about. The server answers every
 * validation failure with `invalid_settings`, so the field is found from (in
 * order) a field-specific code, the setting named in `"value for \"key\""`,
 * the words of the message, or the only key that was sent.
 */
export function errorsFrom(err: unknown, sent: (keyof Settings)[] = []): FieldErrors {
  const message = errorMessage(err);
  if (!(err instanceof ApiError) || err.status >= 500 || err.isNetwork) return { general: message };
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
