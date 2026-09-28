/**
 * Plain-language wording for every enum the API returns. Jargon (codec and
 * encoder names) is only ever secondary detail next to these labels.
 */

import type {
  AudioCodec,
  CheckStatus,
  Container,
  FileStatus,
  Goal,
  HwApi,
  HwPreference,
  JobStage,
  JobState,
  QualityLevel,
  SpeedPreset,
  ValidationLevel,
  VideoCodec,
  GpuVendor,
  HdrFormat,
  TranscodeProfile,
} from "./types";

export const FILE_STATUS_LABEL: Record<FileStatus, string> = {
  pending: "To convert",
  queued: "In queue",
  processing: "Converting",
  done: "Converted",
  skipped: "Skipped",
  failed: "Failed",
};

export const FILE_STATUS_HELP: Record<FileStatus, string> = {
  pending: "Needs converting but isn't in the queue yet.",
  queued: "Waiting in the queue.",
  processing: "Being converted right now.",
  done: "Converted and verified.",
  skipped: "Left as it is. The reason is shown with the file.",
  failed: "The last attempt failed. The original is untouched.",
};

export const JOB_STATE_LABEL: Record<JobState, string> = {
  queued: "In queue",
  running: "Converting",
  done: "Done",
  skipped: "Kept original",
  failed: "Failed",
  cancelled: "Cancelled",
};

export const JOB_STAGE_LABEL: Record<JobStage, string> = {
  waiting: "Waiting",
  preparing: "Preparing",
  transcoding: "Converting",
  verifying: "Checking quality",
  finalizing: "Finishing",
};

/** The stages a running job moves through, in order. */
export const JOB_STAGES: readonly JobStage[] = ["preparing", "transcoding", "verifying", "finalizing"];

export const CHECK_STATUS_LABEL: Record<CheckStatus, string> = {
  pass: "Passed",
  warn: "Warning",
  fail: "Failed",
  skipped: "Not checked",
};

export const GOAL_LABEL: Record<Goal, string> = {
  save_space: "Save space",
  balanced: "Balanced",
  compatible: "Plays everywhere",
  archive: "Archive",
  custom: "Custom",
};

export const GOAL_SUMMARY: Record<Goal, string> = {
  save_space: "Smallest files. Takes longest without a recent GPU.",
  balanced: "Good savings, quick with a GPU, plays on most TVs.",
  compatible: "Plays on every device. Files may not get smaller.",
  archive: "Near-original quality for keeping masters. Smaller savings.",
  custom: "Your own combination of format and quality.",
};

/** Goals offered as cards, in display order. */
export const GOALS: readonly Exclude<Goal, "custom">[] = [
  "save_space",
  "balanced",
  "compatible",
  "archive",
];

export const QUALITY_LABEL: Record<QualityLevel, string> = {
  smallest: "Smallest",
  small: "Small",
  balanced: "Recommended",
  high: "High",
  best: "Best quality",
};

export const QUALITY_HELP: Record<QualityLevel, string> = {
  smallest: "Biggest savings. Fine on phones and small screens; softer on a big TV.",
  small: "Large savings with a small loss in fine detail.",
  balanced: "Looks like the original on most screens. A good default.",
  high: "Hard to tell apart from the original, even on a large TV.",
  best: "Visually identical for nearly all content. Smaller savings.",
};

export const SPEED_LABEL: Record<SpeedPreset, string> = {
  fast: "Faster",
  balanced: "Normal",
  thorough: "Smaller files",
};

export const SPEED_HELP: Record<SpeedPreset, string> = {
  fast: "Finishes sooner. Files come out a little larger.",
  balanced: "A sensible trade between time and size.",
  thorough: "Takes longer. Files come out a little smaller at the same quality.",
};

export const VIDEO_CODEC_LABEL: Record<VideoCodec, string> = {
  av1: "AV1",
  hevc: "HEVC (H.265)",
  h264: "H.264",
  vp9: "VP9",
};

