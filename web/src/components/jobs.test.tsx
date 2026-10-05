import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "@/lib/api";
import type { HardwareInfo, Job, JobAttempt, Settings, ValidationCheck, ValidationReport } from "@/lib/types";
import {
  JobCard,
  JobSheet,
  StopJobButton,
  checkValueText,
  checksLead,
  errorLinesFirst,
  historyNote,
  jobOverall,
  noSpaceFreed,
  savingsText,
} from "./jobs";

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
    freed_bytes: 6e9,
    output_name: null,
    error: null,
    problem: null,
    skip_reason: null,
    validation: null,
    command: null,
    log_tail: null,
    notes: [],
    force: false,
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

  it("goes by what the server says was freed, not by the two sizes", () => {
    // A hard-linked original: the new file is smaller, but its old data stays on disk through the other link.
    expect(savingsText(718_000, 402_000)).toEqual({ text: "Saved 316 KB (44%)", saved: true });
    expect(savingsText(718_000, 402_000, 0)).toBeNull();
    // A number is the saving; a server that didn't record it (null, or no field) leaves the sizes to say.
    expect(savingsText(718_000, 402_000, 316_000)).toEqual({ text: "Saved 316 KB (44%)", saved: true });
    expect(savingsText(10e9, 4e9, null)).toEqual({ text: "Saved 6 GB (60%)", saved: true });
    expect(savingsText(10e9, 4e9, undefined)).toEqual({ text: "Saved 6 GB (60%)", saved: true });
    // A result that grew is still said, and nothing is claimed saved.
    expect(savingsText(1e9, 1.5e9, 0)).toEqual({ text: "500 MB larger", saved: false });
    expect(savingsText(1e9, null, 0)).toBeNull();
  });

  it("knows when a new file freed no space", () => {
    expect(noSpaceFreed({ input_size: 718_000, output_size: 402_000, freed_bytes: 0 })).toBe(true);
    expect(noSpaceFreed({ input_size: 718_000, output_size: 402_000, freed_bytes: 316_000 })).toBe(false);
    expect(noSpaceFreed({ input_size: 718_000, output_size: 402_000, freed_bytes: null })).toBe(false);
    // A file that grew says so itself; it isn't "no space freed".
    expect(noSpaceFreed({ input_size: 1e9, output_size: 1.5e9, freed_bytes: 0 })).toBe(false);
    expect(noSpaceFreed({ input_size: 1e9, output_size: null, freed_bytes: 0 })).toBe(false);
  });

  it("claims no saving for a converted hard-linked file, in the list rows too", () => {
    const linked = job({ input_size: 718_000, output_size: 402_000, freed_bytes: 0 });
    expect(historyNote(linked)).toBe("Converted, no space freed");
    expect(historyNote(job({ input_size: 718_000, output_size: 402_000, freed_bytes: 316_000 }))).toBe("Saved 316 KB (44%)");
    // Jobs from before the server recorded it read the sizes, as before.
    expect(historyNote(job({ input_size: 718_000, output_size: 402_000, freed_bytes: null }))).toBe("Saved 316 KB (44%)");
    expect(historyNote(job({ input_size: 1e9, output_size: 1.5e9, freed_bytes: 0 }))).toBe("500 MB larger");
  });

  it("notes each finished job briefly, and a damaged original as such", () => {
    expect(historyNote(job({}))).toBe("Saved 6 GB (60%)");
    expect(
      historyNote(
        job({
          state: "failed",
          output_size: null,
          error: "The original file appears damaged or incomplete (it stops after 0.1 s). It was left unchanged.",
          problem: "unreadable_source",
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

describe("a running job's card", () => {
  afterEach(cleanup);

  const name = "Das außergewöhnlich lange Serienfinale einer Show (2024) - S01E04 - Extended Cut Bluray-1080p.mkv";

  function renderWithClient(ui: React.ReactElement) {
    vi.spyOn(api, "libraries").mockResolvedValue([]);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
  }

  it("names its buttons after the file, so several cards don't all say just Stop and Details", () => {
    renderWithClient(<JobCard job={job({ state: "running", stage: "transcoding", progress: 40, file_name: name })} onOpen={() => {}} />);
    expect(screen.getByRole("button", { name: `Stop ${name}` }).textContent).toContain("Stop");
    expect(screen.getByRole("button", { name: `Details for ${name}` }).textContent).toContain("Details");
    // The card itself is named by its title too.
    expect(screen.getByRole("article", { name }).tagName).toBe("ARTICLE");
  });

  it("can shrink below its name's width, so a long name never pushes the buttons off a phone", () => {
    const { container } = renderWithClient(
      <JobCard job={job({ state: "running", stage: "transcoding", progress: 40, file_name: name })} onOpen={() => {}} />,
    );
    // A grid or flex child defaults to `min-width: auto`; without `min-w-0` the card is as wide as the name.
    expect(container.querySelector("article")?.className).toMatch(/\bmin-w-0\b/);
  });

  it("keeps the visible word first in the name of a file's Remove from queue button", () => {
    renderWithClient(<StopJobButton job={job({ state: "queued", file_name: name })} named />);
    // Voice control says what's on the button; the file name follows.
    expect(screen.getByRole("button", { name: `Remove from queue: ${name}` }).textContent).toContain("Remove from queue");
  });

  // The Atlas CPU fallback: frames were encoded for minutes while the card said 0%.
  it("shows the frames and the time spent, with a moving bar, while how far it is isn't known", () => {
    renderWithClient(
      <JobCard
        job={job({
          state: "running",
          stage: "transcoding",
          progress: 0,
          progress_basis: "unknown",
          frames: 1017,
          elapsed_secs: 400,
          eta_secs: null,
          attempt: 3,
        })}
        onOpen={() => {}}
      />,
    );
    const bar = screen.getByRole("progressbar");
    // An indeterminate bar has no value, but says what it stands for.
    expect(bar.getAttribute("aria-valuenow")).toBeNull();
    expect(bar.getAttribute("aria-valuetext")).toBe("1,017 frames · 7 min elapsed, how far isn't known yet");
    const card = screen.getByRole("article");
    expect(card.textContent).toContain("1,017 frames · 7 min elapsed");
    expect(card.textContent).toContain("how far isn't known yet");
    // Never a percentage it doesn't have, nor a time left.
    expect(card.textContent).not.toMatch(/\d+%/);
    expect(card.textContent).not.toMatch(/left/);
  });

  it("labels a share estimated from frames as an estimate", () => {
    renderWithClient(
      <JobCard
        job={job({
          state: "running",
          stage: "transcoding",
          progress: 50,
          progress_basis: "frames",
          frames: 732,
          elapsed_secs: 200,
          eta_secs: 200,
        })}
        onOpen={() => {}}
      />,
    );
    const card = screen.getByRole("article");
    expect(card.textContent).toContain("About 44% · estimated from 732 frames · about 3 min left");
    expect(screen.getByRole("progressbar").getAttribute("aria-valuenow")).toBe("44");
  });

  it("reads as before when the server measured it (or doesn't say)", () => {
    renderWithClient(
      <JobCard job={job({ state: "running", stage: "transcoding", progress: 50, eta_secs: 200 })} onOpen={() => {}} />,
    );
    expect(screen.getByRole("article").textContent).toContain("44% · about 3 min left");
    expect(screen.getByRole("article").textContent).not.toContain("estimated");
  });

  it("says which check the last try failed while another way runs", () => {
    renderWithClient(
      <JobCard
        job={job({ state: "running", stage: "transcoding", progress: 0, attempt: 2, attempts: [gpuFailed] })}
        onOpen={() => {}}
      />,
    );
    expect(screen.getByRole("article").textContent).toContain(
      "Attempt 2: the last try made a file that failed a check (Plays start to finish), so Szalinski is trying another way.",
    );
  });
});

/** The AMD GPU's file stopped playing early (the Atlas report). */
const gpuFailed: JobAttempt = {
  attempt: 1,
  encoder: "hevc_vaapi",
  hw_api: "vaapi",
  device: "/dev/dri/renderD128",
  hw_decode: true,
  elapsed_secs: 128,
  result: "failed",
  error: "The new file doesn't play start to finish. Playback stopped at 1:01 of 2:21:02",
  problem: "verification",
  failed_check: { id: "decode", label: "Plays start to finish", status: "fail", detail: "Playback stopped at 1:01 of 2:21:02", value: null },
  command: "ffmpeg -hwaccel vaapi -i Kingsman.mkv -c:v hevc_vaapi out.mkv",
  log_tail: "[warning] Invalid timestamps",
};

const cpuWorked: JobAttempt = {
  attempt: 2,
  encoder: "libx265",
  hw_api: "software",
  device: null,
  hw_decode: false,
  elapsed_secs: 401,
  result: "succeeded",
  error: null,
  problem: null,
  failed_check: null,
  command: "ffmpeg -i Kingsman.mkv -c:v libx265 out.mkv",
  log_tail: null,
};

describe("a job's sheet", () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  function showSheet(shown: Job) {
    vi.spyOn(api, "job").mockResolvedValue(shown);
    vi.spyOn(api, "libraries").mockResolvedValue([]);
    vi.spyOn(api, "settings").mockResolvedValue({ output_mode: "replace" } as Settings);
    vi.spyOn(api, "file").mockRejectedValue(new Error("not needed"));
    vi.spyOn(api, "hardware").mockResolvedValue({
      detecting: false,
      gpus: [{ vendor: "amd", name: "AMD Renoir", render_node: "/dev/dri/renderD128", driver: "amdgpu" }],
    } as unknown as HardwareInfo);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    return render(
      <QueryClientProvider client={client}>
        <JobSheet jobId={shown.id} onClose={() => {}} />
      </QueryClientProvider>,
    );
  }

  it("lists every attempt in plain words, with the check that failed", async () => {
    showSheet(
      job({
        encoder: "libx265",
        hw_api: "software",
        attempt: 2,
        command: cpuWorked.command ?? null,
        attempts: [gpuFailed, cpuWorked],
        notes: ["Converting on the GPU (VA-API) made a file that failed a check (Plays start to finish: playback stopped at 1:01 of 2:21:02), so it was converted on the CPU"],
      }),
    );
    const dialog = await screen.findByRole("dialog");
    const list = await within(dialog).findByRole("list", { name: "Attempts" });
    await waitFor(() => expect(within(list).getByText("Attempt 1 · AMD GPU (VA-API) · decoded on the GPU · 2 min")).toBeTruthy());
    const rows = within(list).getAllByRole("listitem").map((li) => li.textContent);
    expect(rows).toEqual([
      "Attempt 1 · AMD GPU (VA-API) · decoded on the GPU · 2 minFailed: Plays start to finish: playback stopped at 1:01 of 2:21:02",
      "Attempt 2 · CPU · 7 minWorked",
    ]);
    // The technical details keep each attempt's command and log, and copy them all for a bug report.
    expect(within(dialog).getByText("Attempt 1: ffmpeg command")).toBeTruthy();
    expect(within(dialog).getByText("Attempt 1: last lines from ffmpeg")).toBeTruthy();
    expect(within(dialog).getByText(gpuFailed.command ?? "")).toBeTruthy();
    expect(within(dialog).getByRole("button", { name: /Copy for a bug report/ })).toBeTruthy();
  });

  it("shows the attempt that failed and the one running now, with its frames while how far isn't known", async () => {
    showSheet(
      job({
        state: "running",
        stage: "transcoding",
        progress: 0,
        progress_basis: "unknown",
        frames: 366,
        elapsed_secs: 99,
        encoder: "libx265",
        hw_api: "software",
        attempt: 2,
        output_size: null,
        freed_bytes: null,
        attempts: [gpuFailed],
      }),
    );
    const dialog = await screen.findByRole("dialog");
    const list = await within(dialog).findByRole("list", { name: "Attempts" });
    await waitFor(() => expect(within(list).getByText("Attempt 2 · CPU")).toBeTruthy());
    expect(within(list).getByText("Converting now")).toBeTruthy();
    expect(within(list).getByText("Failed: Plays start to finish: playback stopped at 1:01 of 2:21:02")).toBeTruthy();
    expect(dialog.textContent).toContain("366 frames · 2 min elapsed · how far isn't known yet");
    expect(dialog.textContent).not.toMatch(/\b0%/);
    expect(within(dialog).getByRole("progressbar", { name: "Whole file" }).getAttribute("aria-valuenow")).toBeNull();
  });

  it("has no attempts section for a job that worked the first time", async () => {
    showSheet(job({ encoder: "libx265", hw_api: "software", attempts: [{ ...cpuWorked, attempt: 1 }] }));
    const dialog = await screen.findByRole("dialog");
    await waitFor(() => expect(within(dialog).getByText("Details")).toBeTruthy());
    expect(within(dialog).queryByText("Attempts")).toBeNull();
  });
});
