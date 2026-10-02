import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { FileSheet } from "@/components/files";
import { ConvertAnywayButton, JobSheet } from "@/components/jobs";
import { ConfirmDialog } from "@/components/ui/overlays";
import { api } from "@/lib/api";
import { FACTORY_DEFAULT_GOAL, profileForGoal } from "@/lib/profile";
import { parseRoute } from "@/lib/router";
import type { FileDetail, HardwareInfo, Job, Library, MediaFile, QueueState, Settings, TranscodeProfile } from "@/lib/types";
import { GoalStep } from "@/screens/library-steps";
import { FilesTab } from "@/screens/library";
import { RecentResults } from "@/screens/overview";
import { QueueScreen } from "@/screens/queue";

/** What a person sees on the screens a conversion's outcome is told on: rows, sheets and dialogs. */

const LIB = "33333333-3333-3333-3333-333333333333";

function job(partial: Partial<Job> = {}): Job {
  return {
    id: "11111111-1111-1111-1111-111111111111",
    file_id: "22222222-2222-2222-2222-222222222222",
    library_id: LIB,
    file_name: "land1080.mp4",
    file_path: "/media/Clips/land1080.mp4",
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
    input_size: 718_000,
    output_size: 402_000,
    freed_bytes: 316_000,
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
    started_at: "2026-09-28T05:11:00Z",
    finished_at: "2026-09-28T05:12:00Z",
    ...partial,
  };
}

function file(partial: Partial<MediaFile> = {}): MediaFile {
  return {
    id: "22222222-2222-2222-2222-222222222222",
    library_id: LIB,
    path: "/media/Clips/land1080.mkv",
    relative_path: "Clips/land1080.mkv",
    file_name: "land1080.mkv",
    size_bytes: 402_000,
    modified_at: "2026-09-28T05:10:00Z",
    status: "done",
    container: "matroska",
    video_codec: "hevc",
    audio_codec: "aac",
    resolution: "1080p",
    hdr: null,
    duration_secs: 12,
    bit_rate: null,
    original_size_bytes: 718_000,
    saved_bytes: 316_000,
    skip_reason: null,
    error: null,
    problem: null,
    job_id: "11111111-1111-1111-1111-111111111111",
    progress: null,
    scanned_at: "2026-09-28T05:10:00Z",
    updated_at: "2026-09-28T05:12:00Z",
    ...partial,
  };
}

const library = {
  id: LIB,
  name: "Demo Movies",
  path: "/media",
  enabled: true,
  profile: { goal: "balanced", min_savings_pct: 10 },
  stats: { failed: 0, settling: 0 },
} as unknown as Library;

const queueState: QueueState = {
  paused: false,
  running: 0,
  queued: 0,
  max_jobs: 2,
  max_jobs_auto: true,
  max_jobs_source: "auto",
  waiting_for_schedule: false,
};

/** Answer every call the screens make; `history` is the finished jobs, `detail` the file sheet's. */
function mockApi({
  history = [],
  mode = "replace",
  detail,
  single,
}: {
  history?: Job[];
  mode?: Settings["output_mode"];
  detail?: FileDetail;
  single?: Job;
} = {}) {
  vi.spyOn(api, "settings").mockResolvedValue({ output_mode: mode, auto_queue: true } as Settings);
  vi.spyOn(api, "libraries").mockResolvedValue([library]);
  vi.spyOn(api, "queue").mockResolvedValue(queueState);
  vi.spyOn(api, "activity").mockResolvedValue({ items: [] });
  vi.spyOn(api, "files").mockResolvedValue({ items: [], total: 0 });
  vi.spyOn(api, "jobs").mockImplementation(async (query) =>
    query.state === "history" ? { items: history, total: history.length } : { items: [], total: 0 },
  );
  if (single) vi.spyOn(api, "job").mockResolvedValue(single);
  if (detail) vi.spyOn(api, "file").mockResolvedValue(detail);
}

function renderWithClient(ui: React.ReactElement) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

afterEach(cleanup);