export const VIDEO_CODEC_HELP: Record<VideoCodec, string> = {
  av1: "Smallest files. Plays on recent TVs, phones and browsers.",
  hevc: "Small files. Plays on most TVs and streaming boxes.",
  h264: "Plays on everything, including old devices. Largest files.",
  vp9: "Royalty-free, good for web playback.",
};

export const AUDIO_CODEC_LABEL: Record<AudioCodec, string> = {
  copy: "Keep original",
  opus: "Opus",
  aac: "AAC",
  flac: "FLAC (lossless)",
  ac3: "Dolby Digital (AC-3)",
  eac3: "Dolby Digital Plus (E-AC-3)",
  mp3: "MP3",
  vorbis: "Vorbis",
};

export const CONTAINER_LABEL: Record<Container, string> = {
  mkv: "MKV",
  mp4: "MP4",
  webm: "WebM",
};

export const VALIDATION_LABEL: Record<ValidationLevel, string> = {
  off: "Off",
  quick: "Quick",
  standard: "Standard",
  thorough: "Thorough",
};

export const VALIDATION_HELP: Record<ValidationLevel, string> = {
  off: "No checks. A damaged file could replace a good one. Not recommended.",
  quick: "Checks that the new file opens, has every track, and is the right length. Takes seconds.",
  standard:
    "Also plays the whole file and compares four moments against the original to catch corruption and visual glitches.",
  thorough:
    "Compares ten moments and makes sure no black or frozen frames were added. Slowest, most careful.",
};

export const HW_API_LABEL: Record<HwApi, string> = {
  software: "CPU",
  nvenc: "NVIDIA GPU",
  qsv: "Intel Quick Sync",
  vaapi: "GPU (VA-API)",
  videotoolbox: "Apple GPU",
  amf: "AMD GPU",
  rkmpp: "Rockchip",
  v4l2m2m: "V4L2",
};

/** Technical name of the acceleration API, shown as secondary detail. */
export const HW_API_TECH: Record<HwApi, string> = {
  software: "Software",
  nvenc: "NVENC",
  qsv: "QSV",
  vaapi: "VA-API",
  videotoolbox: "VideoToolbox",
  amf: "AMF",
  rkmpp: "RKMPP",
  v4l2m2m: "V4L2 M2M",
};

export const HW_PREFERENCE_LABEL: Record<HwPreference, string> = {
  auto: "Automatic",
  cpu: "CPU only",
  nvenc: "NVIDIA (NVENC)",
  qsv: "Intel Quick Sync",
  vaapi: "VA-API (Intel or AMD)",
  videotoolbox: "Apple VideoToolbox",
  amf: "AMD AMF",
  rkmpp: "Rockchip MPP",
  v4l2m2m: "V4L2 (ARM boards)",
};

export const HW_PREFERENCE_API: Record<HwPreference, HwApi | null> = {
  auto: null,
  cpu: "software",
  nvenc: "nvenc",
  qsv: "qsv",
  vaapi: "vaapi",
  videotoolbox: "videotoolbox",
  amf: "amf",
  rkmpp: "rkmpp",
  v4l2m2m: "v4l2m2m",
};

export const GPU_VENDOR_LABEL: Record<GpuVendor, string> = {
  nvidia: "NVIDIA",
  intel: "Intel",
  amd: "AMD",
  apple: "Apple",
  other: "GPU",
};

export const HDR_LABEL: Record<HdrFormat, string> = {
  hdr10: "HDR10",
  hdr10_plus: "HDR10+",
  hlg: "HLG",
  dolby_vision: "Dolby Vision",
};

/** Resolution choices for "Limit resolution". `null` keeps the source. */
export const MAX_HEIGHTS: readonly { value: number | null; label: string }[] = [
  { value: null, label: "Keep original" },
  { value: 2160, label: "4K (2160p)" },
  { value: 1440, label: "1440p" },
  { value: 1080, label: "1080p" },
  { value: 720, label: "720p" },
  { value: 480, label: "480p" },
];

