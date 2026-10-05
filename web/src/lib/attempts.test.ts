import { describe, expect, it } from "vitest";
import {
  attemptDevice,
  attemptHeading,
  attemptOutcome,
  attemptRows,
  checkReason,
  decodingText,
  jobReportText,
  lastFailedCheck,
} from "./attempts";
import type { GpuDevice, Job, JobAttempt } from "./types";

const amd: GpuDevice = { vendor: "amd", name: "AMD Renoir", render_node: "/dev/dri/renderD128", driver: "amdgpu" };

/** The Atlas report: the AMD GPU's file stopped playing early, then the CPU. */
const gpuFailed: JobAttempt = {
  attempt: 1,
  encoder: "hevc_vaapi",
  hw_api: "vaapi",
  device: "/dev/dri/renderD128",
  hw_decode: true,
  elapsed_secs: 128.4,
  result: "failed",
  error: "The new file doesn't play start to finish. Playback stopped at 1:01 of 2:21:02",
  problem: "verification",
  failed_check: {
    id: "decode",
    label: "Plays start to finish",
    status: "fail",
    detail: "Playback stopped at 1:01 of 2:21:02.",
    value: null,
  },
  command: "ffmpeg -init_hw_device vaapi=va:/dev/dri/renderD128 -hwaccel vaapi -i in.mkv out.mkv",
  log_tail: "[warning] Invalid timestamps",
};

const encodeFailed: JobAttempt = {
  ...gpuFailed,
  attempt: 2,
  hw_decode: false,
  elapsed_secs: 12,
  failed_check: null,
  problem: "encoder",
  error: "Converting on the GPU (VA-API) stopped with an error, so the original was left unchanged.",
  log_tail: "[error] Failed to upload frame: Input/output error",
};

const cpuWorked: JobAttempt = {
  attempt: 3,
  encoder: "libx265",
  hw_api: "software",
  device: null,
  hw_decode: false,
  elapsed_secs: 401,
  result: "succeeded",
  error: null,
  problem: null,
  failed_check: null,
  command: "ffmpeg -i in.mkv -c:v libx265 out.mkv",
  log_tail: null,
};

function job(partial: Partial<Job> = {}): Job {
  return {
    id: "11111111-1111-1111-1111-111111111111",
    file_id: "22222222-2222-2222-2222-222222222222",
    library_id: "33333333-3333-3333-3333-333333333333",
    file_name: "Kingsman.mkv",
    file_path: "/media/Kingsman.mkv",
    state: "done",
    stage: "finalizing",
    priority: 0,
    progress: 100,
    fps: null,
    speed: null,
    eta_secs: null,
    encoder: "libx265",
    hw_api: "software",
    attempt: 3,
    input_size: 487e6,
    output_size: 181e6,
    freed_bytes: 306e6,
    output_name: null,
    error: null,
    problem: null,
    skip_reason: null,
    validation: null,
    command: cpuWorked.command ?? null,
    log_tail: null,
    notes: [],
    force: false,
    created_at: "2026-10-04T05:10:00Z",
    started_at: "2026-10-04T05:11:00Z",
    finished_at: "2026-10-04T05:20:00Z",
    attempts: [gpuFailed, encodeFailed, cpuWorked],
    ...partial,
  };
}