describe("a conversion that renamed its file", () => {
  const renamed = job({ output_name: "land1080.mkv" });

  it("is listed in the queue's history by its new name, with the old one secondary", async () => {
    mockApi({ history: [renamed] });
    renderWithClient(<QueueScreen route={parseRoute("#/queue/history")} />);
    await screen.findByText("land1080.mkv");
    expect(screen.getByText("Was land1080.mp4")).toBeTruthy();
    // The old name is not the row's name any more.
    expect(screen.queryByText("land1080.mp4")).toBeNull();
  });

  it("is listed under Recently finished the same way", async () => {
    mockApi({ history: [renamed] });
    renderWithClient(<RecentResults />);
    await screen.findByText("land1080.mkv");
    expect(screen.getByText("Was land1080.mp4")).toBeTruthy();
  });

  it("keeps the name it had when the name didn't change", async () => {
    mockApi({ history: [job({ file_name: "Movie.mkv", output_name: null })] });
    renderWithClient(<RecentResults />);
    await screen.findByText("Movie.mkv");
    expect(screen.queryByText(/^Was /)).toBeNull();
  });

  it("keeps the old name when the result went to a separate folder, where the original is still called that", async () => {
    mockApi({ history: [renamed], mode: "folder" });
    renderWithClient(<RecentResults />);
    await screen.findByText("land1080.mp4");
    expect(screen.queryByText("Was land1080.mp4")).toBeNull();
    expect(screen.queryByText("land1080.mkv")).toBeNull();
  });

  it("titles its job sheet with the new name and says what it was", async () => {
    mockApi({ single: renamed, detail: { file: file(), jobs: [renamed] } });
    renderWithClient(<JobSheet jobId={renamed.id} onClose={() => {}} />);
    const dialog = await screen.findByRole("dialog");
    await waitFor(() => expect(within(dialog).getByRole("heading", { name: "land1080.mkv" })).toBeTruthy());
    expect(dialog.textContent).toMatch(/was land1080\.mp4/);
    // The details say where it was and where it is now.
    expect(dialog.textContent).toContain("/media/Clips/land1080.mp4");
    expect(dialog.textContent).toContain("/media/Clips/land1080.mkv");
  });

  it("shows the old name beside each conversion in the file's history", async () => {
    mockApi({ detail: { file: file(), jobs: [renamed] } });
    renderWithClient(<FileSheet fileId={file().id} onClose={() => {}} />);
    const dialog = await screen.findByRole("dialog");
    await waitFor(() => expect(within(dialog).getByText("Was land1080.mp4")).toBeTruthy());
  });
});

describe("searching a library for a name a conversion replaced", () => {
  it("sends the text as typed, and says why the renamed file is listed", async () => {
    mockApi();
    const files = vi.spyOn(api, "files").mockResolvedValue({ items: [file()], total: 1 });
    renderWithClient(<FilesTab library={library} route={parseRoute("#/library/x?q=land1080.mp4")} />);
    await waitFor(() => expect(screen.getAllByText("land1080.mkv").length).toBeGreaterThan(0));
    expect(files).toHaveBeenCalledWith(expect.objectContaining({ q: "land1080.mp4", library: LIB }), expect.anything());
    // A row whose own name doesn't hold the text would look like a wrong match without a word.
    expect(screen.getAllByText("Found by its name before it was converted").length).toBeGreaterThan(0);
  });

  it("says nothing extra for an ordinary match", async () => {
    mockApi();
    vi.spyOn(api, "files").mockResolvedValue({ items: [file()], total: 1 });
    renderWithClient(<FilesTab library={library} route={parseRoute("#/library/x?q=land1080.mkv")} />);
    await waitFor(() => expect(screen.getAllByText("land1080.mkv").length).toBeGreaterThan(0));
    expect(screen.queryByText("Found by its name before it was converted")).toBeNull();
  });
});

describe("a converted hard-linked file", () => {
  const linked = job({ freed_bytes: 0, force: true, notes: ["The original has another hard link, so replacing it freed no space"] });

  it("claims no saving anywhere the job is told, even though the new file is smaller", async () => {
    mockApi({ history: [linked], single: linked, detail: { file: file({ saved_bytes: 0 }), jobs: [linked] } });

    renderWithClient(<RecentResults />);
    await screen.findByText("Converted, no space freed");
    expect(document.body.textContent).not.toMatch(/Saved 316 KB/);
    cleanup();

    renderWithClient(<JobSheet jobId={linked.id} onClose={() => {}} />);
    const sheet = await screen.findByRole("dialog");
    await waitFor(() => expect(within(sheet).getByText("No space was freed.")).toBeTruthy());
    // The two sizes are still told; only the saving isn't claimed.
    expect(sheet.textContent).toContain("718 KB");
    expect(sheet.textContent).toContain("402 KB");
    expect(sheet.textContent).not.toMatch(/Saved 316 KB/);
    cleanup();

    renderWithClient(<FileSheet fileId={file().id} onClose={() => {}} />);
    const fileSheet = await screen.findByRole("dialog");
    await waitFor(() => expect(within(fileSheet).getByText("No space freed")).toBeTruthy());
    expect(fileSheet.textContent).not.toMatch(/Saved 316 KB/);
  });

  it("still says what was saved when the server says so, or says nothing (an older job)", async () => {
    const freed = job({ freed_bytes: 316_000 });
    const older = job({ id: "44444444-4444-4444-4444-444444444444", freed_bytes: null, finished_at: "2026-09-28T05:11:00Z" });
    mockApi({ history: [freed, older] });
    renderWithClient(<RecentResults />);
    await waitFor(() => expect(screen.getAllByText("Saved 316 KB (44%)")).toHaveLength(2));
  });
});

