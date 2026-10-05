import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { FileSheet } from "@/components/files";
import { api } from "@/lib/api";
import type { FileDetail, Library, QueueState, Settings, StreamInfo } from "@/lib/types";

/** The tracks in the file sheet: what a track is stays in sight whatever its title says. */

const LIB = "33333333-3333-3333-3333-333333333333";
const FILE = "22222222-2222-2222-2222-222222222222";
const RELEASE =
  "Some.Really.Long.Scene.Release.Name.2017.2160p.UHD.BluRay.x265.10bit.HDR.TrueHD.7.1.Atmos-GROUP";

function stream(partial: Partial<StreamInfo>): StreamInfo {
  return {
    index: 0,
    kind: "audio",
    codec: "truehd",
    profile: null,
    language: "eng",
    title: null,
    is_default: false,
    is_forced: false,
    is_attached_pic: false,
    bit_rate: null,
    width: null,
    height: null,
    pix_fmt: null,
    bit_depth: null,
    frame_rate: null,
    color_primaries: null,
    color_transfer: null,
    color_space: null,
    color_range: null,
    hdr: null,
    interlaced: false,
    channels: 8,
    channel_layout: "7.1",
    sample_rate: 48000,
    ...partial,
  };
}

function detail(streams: StreamInfo[]): FileDetail {
  return {
    file: {
      id: FILE,
      library_id: LIB,
      path: "/media/Clip.mkv",
      relative_path: "Clip.mkv",
      file_name: "Clip.mkv",
      size_bytes: 1_000_000,
      modified_at: "2026-09-28T05:10:00Z",
      status: "pending",
      container: "matroska",
      video_codec: "hevc",
      audio_codec: "truehd",
      resolution: "4K",
      hdr: null,
      duration_secs: 60,
      bit_rate: null,
      original_size_bytes: null,
      saved_bytes: null,
      skip_reason: null,
      error: null,
      problem: null,
      job_id: null,
      progress: null,
      probe: {
        container: "matroska",
        format_long_name: null,
        duration_secs: 60,
        bit_rate: null,
        size_bytes: 1_000_000,
        start_time: 0,
        chapters: 0,
        streams,
      },
      scanned_at: "2026-09-28T05:10:00Z",
      updated_at: "2026-09-28T05:10:00Z",
    },
    jobs: [],
  };
}

const library = {
  id: LIB,
  name: "Movies",
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

function show(streams: StreamInfo[]) {
  vi.spyOn(api, "settings").mockResolvedValue({ output_mode: "replace", auto_queue: true } as Settings);
  vi.spyOn(api, "libraries").mockResolvedValue([library]);
  vi.spyOn(api, "queue").mockResolvedValue(queueState);
  vi.spyOn(api, "file").mockResolvedValue(detail(streams));
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <FileSheet fileId={FILE} onClose={() => {}} />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("the track list", () => {
  it("shows the codec and channels in full while a long title is cut short on its own line", async () => {
    show([
      stream({ index: 1, kind: "video", codec: "hevc", language: null, width: 3840, height: 2160, frame_rate: 23.976, channels: null, channel_layout: null }),
      stream({ index: 2, title: RELEASE, is_default: true }),
      stream({ index: 3, kind: "subtitle", codec: "hdmv_pgs_subtitle", language: "fra", title: RELEASE, channels: null, channel_layout: null }),
    ]);
    // The codec and channels are plain text of their own, not part of the title.
    const audio = (await screen.findByText("TrueHD · 7.1")).closest("li");
    expect(audio).not.toBeNull();
    expect(audio?.textContent).toContain("English audio");
    expect(screen.getByText("HEVC · 3840×2160 · 23.98 frames a second")).toBeTruthy();
    expect(screen.getByText("PGS")).toBeTruthy();
    // The title is on its own line, truncated, with the whole thing as its tooltip.
    const titles = screen.getAllByTitle(RELEASE);
    expect(titles).toHaveLength(2);
    for (const note of titles) {
      expect(note.className).toContain("truncate");
      expect(note.textContent).toBe(RELEASE);
      // The codec line is another element: cutting the title short never cuts it.
      expect(note).not.toBe(screen.getByText("TrueHD · 7.1"));
    }
    const codecLine = screen.getByText("TrueHD · 7.1");
    expect(codecLine.className).not.toContain("truncate");
    // The track's name doesn't repeat the title either.
    expect(screen.getByText("English audio").textContent).toBe("English audio");
  });

  it("leaves out the title line when a track has none", async () => {
    show([stream({ index: 1 })]);
    await screen.findByText("TrueHD · 7.1");
    expect(document.querySelector("li .truncate")).toBeNull();
  });
});
