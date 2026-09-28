/**
 * Plain-language readings of the hardware report: which GPU will do the
 * work, whether a codec encodes quickly here, and which goal fits best.
 */

import { GPU_VENDOR_LABEL, VIDEO_CODEC_LABEL } from "./labels";
import type { EncoderStatus, Goal, GpuVendor, HardwareInfo, HwApi, HwPreference, VideoCodec } from "./types";

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

export type SpeedTone = "fast" | "slow" | "blocked";

export interface SpeedHint {
  tone: SpeedTone;
  text: string;
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
  if (preference === "cpu") {
    return { tone: "slow", text: `Set to CPU only, and ${label} on the CPU is slower.` };
  }
  if (hw.gpus.length === 0) {
    return { tone: "slow", text: `No GPU found. ${label} on the CPU is slower.` };
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

/** The goal that suits this machine best. */
export function recommendedGoal(hw: HardwareInfo | undefined): Exclude<Goal, "custom"> {
  if (!hw) return "save_space";
  if (hasHardwareEncoder(hw, "av1")) return "save_space";
  if (hasHardwareEncoder(hw, "hevc")) return "balanced";
  return "save_space";
}

/** Start of the hint title the server shows while its first detection runs. */
const DETECTING_HINT = "Checking your hardware";

/**
 * Whether this report is the server's stand-in while the first detection is
 * still running (HTTP 200 with no encoders and a "Checking…" hint), rather
 * than real results. It must never be shown as "ffmpeg wasn't found".
 */
export function isDetecting(hw: HardwareInfo | undefined): boolean {
  if (!hw) return false;
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
