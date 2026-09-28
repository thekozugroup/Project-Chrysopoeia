/**
 * Plain-language readings of the hardware report: which GPU will do the
 * work, whether a codec encodes quickly here, and which goal fits best.
 */

import { GPU_VENDOR_LABEL, HW_PREFERENCE_LABEL, VIDEO_CODEC_LABEL } from "./labels";
import type {
  EncoderStatus,
  Goal,
  GpuVendor,
  HardwareInfo,
  HwApi,
  HwPreference,
  QueueState,
  Settings,
  SystemInfo,
  VideoCodec,
} from "./types";

const API_VENDORS: Partial<Record<HwApi, GpuVendor[]>> = {
  nvenc: ["nvidia"],
  qsv: ["intel"],
  vaapi: ["intel", "amd"],
  amf: ["amd"],
  videotoolbox: ["apple"],
};

/** Display name for the device behind a hardware API, e.g. "NVIDIA GeForce RTX 3060". */
export function deviceName(hw: HardwareInfo, api: HwApi): string {
  if (api === "software") return "CPU";
  const vendors = API_VENDORS[api];
  const gpu = vendors ? hw.gpus.find((g) => vendors.includes(g.vendor)) : hw.gpus[0];
  if (gpu?.name) return gpu.name;
  if (gpu) return `${GPU_VENDOR_LABEL[gpu.vendor]} GPU`;
  return "GPU";
}

/** Verified hardware encoders for a codec, honouring a pinned preference. */
function hardwareEncoder(hw: HardwareInfo, codec: VideoCodec, preference: HwPreference) {
  if (preference === "cpu") return undefined;
  return hw.encoders.find(
    (e) =>
      e.codec === codec &&
      e.verified &&
      e.api !== "software" &&
      (preference === "auto" || e.api === preference),
  );
}

/**
 * `fast`: a GPU encodes it. `cpu`: the CPU does all the work anyway, so the
 * text only compares formats. `slow`: a GPU is used for other formats but
 * not this one. `blocked`: can't be encoded at all.
 */
export type SpeedTone = "fast" | "cpu" | "slow" | "blocked";

export interface SpeedHint {
  tone: SpeedTone;
  text: string;
}

/**
 * How long each format takes on the CPU with the encoders Chrysopoeia uses
 * (x264, x265, libvpx-vp9, SVT-AV1), fastest first.
 */
const CPU_SPEED_TEXT: Record<VideoCodec, string> = {
  h264: "Quickest to convert on the CPU.",
  hevc: "Medium speed on the CPU.",
  vp9: "Slow to convert on the CPU.",
  av1: "Slowest to convert on the CPU.",
};

/**
 * Whether the CPU does every conversion: set to CPU only, or no hardware
 * encoder passed its test. Then speed is a comparison between formats.
 */
export function cpuDoesAllWork(hw: HardwareInfo, preference: HwPreference = "auto"): boolean {
  if (isDetecting(hw) || !hw.ffmpeg.found) return false;
  if (preference === "cpu") return true;
  return !hw.encoders.some(
    (e) => e.verified && e.api !== "software" && (preference === "auto" || e.api === preference),
  );
}

/** How quickly a format converts on this machine, in one or two words. */
export interface SpeedWord {
  tone: "fast" | "medium" | "slow" | "blocked";
  label: string;
}

/** CPU encoders from quickest to slowest (x264, x265, libvpx-vp9, SVT-AV1). */
const CPU_SPEED: Record<VideoCodec, SpeedWord["tone"]> = {
  h264: "fast",
  hevc: "medium",
  vp9: "slow",
  av1: "slow",
};

const SPEED_LABEL: Record<SpeedWord["tone"], string> = {
  fast: "Fast",
  medium: "Medium",
  slow: "Slow here",
  blocked: "Not available here",
};

/**
 * The speed of a format relative to this machine: "Fast" when a GPU does
 * it, otherwise how the CPU encoders compare. `null` until detection has
 * finished, so no guess is shown as a fact.
 */
export function speedWord(
  hw: HardwareInfo | undefined,
  codec: VideoCodec,
  preference: HwPreference = "auto",
): SpeedWord | null {
  if (!hw || isDetecting(hw)) return null;
  const hint = codecSpeedHint(hw, codec, preference);
  const tone: SpeedWord["tone"] =
    hint.tone === "fast" ? "fast" : hint.tone === "blocked" ? "blocked" : hint.tone === "slow" ? "slow" : CPU_SPEED[codec];
  return { tone, label: SPEED_LABEL[tone] };
}

