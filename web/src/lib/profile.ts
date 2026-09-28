/**
 * Profile rules on the client. `GET /api/presets` is the source of truth;
 * the tables here mirror `chrysopoeia-core` (codec.rs, profile.rs) so the
 * editor still works, and stays valid, if presets have not loaded.
 */

import { AUDIO_CODEC_LABEL, CONTAINER_LABEL, VIDEO_CODEC_LABEL } from "./labels";
import type {
  AudioCodec,
  Container,
  Goal,
  Presets,
  TranscodeProfile,
  VideoCodec,
} from "./types";
import { AUDIO_CODECS, CONTAINERS, VIDEO_CODECS } from "./types";

const FALLBACK_CONTAINER_VIDEO: Record<Container, VideoCodec[]> = {
  mkv: ["av1", "hevc", "h264", "vp9"],
  mp4: ["av1", "hevc", "h264", "vp9"],
  webm: ["av1", "vp9"],
};

const FALLBACK_CONTAINER_AUDIO: Record<Container, AudioCodec[]> = {
  mkv: [...AUDIO_CODECS],
  mp4: ["copy", "aac", "ac3", "eac3", "opus", "mp3"],
  webm: ["copy", "opus", "vorbis"],
};

const FALLBACK_AUDIO: Record<Container, AudioCodec> = { mkv: "opus", mp4: "aac", webm: "opus" };

/** `TranscodeProfile::from_goal` from the core crate. */
export function profileForGoal(goal: Goal): TranscodeProfile {
  const base: TranscodeProfile = {
    goal,
    video_codec: "av1",
    audio_codec: "opus",
    container: "mkv",
    quality: "balanced",
    speed: "balanced",
    quality_override: null,
    max_height: null,
    subtitles: "keep",
    audio_languages: [],
    subtitle_languages: [],
    skip_efficient: true,
    min_savings_pct: 10,
  };
  switch (goal) {
    case "balanced":
      return { ...base, video_codec: "hevc", audio_codec: "copy" };
    case "compatible":
      return {
        ...base,
        video_codec: "h264",
        audio_codec: "aac",
        container: "mp4",
        quality: "high",
        skip_efficient: false,
        min_savings_pct: null,
      };
    case "archive":
      return { ...base, audio_codec: "copy", quality: "best", speed: "thorough", min_savings_pct: 5 };
    default:
      return base;
  }
}

/** The preset profile for a goal, from the server when available. */
export function presetProfile(presets: Presets | undefined, goal: Goal): TranscodeProfile {
  const fromServer = presets?.goals.find((g) => g.goal === goal)?.profile;
  return fromServer ? { ...fromServer } : profileForGoal(goal);
}

/** Video codecs a container can hold. */
export function videoCodecsFor(presets: Presets | undefined, container: Container): VideoCodec[] {
  return presets?.containers.find((c) => c.container === container)?.video ?? FALLBACK_CONTAINER_VIDEO[container];
}

/** Audio codecs a container can hold (always includes "copy"). */
export function audioCodecsFor(presets: Presets | undefined, container: Container): AudioCodec[] {
  const list = presets?.containers.find((c) => c.container === container)?.audio ?? FALLBACK_CONTAINER_AUDIO[container];
  return list.includes("copy") ? list : ["copy", ...list];
}

/** Containers that can hold a video codec. */
export function containersFor(presets: Presets | undefined, codec: VideoCodec): Container[] {
  return CONTAINERS.filter((c) => videoCodecsFor(presets, c).includes(codec));
}

/** All video codecs, in server order when available. */
export function allVideoCodecs(presets: Presets | undefined): VideoCodec[] {
  return presets?.video_codecs.map((v) => v.codec) ?? [...VIDEO_CODECS];
}

/**
 * Fix combinations a container cannot hold, like `TranscodeProfile::normalize`.
 * Returns the fixed profile and a sentence for each change.
 */
