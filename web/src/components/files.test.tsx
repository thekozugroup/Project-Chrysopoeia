import { describe, expect, it } from "vitest";
import { trackTitle } from "./files";
import { rootOf } from "./folder-picker";
import { skipFollowsSettings } from "@/lib/convertible";
import type { MediaFile, ProbeInfo, StreamInfo } from "@/lib/types";

function stream(partial: Partial<StreamInfo>): StreamInfo {
  return {
    index: 0,
    kind: "video",
    codec: "hevc",
    profile: null,
    language: null,
    title: null,
    is_default: false,
    is_forced: false,
    is_attached_pic: false,
    bit_rate: null,
    width: 1280,
    height: 720,
    pix_fmt: null,
    bit_depth: 8,
    frame_rate: 24,
    color_primaries: null,
    color_transfer: null,
    color_space: null,
    color_range: null,
    hdr: null,
    interlaced: false,
    channels: null,
    channel_layout: null,
    sample_rate: null,
    ...partial,
  };
}

function file(partial: Partial<MediaFile> = {}): MediaFile {
  return {
    id: "22222222-2222-2222-2222-222222222222",
    library_id: "33333333-3333-3333-3333-333333333333",
    path: "/media/Show - S01E02.mkv",
    relative_path: "Show - S01E02.mkv",
    file_name: "Show - S01E02.mkv",
    size_bytes: 2_770_000,
    modified_at: "2026-09-28T05:10:00Z",
    status: "skipped",
    container: "matroska",
    video_codec: "hevc",
    audio_codec: "aac",
    resolution: "720p",
    hdr: null,
    duration_secs: 8,
    bit_rate: null,
    original_size_bytes: null,
    saved_bytes: null,
    skip_reason: "Already HEVC",
    error: null,
    job_id: null,
    progress: null,
    scanned_at: "2026-09-28T05:10:00Z",
    updated_at: "2026-09-28T05:10:00Z",
    ...partial,
  };
}

const probe = (streams: StreamInfo[]): ProbeInfo => ({
  container: "matroska",
  format_long_name: null,
  duration_secs: 8,
  bit_rate: null,
  size_bytes: 1,
  start_time: 0,
  chapters: 0,
  streams,
});

describe("skipFollowsSettings", () => {
  it("is true for a video the library's settings chose to leave alone", () => {
    expect(skipFollowsSettings(file())).toBe(true);
    expect(skipFollowsSettings(file({ skip_reason: "Only 6% smaller — kept the original" }))).toBe(true);
  });

  it("is false for the user's own skip, audio-only files and anything not skipped", () => {
    expect(skipFollowsSettings(file({ skip_reason: "Skipped by you" }))).toBe(false);
    expect(
      skipFollowsSettings(
        file({ video_codec: null, skip_reason: "Audio-only files are left as they are", probe: probe([stream({ kind: "audio" })]) }),
      ),
    ).toBe(false);
    // Cover art is not real video.
    expect(skipFollowsSettings(file({ probe: probe([stream({ is_attached_pic: true }), stream({ kind: "audio" })]) }))).toBe(false);
    expect(skipFollowsSettings(file({ duration_secs: 0.4 }))).toBe(false);
    // Safety skips always apply, whatever the settings.
    expect(
      skipFollowsSettings(file({ skip_reason: "Dolby Vision profile 5 can't be converted without losing its colours — left unchanged" })),
    ).toBe(false);
    expect(skipFollowsSettings(file({ skip_reason: "HDR video would lose its colours as H.264 — left unchanged" }))).toBe(false);
    expect(skipFollowsSettings(file({ skip_reason: "The new file was 7% larger — kept the original" }))).toBe(true);
    expect(skipFollowsSettings(file({ status: "done" }))).toBe(false);
  });
});

describe("trackTitle", () => {
  it("names the language, or just the kind of track when it has none", () => {
    expect(trackTitle("audio", "eng", null)).toBe("English audio");
    expect(trackTitle("audio", null, null)).toBe("Audio");
    expect(trackTitle("audio", "und", "Commentary")).toBe("Audio · Commentary");
    expect(trackTitle("subtitles", "jpn", "Signs")).toBe("Japanese subtitles · Signs");
    expect(trackTitle("subtitles", null, null)).toBe("Subtitles");
  });
});

describe("rootOf", () => {
  const roots = ["/tmp", "/tmp/fe-media", "/media"];

  it("picks the most specific root that holds a folder", () => {
    expect(rootOf("/tmp/fe-media/TV", roots)).toBe("/tmp/fe-media");
    expect(rootOf("/tmp/fe-media", roots)).toBe("/tmp/fe-media");
    expect(rootOf("/tmp/other", roots)).toBe("/tmp");
  });

  it("doesn't mistake a sibling with a common prefix for a child", () => {
    expect(rootOf("/tmp/fe-media2", roots)).toBe("/tmp");
    expect(rootOf("/media2", roots)).toBeNull();
    expect(rootOf("/anything", ["/"])).toBe("/");
  });
});
