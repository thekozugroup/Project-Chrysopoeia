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
  if (next.min_savings_pct !== null) next.min_savings_pct = Math.min(90, next.min_savings_pct);
  return { profile: next, notes };
}

/** Whether two profiles are the same, field by field. */
export function sameProfile(a: TranscodeProfile, b: TranscodeProfile): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
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