export function normalizeProfile(
  presets: Presets | undefined,
  profile: TranscodeProfile,
): { profile: TranscodeProfile; notes: string[] } {
  const next = { ...profile };
  const notes: string[] = [];
  if (!videoCodecsFor(presets, next.container).includes(next.video_codec)) {
    const old = next.container;
    next.container = "mkv";
    notes.push(
      `${CONTAINER_LABEL[old]} can't hold ${VIDEO_CODEC_LABEL[next.video_codec]} video, so the container was changed to MKV.`,
    );
  }
  if (!audioCodecsFor(presets, next.container).includes(next.audio_codec)) {
    const fallback = FALLBACK_AUDIO[next.container];
    notes.push(
      `${CONTAINER_LABEL[next.container]} can't hold ${AUDIO_CODEC_LABEL[next.audio_codec]} audio, so audio will be ${AUDIO_CODEC_LABEL[fallback]}.`,
    );
    next.audio_codec = fallback;
  }
  if (next.quality_override !== null) {
    const max = qualityOverrideMax(next.video_codec);
    if (next.quality_override > max) {
      notes.push(
        `${VIDEO_CODEC_LABEL[next.video_codec]} accepts encoder quality values up to ${max}, so it was lowered to ${max}.`,
      );
      next.quality_override = max;
    }
  }
  if (next.min_savings_pct !== null) next.min_savings_pct = Math.min(90, next.min_savings_pct);
  return { profile: next, notes };
}

/**
 * Highest raw quality value (CRF / CQ / QP) the encoders for a codec accept:
 * 63 for AV1 and VP9, 51 for H.264 and HEVC (x264, x265, NVENC, QSV).
 */
export function qualityOverrideMax(codec: VideoCodec): number {
  return codec === "av1" || codec === "vp9" ? 63 : 51;
}

/**
 * Check a typed raw quality value. Empty means automatic (`null`). Returns an
 * error sentence for anything the encoder would reject.
 */
export function parseQualityOverride(
  text: string,
  codec: VideoCodec,
): { value: number | null; error: string | null } {
  const trimmed = text.trim();
  if (!trimmed) return { value: null, error: null };
  const max = qualityOverrideMax(codec);
  const n = Number(trimmed);
  if (!Number.isInteger(n) || n < 0 || n > max) {
    return {
      value: null,
      error: `Use a whole number from 0 to ${max} for ${VIDEO_CODEC_LABEL[codec]}, or leave it empty.`,
    };
  }
  return { value: n, error: null };
}

/** Fields that describe the library's tracks and limits rather than its goal. */
function libraryFields(profile: TranscodeProfile): Partial<TranscodeProfile> {
  return {
    max_height: profile.max_height,
    subtitles: profile.subtitles,
    audio_languages: profile.audio_languages,
    subtitle_languages: profile.subtitle_languages,
  };
}

/** A goal's preset, keeping the track and resolution choices of `from`. */
export function profileWithGoal(
  presets: Presets | undefined,
  goal: Exclude<Goal, "custom">,
  from: TranscodeProfile,
): TranscodeProfile {
  return { ...presetProfile(presets, goal), ...libraryFields(from), goal };
}

/**
 * Whether the defaults for new libraries were changed from the stock preset
 * of their goal (in Settings › Advanced). Untouched defaults let the goal
 * step recommend a goal from the hardware instead.
 */
export function defaultsCustomized(presets: Presets | undefined, defaults: TranscodeProfile): boolean {
  if (defaults.goal === "custom") return true;
  return !sameProfile(defaults, presetProfile(presets, defaults.goal));
}

/**
 * The profile to create a library with. `"defaults"` means the defaults for
 * new libraries as they are; a goal means that goal's preset, keeping the
 * defaults' track and resolution choices.
 */
export function profileForNewLibrary(
  presets: Presets | undefined,
  defaults: TranscodeProfile,
  choice: Exclude<Goal, "custom"> | "defaults",
): TranscodeProfile {
  if (choice === "defaults") return { ...defaults };
  return profileWithGoal(presets, choice, defaults);
}

/** Whether two profiles are the same, field by field (key order doesn't matter). */
export function sameProfile(a: TranscodeProfile, b: TranscodeProfile): boolean {
  const left = a as unknown as Record<string, unknown>;
  const right = b as unknown as Record<string, unknown>;
  const fields = new Set([...Object.keys(left), ...Object.keys(right)]);
  for (const field of fields) {
    if (JSON.stringify(left[field]) !== JSON.stringify(right[field])) return false;
  }
  return true;
}

/** Parse "eng, jpn" into codes. Returns an error sentence for bad input. */
export function parseLanguages(text: string): { codes: string[]; error: string | null } {
  const codes = text
    .split(/[\s,;]+/)
    .map((c) => c.trim().toLowerCase())
    .filter(Boolean);
  const bad = codes.filter((c) => !/^[a-z]{2,3}$/.test(c));
  if (bad.length) {
    return {
      codes,
      error: `Use two- or three-letter language codes like eng or jpn. Not valid: ${bad.join(", ")}.`,
    };
  }
  return { codes: [...new Set(codes)], error: null };
}