/** Formats as a list for a sentence: "AV1, HEVC and H.264". */
function formatList(codecs: VideoCodec[]): string {
  const names = codecs.map((c) => VIDEO_CODEC_LABEL[c].replace(/ \(.*\)/, ""));
  return names.length <= 1 ? (names[0] ?? "") : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

/**
 * What does the converting here, in one sentence: "Everything converts on
 * the CPU." or "NVIDIA GeForce RTX 3060 converts HEVC and H.264. The rest
 * converts on the CPU."
 */
export function machineSummary(hw: HardwareInfo, preference: HwPreference = "auto"): string {
  if (!hw.ffmpeg.found) return "Nothing can be converted until ffmpeg is available.";
  if (cpuDoesAllWork(hw, preference)) {
    return preference === "cpu" ? "Set to use the CPU only, so everything converts on the CPU." : "Everything converts on the CPU.";
  }
  const byApi = new Map<HwApi, VideoCodec[]>();
  for (const e of hw.encoders) {
    if (!e.verified || e.api === "software" || (preference !== "auto" && e.api !== preference)) continue;
    const list = byApi.get(e.api) ?? [];
    if (!list.includes(e.codec)) list.push(e.codec);
    byApi.set(e.api, list);
  }
  const parts = [...byApi.entries()].map(([api, codecs]) => {
    const ordered = (["av1", "hevc", "h264", "vp9"] as VideoCodec[]).filter((c) => codecs.includes(c));
    return `${deviceName(hw, api)} converts ${formatList(ordered)}`;
  });
  const covered = new Set([...byApi.values()].flat());
  const rest = (["av1", "hevc", "h264", "vp9"] as VideoCodec[]).some(
    (c) => !covered.has(c) && hw.encoders.some((e) => e.codec === c && e.verified),
  );
  return `${parts.join(". ")}.${rest ? " The rest converts on the CPU." : ""}`;
}

/** How quickly this machine can produce a codec, in one sentence. */
export function codecSpeedHint(
  hw: HardwareInfo,
  codec: VideoCodec,
  preference: HwPreference = "auto",
): SpeedHint {
  const label = VIDEO_CODEC_LABEL[codec].replace(/ \(.*\)/, "");
  if (isDetecting(hw)) {
    return { tone: "slow", text: "Checking your hardware…" };
  }
  if (!hw.ffmpeg.found) {
    return { tone: "blocked", text: "ffmpeg wasn't found, so nothing can be converted yet." };
  }
  const hwEnc = hardwareEncoder(hw, codec, preference);
  if (hwEnc) {
    return { tone: "fast", text: `Your ${deviceName(hw, hwEnc.api)} can encode ${label} quickly.` };
  }
  const software = hw.encoders.some((e) => e.codec === codec && e.verified && e.api === "software");
  if (!software) {
    return { tone: "blocked", text: `This ffmpeg build can't encode ${label}.` };
  }
  if (cpuDoesAllWork(hw, preference)) {
    // Every format runs on the CPU here, so compare them rather than repeat
    // "slower" on each.
    return { tone: "cpu", text: CPU_SPEED_TEXT[codec] };
  }
  return { tone: "slow", text: `Your GPU can't encode ${label}. The CPU will, which is slower.` };
}

/** Whether a verified hardware encoder exists for a codec. */
export function hasHardwareEncoder(hw: HardwareInfo, codec: VideoCodec): boolean {
  return hw.encoders.some((e) => e.codec === codec && e.verified && e.api !== "software");
}

/** Whether any verified hardware encoder exists at all. */
export function hasAnyHardware(hw: HardwareInfo): boolean {
  return hw.encoders.some((e) => e.verified && e.api !== "software");
}

/**
 * The goal that suits this machine best: Save space (AV1) when a GPU encodes
 * AV1, otherwise Balanced (HEVC), which a GPU encodes quickly or, on the CPU,
 * finishes much sooner than AV1 (the hardware page's own advice). Balanced
 * is also the answer until detection has finished.
 */
export function recommendedGoal(hw: HardwareInfo | undefined): Exclude<Goal, "custom"> {
  if (hw && !isDetecting(hw) && hasHardwareEncoder(hw, "av1")) return "save_space";
  return "balanced";
}

/** Start of the hint title the server shows while its first detection runs. */
const DETECTING_HINT = "Checking your hardware";

/**
 * Whether this report is the server's stand-in while the first detection is
 * still running, rather than real results. It must never be shown as
 * "ffmpeg wasn't found". The server says so with `detecting`; servers from
 * before that flag are recognised by their stand-in (no encoders and a
 * "Checking…" hint).
 */
export function isDetecting(hw: HardwareInfo | undefined): boolean {
  if (!hw) return false;
  if (typeof hw.detecting === "boolean") return hw.detecting;
  return hw.encoders.length === 0 && hw.hints.some((h) => h.title.startsWith(DETECTING_HINT));
}

/** Failure text that means the device simply isn't there (not a broken one). */
const NO_DEVICE = /\b(no|not)\b.*\b(device|gpu|visible|found|passed|present|available)\b/i;

/**
 * Whether the device behind a hardware API is present on this machine: a GPU
 * of a matching vendor was detected, or one of its encoders passed a test.
 */
export function apiDevicePresent(hw: HardwareInfo, api: HwApi): boolean {
  if (api === "software") return true;
  if (hw.encoders.some((e) => e.api === api && e.verified)) return true;
  const vendors = API_VENDORS[api];
  if (!vendors) return false;
  return hw.gpus.some((g) => vendors.includes(g.vendor));
}

/** What one cell of the encoder matrix says. */
export type EncoderCell = "works" | "unsupported" | "failed" | "not_set_up" | "unavailable";

/**
 * Plain reading of one encoder: it works; the device works but can't do this
 * format (an older NVIDIA card and AV1); the device is present but its test
 * failed; the device isn't set up (ffmpeg lists encoders for hardware that
 * isn't there); or this ffmpeg can't produce the format at all.
 */
export function encoderCell(hw: HardwareInfo, encoder: EncoderStatus | undefined): EncoderCell {
  if (!encoder || (!encoder.available && !encoder.verified)) return "unavailable";
  if (encoder.verified) return "works";
  if (encoder.api === "software") return "failed";
  if (!apiDevicePresent(hw, encoder.api)) return "not_set_up";
  const siblingWorks = hw.encoders.some((e) => e.api === encoder.api && e.verified);
  if (siblingWorks) return "unsupported";
  if (encoder.error && NO_DEVICE.test(encoder.error)) return "not_set_up";
  return "failed";
}

/**
 * Hardware APIs worth a column in the encoder matrix: those whose device is
 * present. ffmpeg builds list NVENC, QSV and VA-API encoders on every host,
 * so listing them all would show a wall of failures on a CPU-only server.
 */
export function matrixApis(hw: HardwareInfo): HwApi[] {
  const apis = new Set<HwApi>();
  for (const e of hw.encoders) {
    if (e.api !== "software" && (e.available || e.verified) && apiDevicePresent(hw, e.api)) apis.add(e.api);
  }
  return [...apis];
}

/** A choice for "Use for converting". Disabled ones are shown but can't be picked. */
export interface PreferenceChoice {
  value: HwPreference;
  disabled: boolean;
}

/**
 * Options for the hardware preference: Automatic and CPU, every API with a
 * verified encoder, and (disabled) APIs whose device is present but whose
 * encoders all failed. The current value is always kept so it stays visible.
 */
export function preferenceChoices(
  hw: HardwareInfo | undefined,
  current: HwPreference,
  apiOf: Record<HwPreference, HwApi | null>,
): PreferenceChoice[] {
  const choices: PreferenceChoice[] = [
    { value: "auto", disabled: false },
    { value: "cpu", disabled: false },
  ];
  for (const [pref, api] of Object.entries(apiOf) as [HwPreference, HwApi | null][]) {
    if (!api || api === "software" || !hw) continue;
    const verified = hw.encoders.some((e) => e.api === api && e.verified);
    if (verified) choices.push({ value: pref, disabled: false });
    else if (apiDevicePresent(hw, api) && hw.encoders.some((e) => e.api === api && e.available)) {
      choices.push({ value: pref, disabled: true });
    }
  }
  if (!choices.some((c) => c.value === current)) choices.push({ value: current, disabled: false });
  return choices;
}

/**
 * The facts a bug report needs, as plain lines to paste: version and build,
 * container, ffmpeg, GPUs, verified encoders, hardware preference and files
 * at once with where that number comes from. Parts not loaded yet are left
 * out rather than guessed.
 */
export function bugReportText({
  system,
  hw,
  queue,
  settings,
}: {
  system: Pick<SystemInfo, "version" | "build" | "in_container">;
  hw?: Pick<HardwareInfo, "ffmpeg" | "gpus" | "encoders" | "cpu"> | undefined;
  queue?: Pick<QueueState, "max_jobs" | "max_jobs_auto" | "max_jobs_source"> | undefined;
  settings?: Pick<Settings, "hardware"> | undefined;
}): string {
  const lines = [
    `Chrysopoeia ${system.version}${system.build ? ` (build ${system.build})` : ""}${system.in_container ? ", in a container" : ""}`,
  ];
  if (hw) {
    lines.push(`ffmpeg: ${hw.ffmpeg.found ? (hw.ffmpeg.version ?? "found, version unknown") : "not found"}`);
    lines.push(`CPU: ${hw.cpu.model || "unknown"} (${hw.cpu.logical_cores} threads)`);
    lines.push(
      `GPUs: ${hw.gpus.length ? hw.gpus.map((g) => `${g.name || GPU_VENDOR_LABEL[g.vendor]}${g.driver ? ` (${g.driver})` : ""}`).join(", ") : "none found"}`,
    );
    const verified = hw.encoders.filter((e) => e.verified).map((e) => e.name);
    lines.push(`Verified encoders: ${verified.length ? verified.join(", ") : "none"}`);
  }
  if (settings) lines.push(`Hardware preference: ${HW_PREFERENCE_LABEL[settings.hardware] ?? settings.hardware}`);
  if (queue) {
    const source = queue.max_jobs_source ?? (queue.max_jobs_auto ? "auto" : "settings");
    const from = source === "env" ? "from MAX_JOBS" : source === "auto" ? "automatic" : "set in Settings";
    lines.push(`Files at once: ${queue.max_jobs} (${from})`);
  }
  return lines.join("\n");
}
