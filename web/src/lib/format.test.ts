import { describe, expect, it } from "vitest";
import {
  ffmpegVersionLabel,
  formatBytes,
  formatClock,
  formatDuration,
  formatEta,
  formatRelative,
  plural,
  splitFileName,
} from "./format";
import { languageLabel, skippedByUser, sourceCodecLabel } from "./labels";

describe("formatBytes", () => {
  it("uses decimal units like Finder", () => {
    expect(formatBytes(0)).toBe("0 bytes");
    expect(formatBytes(1)).toBe("1 byte");
    expect(formatBytes(999)).toBe("999 bytes");
    expect(formatBytes(12_345)).toBe("12 KB");
    expect(formatBytes(1_234_567)).toBe("1.23 MB");
    expect(formatBytes(345_600_000)).toBe("346 MB");
    expect(formatBytes(34_560_000)).toBe("34.6 MB");
    expect(formatBytes(1_200_000_000)).toBe("1.2 GB");
    expect(formatBytes(3_250_000_000)).toBe("3.25 GB");
  });

  it("rounds before promoting, so values never read 1,000 of a unit", () => {
    expect(formatBytes(999.6)).toBe("1 KB");
    expect(formatBytes(999_600)).toBe("1 MB");
    expect(formatBytes(999_700_000)).toBe("1 GB");
    expect(formatBytes(999_960_000_000)).toBe("1 TB");
    expect(formatBytes(99_960_000)).toBe("100 MB");
  });

  it("handles missing and negative values", () => {
    expect(formatBytes(null)).toBe("—");
    expect(formatBytes(undefined)).toBe("—");
    expect(formatBytes(Number.NaN)).toBe("—");
    expect(formatBytes(-2_500_000_000)).toBe("-2.5 GB");
  });
});

describe("durations", () => {
  it("formats clocks and words", () => {
    expect(formatClock(3725)).toBe("1:02:05");
    expect(formatClock(245)).toBe("4:05");
    expect(formatDuration(45)).toBe("45 s");
    expect(formatDuration(80 * 60)).toBe("1 h 20 min");
    expect(formatDuration(2 * 86_400)).toBe("2 days");
  });

  it("says roughly how long is left", () => {
    expect(formatEta(null)).toBeNull();
    expect(formatEta(20)).toBe("less than a minute left");
    expect(formatEta(70)).toBe("about a minute left");
    expect(formatEta(12 * 60 + 10)).toBe("about 12 min left");
    expect(formatEta(2 * 3600 + 7 * 60)).toBe("about 2 h 5 min left");
  });

  it("formats relative times", () => {
    const now = Date.parse("2026-09-28T12:00:00Z");
    expect(formatRelative("2026-09-28T11:59:40Z", now)).toBe("just now");
    expect(formatRelative("2026-09-28T11:57:00Z", now)).toBe("3 min ago");
    expect(formatRelative("2026-09-28T10:00:00Z", now)).toBe("2 h ago");
    expect(formatRelative(null, now)).toBe("—");
    expect(formatRelative("not a date", now)).toBe("—");
  });

  it("pluralises", () => {
    expect(plural(1, "file")).toBe("1 file");
    expect(plural(3, "file")).toBe("3 files");
    expect(plural(2, "library", "libraries")).toBe("2 libraries");
  });
});

describe("ffmpegVersionLabel", () => {
  it("drops the copyright notice from ffmpeg -version", () => {
    expect(ffmpegVersionLabel("ffmpeg version 6.1.1-3ubuntu5 Copyright (c) 2000-2023 the FFmpeg developers")).toBe(
      "ffmpeg 6.1.1-3ubuntu5",
    );
    expect(ffmpegVersionLabel("ffmpeg version 7.0.2-Jellyfin Copyright (c) 2000-2024 the FFmpeg developers")).toBe(
      "ffmpeg 7.0.2-Jellyfin",
    );
  });

  it("keeps unusual lines readable and says when nothing is known", () => {
    expect(ffmpegVersionLabel("custom build 2024 Copyright (c) someone")).toBe("custom build 2024");
    expect(ffmpegVersionLabel(null)).toBe("Version unknown");
    expect(ffmpegVersionLabel("   ")).toBe("Version unknown");
  });
});

describe("skippedByUser", () => {
  it("tells the user's own skips from the settings' decisions", () => {
    expect(skippedByUser("Skipped by you")).toBe(true);
    expect(skippedByUser(" skipped by you ")).toBe(true);
    expect(skippedByUser("Already HEVC")).toBe(false);
    expect(skippedByUser("Only 6% smaller — kept the original")).toBe(false);
    expect(skippedByUser(null)).toBe(false);
  });
});

describe("languageLabel", () => {
  it("names common codes and keeps unknown ones", () => {
    expect(languageLabel("eng")).toBe("English");
    expect(languageLabel("JPN")).toBe("Japanese");
    expect(languageLabel("tlh")).toBe("tlh");
  });
});

describe("sourceCodecLabel", () => {
  it("names known codecs and capitalises short unknown tokens", () => {
    expect(sourceCodecLabel("hevc")).toBe("HEVC");
    expect(sourceCodecLabel("mpeg2video")).toBe("MPEG-2");
    expect(sourceCodecLabel("dvvideo")).toBe("DVVIDEO");
  });

  it("keeps labels the server already wrote", () => {
    expect(sourceCodecLabel("No video")).toBe("No video");
    expect(sourceCodecLabel("No audio")).toBe("No audio");
    expect(sourceCodecLabel(null)).toBe("Unknown");
  });
});

describe("splitFileName", () => {
  it("keeps a late episode in view", () => {
    expect(splitFileName("Das außergewöhnlich lange Serienfinale einer Show (2024) - S01E04 - Pilot.mkv")).toEqual({
      head: "Das außergewöhnlich lange Serienfinale einer Show (2024) - ",
      tail: "S01E04 - Pilot.mkv",
    });
    expect(splitFileName("[Group] A Very Long Anime Series Title Here - 07 [1080p].mkv")?.tail).toBe("07 [1080p].mkv");
    expect(splitFileName("The Late Night Show With Someone Famous 2024-05-01 Guest.mkv")?.tail).toBe("2024-05-01 Guest.mkv");
  });

  it("cuts at the end when that already keeps what tells names apart", () => {
    // Sonarr's usual naming: the episode is near the start.
    expect(splitFileName("The Office (US) - S02E03 - The Dundies Extended Cut Bluray-1080p.mkv")).toBeNull();
    // A movie: its title comes first.
    expect(splitFileName("Some Very Long Movie Title (2010) Remastered Bluray-1080p.mkv")).toBeNull();
    // Short names fit.
    expect(splitFileName("Show - S01E02.mkv")).toBeNull();
    // Picture sizes and codecs aren't episodes.
    expect(splitFileName("A Rather Long Home Video Title Here 1920x1080 x264.mkv")).toBeNull();
  });
});