describe("Convert anyway", () => {
  const lossReason =
    "MP4 can't hold this file's 2 picture-based subtitles and 1 subtitle font, so it was left unchanged. To convert it, choose an MKV goal or save converted files to a separate folder; Convert anyway converts it without them";

  it("says plainly what a skipped file will lose, and queues it forced only after the user agrees", async () => {
    const queued = vi.spyOn(api, "queueFile").mockResolvedValue(job({ state: "queued", force: true }));
    renderWithClient(
      <ConvertAnywayButton file={file({ status: "skipped", file_name: "Demo Anime - S01E01.mkv", skip_reason: lossReason })} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Convert anyway" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(within(dialog).getByRole("heading", { name: "Convert Demo Anime - S01E01.mkv anyway?" })).toBeTruthy();
    expect(dialog.textContent).toContain("MP4 can't hold this file's 2 picture-based subtitles and 1 subtitle font.");
    expect(dialog.textContent).toContain("leaves them out of the new file");
    expect(dialog.textContent).toContain("gone for good");
    expect(dialog.textContent).toContain("save converted files to a separate folder");
    // Nothing is queued until it's confirmed.
    expect(queued).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "Convert anyway" }));
    await waitFor(() => expect(queued).toHaveBeenCalledWith(file().id, { force: true }));
  });

  it("keeps its short explanation for the library's own rules", async () => {
    renderWithClient(<ConvertAnywayButton file={file({ status: "skipped", skip_reason: "Already HEVC" })} />);
    fireEvent.click(screen.getByRole("button", { name: "Convert anyway" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog.textContent).toContain("ignoring this library's rules for skipping files");
    expect(dialog.textContent).not.toContain("gone for good");
  });
});

describe("the goal step of Add library", () => {
  /** A machine with no GPU encoder: the hardware suggests Balanced ("Best fit"). */
  const cpuOnly = { detecting: false, encoders: [], gpus: [], ffmpeg: { found: true }, hints: [] } as unknown as HardwareInfo;

  function renderGoalStep(defaults: TranscodeProfile) {
    vi.spyOn(api, "settings").mockResolvedValue({ default_profile: defaults, auto_queue: true, hardware: "auto" } as Settings);
    vi.spyOn(api, "presets").mockResolvedValue({ goals: [], video_codecs: [], audio_codecs: [], containers: [] });
    vi.spyOn(api, "hardware").mockResolvedValue(cpuOnly);
    renderWithClient(
      <GoalStep
        path="/media/Movies"
        submitLabel="Add library"
        onChangeFolder={() => {}}
        onCreated={() => {}}
        onFolderError={() => {}}
      />,
    );
  }

  /** The goal card that is selected. */
  const chosen = () => screen.getAllByRole("radio").find((r) => (r as HTMLInputElement).checked)?.getAttribute("aria-labelledby");
  const chosenTitle = () => {
    const id = chosen();
    return id ? document.getElementById(id)?.textContent : undefined;
  };

  it("starts with the goal the hardware suggests on a fresh install", async () => {
    renderGoalStep(profileForGoal(FACTORY_DEFAULT_GOAL));
    await screen.findByText("Best fit");
    expect(chosenTitle()).toBe("Balanced");
    expect(screen.queryByText("Your defaults")).toBeNull();
  });

  it("starts with the goal saved as the default, not the hardware's", async () => {
    // After saving Plays everywhere in Settings, Add library used to preselect Balanced / Best fit.
    renderGoalStep(profileForGoal("compatible"));
    await waitFor(() => expect(chosenTitle()).toBe("Plays everywhere"));
    // The hardware's suggestion is still marked, on its own card.
    expect(screen.getByText("Best fit")).toBeTruthy();
    expect(screen.queryByText("Your defaults")).toBeNull();
  });

  it("offers defaults that are no goal's preset as a card of their own, chosen from the start", async () => {
    renderGoalStep({ ...profileForGoal("compatible"), max_height: 1080 });
    await waitFor(() => expect(chosenTitle()).toBe("Your defaults"));
  });
});

describe("a confirmation's title", () => {
  it("can break anywhere, so a long dotted release name never runs past the dialog", async () => {
    const name = "Some.Really.Long.Scene.Release.Name.2021.1080p.BluRay.x264.DTS-HD.MA.5.1.REMUX-GROUP.mkv";
    render(
      <ConfirmDialog open onOpenChange={() => {}} title={`Convert ${name} anyway?`} confirmLabel="Convert anyway" onConfirm={() => {}}>
        <p>Text.</p>
      </ConfirmDialog>,
    );
    const title = await screen.findByRole("heading", { name: `Convert ${name} anyway?` });
    // Layout isn't measured in jsdom; the rule that lets the name wrap is.
    expect(title.className).toContain("[overflow-wrap:anywhere]");
  });
});