/** Minimum-savings choices. `null` keeps every result. */
export const MIN_SAVINGS: readonly { value: number | null; label: string }[] = [
  { value: null, label: "Keep every result" },
  { value: 5, label: "At least 5% smaller" },
  { value: 10, label: "At least 10% smaller" },
  { value: 15, label: "At least 15% smaller" },
  { value: 20, label: "At least 20% smaller" },
  { value: 30, label: "At least 30% smaller" },
];

/** Short technical summary of a profile: `AV1 · Opus · MKV`. */
export function profileSummary(profile: TranscodeProfile): string {
  const video = VIDEO_CODEC_LABEL[profile.video_codec].replace(/ \(.*\)/, "");
  const audio =
    profile.audio_codec === "copy" ? "original audio" : AUDIO_CODEC_LABEL[profile.audio_codec].replace(/ \(.*\)/, "");
  return `${video} · ${audio} · ${CONTAINER_LABEL[profile.container]}`;
}

/** Human-friendly name for a codec string reported by ffprobe. */
export function sourceCodecLabel(name: string | null | undefined): string {
  if (!name) return "Unknown";
  const map: Record<string, string> = {
    h264: "H.264",
    hevc: "HEVC",
    av1: "AV1",
    vp9: "VP9",
    vp8: "VP8",
    mpeg2video: "MPEG-2",
    mpeg4: "MPEG-4",
    msmpeg4v3: "DivX",
    vc1: "VC-1",
    wmv3: "WMV",
    prores: "ProRes",
    aac: "AAC",
    ac3: "AC-3",
    eac3: "E-AC-3",
    dts: "DTS",
    truehd: "TrueHD",
    opus: "Opus",
    flac: "FLAC",
    mp3: "MP3",
    mp2: "MP2",
    vorbis: "Vorbis",
    pcm_s16le: "PCM",
    pcm_s24le: "PCM",
    subrip: "SRT",
    ass: "ASS",
    hdmv_pgs_subtitle: "PGS",
    dvd_subtitle: "VobSub",
    mov_text: "MP4 text",
    webvtt: "WebVTT",
    matroska: "MKV",
    mov: "MP4",
    mpegts: "MPEG-TS",
    avi: "AVI",
  };
  const known = map[name.toLowerCase()];
  if (known) return known;
  // Short codec-like tokens ("dvvideo") read best in capitals; anything else
  // (the server's "No video" bucket, for one) is already a label.
  return /^[a-z0-9_]+$/.test(name) ? name.toUpperCase() : name;
}

/** Human channel count: `Stereo`, `5.1`, `7.1`, `Mono`. */
export function channelsLabel(channels: number | null, layout: string | null): string {
  if (layout) {
    const base = layout.replace(/\(.*\)/, "");
    if (base === "stereo") return "Stereo";
    if (base === "mono") return "Mono";
    if (/^\d\.\d$/.test(base)) return base;
  }
  if (!channels) return "—";
  if (channels === 1) return "Mono";
  if (channels === 2) return "Stereo";
  if (channels === 6) return "5.1";
  if (channels === 8) return "7.1";
  return `${channels} channels`;
}

const LANGUAGE_NAMES: Record<string, string> = {
  eng: "English",
  en: "English",
  jpn: "Japanese",
  ja: "Japanese",
  fra: "French",
  fre: "French",
  fr: "French",
  deu: "German",
  ger: "German",
  de: "German",
  spa: "Spanish",
  es: "Spanish",
  ita: "Italian",
  it: "Italian",
  por: "Portuguese",
  pt: "Portuguese",
  rus: "Russian",
  ru: "Russian",
  kor: "Korean",
  ko: "Korean",
  zho: "Chinese",
  chi: "Chinese",
  zh: "Chinese",
  nld: "Dutch",
  dut: "Dutch",
  swe: "Swedish",
  nor: "Norwegian",
  dan: "Danish",
  fin: "Finnish",
  pol: "Polish",
  hin: "Hindi",
  ara: "Arabic",
  und: "Unknown",
};

/** `eng` → `English`; unknown codes are shown as-is. */
export function languageLabel(code: string | null | undefined): string {
  if (!code) return "No language";
  return LANGUAGE_NAMES[code.toLowerCase()] ?? code;
}
