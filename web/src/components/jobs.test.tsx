import { describe, expect, it } from "vitest";
import type { Job, ValidationCheck, ValidationReport } from "@/lib/types";
import { checkValueText, checksLead, errorLinesFirst, historyNote, jobOverall, savingsText } from "./jobs";

const check = (id: string, value: number | null, status: ValidationCheck["status"] = "pass"): ValidationCheck => ({
  id,
  label: id === "visual" ? "Looks the same as the original" : id,
  status,
  detail: "",
  value,
});

function report(partial: Partial<ValidationReport>): ValidationReport {
  return {
    passed: true,
    level: "standard",
    checks: [],
    ssim_min: null,
    ssim_avg: null,
    psnr_avg: null,
    elapsed_secs: 30,
    ...partial,
  };
}

function job(partial: Partial<Job>): Job {
  return {
    id: "11111111-1111-1111-1111-111111111111",
    file_id: "22222222-2222-2222-2222-222222222222",
    library_id: "33333333-3333-3333-3333-333333333333",
    file_name: "Movie.mkv",
    file_path: "/media/Movie.mkv",
    state: "done",
    stage: "finalizing",
    priority: 0,
    progress: 100,
    fps: null,
    speed: null,
    eta_secs: null,
    encoder: null,
    hw_api: null,
    attempt: 1,
    input_size: 10e9,
    output_size: 4e9,
    error: null,
    skip_reason: null,
    validation: null,
    command: null,
    log_tail: null,
    created_at: "2026-09-28T05:10:00Z",
    started_at: null,
    finished_at: null,
    ...partial,
  };
}

describe("checkValueText", () => {
  it("gives each measured value its unit", () => {
    expect(checkValueText(check("duration", 1.25))).toBe("1.3 s off");
    expect(checkValueText(check("black_frames", 2.4))).toBe("+2.4 s");
    expect(checkValueText(check("frozen_frames", 30))).toBe("+30 s");
  });

  it("leaves out numbers the sentence already covers or that mean nothing alone", () => {
    expect(checkValueText(check("duration", 0.01))).toBeNull();
    expect(checkValueText(check("black_frames", 0))).toBeNull();
    expect(checkValueText(check("size", 43.2))).toBeNull();
    expect(checkValueText(check("decode", 7200))).toBeNull();
    // Similarity is said once, in the lead sentence; the score itself is technical.
    expect(checkValueText(check("visual", 0.6123))).toBeNull();
  });
});

describe("checksLead", () => {
  it("says the verdict in one plain sentence, similarity as a percentage", () => {
    expect(checksLead(report({ ssim_min: 0.9961, checks: [check("visual", 0.9961)] }))).toBe(
      "Looks the same as the original: 99.6% similar at its lowest point.",
    );
    expect(checksLead(report({ passed: false, ssim_min: 0.6123, checks: [check("visual", 0.6123, "fail")] }))).toBe(
      "Looked different from the original: only 61.2% similar at its lowest point.",
    );
    expect(checksLead(report({ checks: [check("probe", null)] }))).toBe("Passed every check.");
    expect(
      checksLead(report({ passed: false, checks: [{ ...check("duration", 3, "fail"), label: "Same length" }] })),
    ).toBe("Failed a check: same length.");
  });
});

describe("errorLinesFirst", () => {
  it("pulls out the lines that name the problem", () => {
    const log = "Svt[info]: banner\n[matroska,webm @ 0x55] [error] File ended prematurely\nConversion failed!";
    expect(errorLinesFirst(log)).toEqual({
      errors: "[matroska,webm @ 0x55] [error] File ended prematurely\nConversion failed!",
      rest: log,
    });
    expect(errorLinesFirst("frame=10 fps=5").errors).toBeNull();
  });
});