describe("an attempt in plain words", () => {
  it("names where it ran, the GPU's maker for VA-API when its render node is known", () => {
    expect(attemptDevice(gpuFailed, [amd])).toBe("AMD GPU (VA-API)");
    expect(attemptDevice(gpuFailed, [{ ...amd, vendor: "intel" }])).toBe("Intel GPU (VA-API)");
    // Not found (or not loaded yet): the API's own name.
    expect(attemptDevice(gpuFailed, [])).toBe("GPU (VA-API)");
    expect(attemptDevice({ hw_api: "nvenc", device: null })).toBe("NVIDIA GPU");
    expect(attemptDevice(cpuWorked)).toBe("CPU");
  });

  it("says how the original was decoded, except on the CPU", () => {
    expect(decodingText(gpuFailed)).toBe("decoded on the GPU");
    expect(decodingText(encodeFailed)).toBe("decoded on the CPU");
    expect(decodingText(cpuWorked)).toBeNull();
  });

  it("heads each attempt with its number, place, decoding and time", () => {
    expect(attemptHeading(gpuFailed, [amd])).toBe("Attempt 1 · AMD GPU (VA-API) · decoded on the GPU · 2 min");
    expect(attemptHeading(cpuWorked)).toBe("Attempt 3 · CPU · 7 min");
  });

  it("says why it failed: the check and its reason, else the error", () => {
    expect(attemptOutcome(gpuFailed)).toBe("Failed: Plays start to finish: playback stopped at 1:01 of 2:21:02");
    expect(attemptOutcome(encodeFailed)).toBe(
      "Failed: converting on the GPU (VA-API) stopped with an error, so the original was left unchanged.",
    );
    expect(attemptOutcome({ result: "failed", failed_check: null, error: null })).toBe("Failed");
    expect(attemptOutcome(cpuWorked)).toBe("Worked");
    expect(checkReason({ label: "Same length as the original", detail: "" })).toBe("Same length as the original");
  });
});

describe("attemptRows", () => {
  it("lists them when there was more than one", () => {
    expect(attemptRows(job()).map((r) => r.attempt?.attempt)).toEqual([1, 2, 3]);
  });

  it("leaves a single attempt to the job's own outcome", () => {
    expect(attemptRows(job({ attempt: 1, attempts: [cpuWorked] }))).toEqual([]);
    expect(attemptRows(job({ attempts: [] }))).toEqual([]);
    // A server older than the history sends none.
    expect(attemptRows(job({ attempts: undefined }))).toEqual([]);
  });

  it("adds the attempt still running after one that failed", () => {
    const running = job({ state: "running", stage: "transcoding", attempt: 2, attempts: [gpuFailed] });
    const rows = attemptRows(running, [amd]);
    expect(rows).toHaveLength(2);
    expect(rows[1]).toEqual({ attempt: null, running: { attempt: 2, device: "CPU" } });
    expect(lastFailedCheck(running)?.label).toBe("Plays start to finish");
    expect(lastFailedCheck(job())).toBeNull();
  });
});

describe("jobReportText", () => {
  it("has every attempt with its command, why it failed and what ffmpeg said", () => {
    const text = jobReportText(job({ notes: ["Converting on the GPU (VA-API) made a file that failed a check"] }), [amd]);
    expect(text).toContain("File: /media/Kingsman.mkv");
    expect(text).toContain("Attempt 1 · AMD GPU (VA-API) · decoded on the GPU · 2 min (hevc_vaapi, VA-API)");
    expect(text).toContain("Failed: Plays start to finish: playback stopped at 1:01 of 2:21:02");
    expect(text).toContain(`Command: ${gpuFailed.command}`);
    expect(text).toContain("[warning] Invalid timestamps");
    expect(text).toContain("[error] Failed to upload frame: Input/output error");
    expect(text).toContain("Attempt 3 · CPU · 7 min (libx265, Software)\nWorked");
    expect(text).toContain("Note: Converting on the GPU (VA-API) made a file that failed a check");
    // The attempts come in order.
    expect(text.indexOf("Attempt 1")).toBeLessThan(text.indexOf("Attempt 2"));
    expect(text.indexOf("Attempt 2")).toBeLessThan(text.indexOf("Attempt 3"));
  });

  it("says when a running job's share isn't known", () => {
    const text = jobReportText(
      job({ state: "running", stage: "transcoding", progress: 0, progress_basis: "unknown", frames: 366, elapsed_secs: 99, attempts: [gpuFailed] }),
    );
    expect(text).toContain("Progress: not known, 366 frames, 99 s");
  });
});
