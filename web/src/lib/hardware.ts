/**
 * Plain-language readings of the hardware report: which GPU will do the
 * work, whether a codec encodes quickly here, and which goal fits best.
 */

import { GPU_VENDOR_LABEL, VIDEO_CODEC_LABEL } from "./labels";
import type { Goal, GpuVendor, HardwareInfo, HwApi, HwPreference, VideoCodec } from "./types";

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
    return { tone: "slow", text: `Set to CPU only — ${label} on the CPU is slower.` };
  }
  if (hw.gpus.length === 0) {
    return { tone: "slow", text: `No GPU found — ${label} on the CPU is slower.` };
  }
  return { tone: "slow", text: `Your GPU can't encode ${label} — the CPU will, which is slower.` };
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

/** APIs that have at least one encoder listed or a matching GPU, for the preference menu. */
export function relevantApis(hw: HardwareInfo): HwApi[] {
  const apis = new Set<HwApi>();
  for (const e of hw.encoders) {
    if (e.api !== "software" && (e.available || e.verified)) apis.add(e.api);
  }
  return [...apis];
}
