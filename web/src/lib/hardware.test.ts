import { describe, expect, it } from "vitest";
import {
  codecSpeedHint,
  encoderCell,
  isDetecting,
  matrixApis,
  preferenceChoices,
  recommendedGoal,
} from "./hardware";
import { HW_PREFERENCE_API } from "./labels";
import type { EncoderStatus, HardwareInfo } from "./types";

function enc(name: string, codec: EncoderStatus["codec"], api: EncoderStatus["api"], verified: boolean, error: string | null = null): EncoderStatus {
  return { name, codec, api, available: true, verified, device: null, error };
}

function hardware(partial: Partial<HardwareInfo>): HardwareInfo {
  return {
    cpu: { model: "Test CPU", logical_cores: 8, physical_cores: 4, cgroup_limit: null },
    memory: { total_bytes: 16e9, available_bytes: 8e9, cgroup_limit_bytes: null },
    gpus: [],
    encoders: [enc("libx265", "hevc", "software", true), enc("libsvtav1", "av1", "software", true)],
    audio_encoders: [],
    filters: [],
    ffmpeg: { ffmpeg_path: "ffmpeg", ffprobe_path: "ffprobe", found: true, ffprobe_found: true, version: "6.1" },
    recommended_jobs: { cpu_jobs: 2, gpu_jobs: 0, total: 2, reason: "Two at once." },
    hints: [],
    in_container: true,
    detected_at: "2026-09-28T00:00:00Z",
    ...partial,
  };
}

/** What the server returns while its first detection runs. */
const placeholder = hardware({
  cpu: { model: "Unknown CPU", logical_cores: 8, physical_cores: null, cgroup_limit: null },
  encoders: [],
  ffmpeg: { ffmpeg_path: "ffmpeg", ffprobe_path: "ffprobe", found: false, ffprobe_found: false, version: null },
  hints: [{ level: "info", title: "Checking your hardware…", detail: "Testing encoders.", fix: null }],
});

describe("isDetecting", () => {
  it("recognises the server's stand-in", () => {
    expect(isDetecting(placeholder)).toBe(true);
    expect(isDetecting(undefined)).toBe(false);
  });

  it("does not mistake a real result (even without ffmpeg) for it", () => {
    expect(isDetecting(hardware({}))).toBe(false);
    expect(
      isDetecting(
        hardware({
          encoders: [],
          ffmpeg: { ffmpeg_path: "ffmpeg", ffprobe_path: "ffprobe", found: false, ffprobe_found: false, version: null },
          hints: [{ level: "error", title: "ffmpeg isn't installed", detail: "", fix: null }],
        }),
      ),
    ).toBe(false);
  });

  it("trusts the server's detecting flag over the look of the report", () => {
    // Newer servers say so outright; a stand-in without the hint still counts.
    expect(isDetecting({ ...placeholder, hints: [], detecting: true })).toBe(true);
    // And a finished detection that happens to find nothing is real.
    expect(isDetecting({ ...placeholder, detecting: false })).toBe(false);
    expect(codecSpeedHint({ ...placeholder, detecting: false }, "hevc").tone).toBe("blocked");
  });

  it("never reports the stand-in as missing ffmpeg", () => {
    expect(codecSpeedHint(placeholder, "hevc").text).toBe("Checking your hardware…");
  });
});

describe("encoder matrix on a CPU-only host", () => {
  // jellyfin-ffmpeg lists NVENC and VA-API encoders even without the devices.
  const cpuOnly = hardware({
    encoders: [
      enc("libx265", "hevc", "software", true),
      enc("hevc_nvenc", "hevc", "nvenc", false, "No NVIDIA GPU is visible inside the container."),
      enc("hevc_vaapi", "hevc", "vaapi", false, "No VA-API device was found."),
    ],
  });

  it("shows no columns for hardware that isn't there", () => {
    expect(matrixApis(cpuOnly)).toEqual([]);
  });

  it("calls absent hardware 'not set up', not a failure", () => {
    expect(encoderCell(cpuOnly, cpuOnly.encoders[1])).toBe("not_set_up");
    expect(encoderCell(cpuOnly, cpuOnly.encoders[2])).toBe("not_set_up");
    expect(encoderCell(cpuOnly, cpuOnly.encoders[0])).toBe("works");
    expect(encoderCell(cpuOnly, undefined)).toBe("unavailable");
  });

  it("offers only working hardware as a preference", () => {
    const values = preferenceChoices(cpuOnly, "auto", HW_PREFERENCE_API).map((c) => c.value);
    expect(values).toEqual(["auto", "cpu"]);
  });

  it("keeps a pinned preference visible even when it no longer works", () => {
    const choices = preferenceChoices(cpuOnly, "nvenc", HW_PREFERENCE_API);
    expect(choices.map((c) => c.value)).toContain("nvenc");
  });
});

describe("encoder matrix with a GPU present", () => {
  const nvidia = hardware({
    gpus: [{ vendor: "nvidia", name: "NVIDIA GeForce RTX 3060", driver: "nvidia", render_node: null }],
    encoders: [
      enc("libx265", "hevc", "software", true),
      enc("libsvtav1", "av1", "software", true),
      enc("hevc_nvenc", "hevc", "nvenc", true),
      enc("av1_nvenc", "av1", "nvenc", false, "OpenEncodeSessionEx failed: unsupported device"),
      enc("hevc_vaapi", "hevc", "vaapi", false, "No VA-API device was found."),
    ],
  });

  it("hides other vendors, and reads a format the working GPU can't do as unsupported", () => {
    expect(matrixApis(nvidia)).toEqual(["nvenc"]);
    expect(encoderCell(nvidia, nvidia.encoders[3])).toBe("unsupported");
    expect(encoderCell(nvidia, nvidia.encoders[4])).toBe("not_set_up");
  });

  it("offers the working GPU and recommends a goal it speeds up", () => {
    const choices = preferenceChoices(nvidia, "auto", HW_PREFERENCE_API);
    expect(choices).toContainEqual({ value: "nvenc", disabled: false });
    expect(choices.some((c) => c.value === "vaapi")).toBe(false);
    expect(recommendedGoal(nvidia)).toBe("balanced");
    expect(codecSpeedHint(nvidia, "hevc").tone).toBe("fast");
    expect(codecSpeedHint(nvidia, "av1").tone).toBe("slow");
  });

  it("lists a present device whose encoders all failed, disabled", () => {
    const broken = hardware({
      gpus: [{ vendor: "intel", name: "Intel UHD 630", driver: "i915", render_node: "/dev/dri/renderD128" }],
      encoders: [enc("libx265", "hevc", "software", true), enc("hevc_qsv", "hevc", "qsv", false, "Error creating a MFX session")],
    });
    expect(preferenceChoices(broken, "auto", HW_PREFERENCE_API)).toContainEqual({ value: "qsv", disabled: true });
    expect(encoderCell(broken, broken.encoders[1])).toBe("failed");
  });
});