describe("job summaries", () => {
  it("reports whole-file progress, not the stage's", () => {
    expect(jobOverall({ stage: "verifying", progress: 0 })).toBe(85);
    expect(jobOverall({ stage: "transcoding", progress: 100 })).toBe(85);
  });

  it("words savings and growth", () => {
    expect(savingsText(10e9, 4e9)).toEqual({ text: "Saved 6 GB (60%)", saved: true });
    expect(savingsText(1e9, 1.5e9)).toEqual({ text: "500 MB larger", saved: false });
    expect(savingsText(1e9, null)).toBeNull();
  });

  it("notes each finished job briefly, and a damaged original as such", () => {
    expect(historyNote(job({}))).toBe("Saved 6 GB (60%)");
    expect(
      historyNote(
        job({
          state: "failed",
          output_size: null,
          error: "The original file appears damaged or incomplete (it stops after 0.1 s). It was left unchanged.",
        }),
      ),
    ).toBe("Looks damaged or isn't a video");
    expect(historyNote(job({ state: "failed", output_size: null, error: "ffmpeg ran out of memory" }))).toBe(
      "ffmpeg ran out of memory",
    );
    expect(historyNote(job({ state: "cancelled", output_size: null }))).toMatch(/^Stopped/);
  });

  it("says why a kept original was kept, naming the library's minimum", () => {
    const kept = job({ state: "skipped", output_size: 9.4e9, skip_reason: "Only 6% smaller — kept the original" });
    expect(historyNote(kept, 10)).toBe("6% smaller (needs at least 10%)");
    expect(historyNote(kept)).toBe("6% smaller, not enough for this library");
    expect(historyNote(job({ state: "skipped", output_size: null, skip_reason: "Already HEVC" }))).toBe("Already HEVC");
    expect(historyNote(job({ state: "skipped", output_size: null, skip_reason: null }))).toBe("No conversion needed");
  });

  it("names a failure by its code: the setup cause, else the server's sentence", () => {
    const failed = (problem: Job["problem"], error = "Not enough free space in /temp for the new file (needs about 4 GB)") =>
      job({ state: "failed", output_size: null, error, problem });
    expect(historyNote(failed("disk_full"))).toBe("The disk is full");
    expect(historyNote(failed("work_folder"))).toBe("The work folder can't be used");
    // A moved or replaced file: the server's sentence says which.
    expect(historyNote(failed("source_changed", "The file is no longer there. It may have been moved or deleted."))).toBe(
      "The file is no longer there. It may have been moved or deleted.",
    );
    expect(historyNote(failed("source_changed", ""))).toBe("Moved or changed during the conversion");
    expect(historyNote(failed("verification", "Looked different."))).toBe("Looked different.");
    // The code wins over a damaged-sounding sentence.
    expect(historyNote(failed("encoder", "The original file appears damaged or incomplete."))).toBe(
      "The original file appears damaged or incomplete.",
    );
  });

  it("puts the reason first for a second conversion that kept the converted file", () => {
    const skipped = job({ state: "skipped", output_size: 9.4e9, skip_reason: "Only 6% smaller — kept the original" });
    // The badge says "Kept as converted"; the note says why.
    expect(historyNote(skipped, 10, "kept")).toBe("6% smaller (needs at least 10%)");
    expect(historyNote(job({ state: "skipped", output_size: 9.4e9, skip_reason: null }), 10, "kept")).toBe(
      "Not worth converting again",
    );
    // A setup problem: the badge says "Needs a fix", so the note says the file is fine.
    expect(historyNote(job({ state: "failed", output_size: null, error: "x", problem: "work_folder" }), null, "kept")).toBe(
      "The work folder can't be used · converted file kept",
    );
    expect(historyNote(job({ state: "failed", output_size: null, error: "NVENC stopped.", problem: "encoder" }), null, "kept")).toBe(
      "NVENC stopped.",
    );
    expect(historyNote(job({ state: "cancelled", output_size: null }), null, "kept")).toBe(
      "Stopped. The converted file is unchanged.",
    );
  });

  it("says when a file has moved past an old outcome", () => {
    const failed = job({ state: "failed", output_size: null, error: "x", problem: "work_folder" });
    expect(historyNote(failed, null, "converted")).toBe("The work folder can't be used · now converted");
    expect(historyNote(failed, null, "queued")).toBe("The work folder can't be used · queued again");
    expect(historyNote(job({ state: "cancelled", output_size: null }), null, "converted")).toBe("Stopped · now converted");
  });
});
