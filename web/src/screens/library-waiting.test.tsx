import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { finishedText, isWaiting } from "@/components/library-bar";
import { LibraryLink } from "@/components/shell";
import { api } from "@/lib/api";
import type { Library, LibraryStats, QueueState } from "@/lib/types";
import { Summary } from "./library";
import { LibraryRow, copyingFiles, overviewStatus } from "./overview";

/**
 * A library is only "finished" once nothing more is on its way: files still
 * being copied in (`LibraryStats.settling`) or a scan still looking mean the
 * files counted so far are not all there is.
 */

const LIB = "44444444-4444-4444-4444-444444444444";

function stats(partial: Partial<LibraryStats> = {}): LibraryStats {
  return {
    file_count: 3,
    total_bytes: 3_000_000,
    pending: 0,
    queued: 0,
    processing: 0,
    done: 2,
    skipped: 1,
    failed: 0,
    saved_bytes: 1_000_000,
    settling: 0,
    ...partial,
  };
}

function library(partial: Partial<Omit<Library, "stats">> & { stats?: Partial<LibraryStats> } = {}): Library {
  const { stats: s, ...rest } = partial;
  return {
    id: LIB,
    name: "Movies",
    path: "/media/Movies",
    enabled: true,
    profile: { goal: "balanced", min_savings_pct: 10 },
    stats: stats(s),
    scanning: false,
    last_scan_at: null,
    path_error: null,
    created_at: "2026-10-01T05:10:00Z",
    ...rest,
  } as unknown as Library;
}

function show(ui: React.ReactElement, libraries: Library[] = [library()]) {
  vi.spyOn(api, "libraries").mockResolvedValue(libraries);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const text = () => document.body.textContent ?? "";

describe("finishedText", () => {
  it("says how much is finished", () => {
    expect(finishedText(stats({ file_count: 5, done: 2, skipped: 1, pending: 2 }))).toBe("60% finished");
    expect(finishedText(stats())).toBe("100% finished");
  });

  it("never says 100% while a file is still being copied in, or a scan is still looking", () => {
    expect(finishedText(stats({ settling: 1 }))).toBeNull();
    expect(finishedText(stats(), 0, true)).toBeNull();
    // Part of the way, the number is still true.
    expect(finishedText(stats({ file_count: 5, pending: 2, settling: 2 }))).toBe("60% finished");
  });

  it("rounds down, so one file left is never 100%", () => {
    expect(finishedText(stats({ file_count: 300, done: 299, skipped: 0, pending: 1 }))).toBe("99% finished");
    expect(finishedText(stats({ file_count: 1000, done: 999, skipped: 0, pending: 1 }))).toBe("99% finished");
  });

  it("counts what the bar counts: unreadable originals are set aside", () => {
    expect(finishedText(stats({ file_count: 5, done: 2, skipped: 1, failed: 2 }), 2)).toBe("100% finished");
    expect(finishedText(stats({ file_count: 5, done: 2, skipped: 1, failed: 2 }), 2, true)).toBeNull();
  });

  it("is waiting for copies or a scan", () => {
    expect(isWaiting({ settling: 0 })).toBe(false);
    expect(isWaiting({ settling: 2 })).toBe(true);
    expect(isWaiting({ settling: 0 }, true)).toBe(true);
  });
});

describe("an Overview library row", () => {
  it("says it is finished once nothing more is on its way", () => {
    show(<LibraryRow library={library()} />);
    expect(text()).toContain("Everything is finished");
    expect(text()).toContain("100% finished");
  });

  it("says what it waits for instead while a file is still being copied in", () => {
    show(<LibraryRow library={library({ stats: { settling: 2 } })} />);
    expect(text()).toContain("Waiting for 2 files to finish copying");
    expect(text()).not.toContain("100% finished");
    expect(text()).not.toContain("Everything is finished");
  });

  it("does so when other files failed too", () => {
    show(<LibraryRow library={library({ stats: { settling: 1, failed: 1, file_count: 4 } })} />);
    expect(text()).toContain("Waiting for 1 file to finish copying");
    expect(text()).not.toContain("Everything else is finished");
    expect(text()).toContain("1 to review");
  });

  it("doesn't call a library finished while it is being scanned", () => {
    show(<LibraryRow library={library({ scanning: true })} />);
    expect(text()).toContain("Scanning");
    expect(text()).not.toContain("100% finished");
    expect(text()).not.toContain("Everything is finished");
  });

  it("keeps the files to go and the copies on separate lines", () => {
    show(<LibraryRow library={library({ stats: { file_count: 5, pending: 2, done: 2, skipped: 1, settling: 3 } })} />);
    expect(text()).toContain("2 files to go");
    expect(text()).toContain("Waiting for 3 files to finish copying");
    expect(text()).toContain("60% finished");
  });
});

describe("the sidebar's line for a library", () => {
  it("shows the percentage, and the files it waits for instead of 100%", () => {
    show(<LibraryLink library={library({ stats: { file_count: 5, pending: 2 } })} active={false} />);
    expect(text()).toContain("60% finished");
    cleanup();
    show(<LibraryLink library={library()} active={false} />);
    expect(text()).toContain("100% finished");
    cleanup();
    show(<LibraryLink library={library({ stats: { settling: 2 } })} active={false} />);
    expect(text()).toContain("Waiting for 2 files");
    expect(text()).not.toContain("100% finished");
  });

  it("says files are on their way, not that there are none, for an empty library", () => {
    show(<LibraryLink library={library({ stats: { file_count: 0, done: 0, skipped: 0, saved_bytes: 0, settling: 1 } })} active={false} />);
    expect(text()).toContain("Waiting for 1 file");
    expect(text()).not.toContain("No files yet");
    cleanup();
    show(<LibraryLink library={library({ stats: { file_count: 0, done: 0, skipped: 0, saved_bytes: 0 } })} active={false} />);
    expect(text()).toContain("No files yet");
  });
});

describe("a library page's summary", () => {
  it("says so far, and what it waits for, while files are still being copied in", () => {
    show(<Summary library={library({ stats: { settling: 2 } })} />);
    expect(text()).toContain("3 files finished so far");
    expect(text()).toContain("Waiting for 2 files to finish copying");
  });

  it("is plain once nothing more is on its way", () => {
    show(<Summary library={library()} />);
    expect(text()).toContain("3 files finished");
    expect(text()).not.toContain("so far");
    expect(text()).not.toContain("Waiting for");
  });
});

describe("the overview's status sentence", () => {
  const queue: QueueState = {
    paused: false,
    running: 0,
    queued: 0,
    max_jobs: 2,
    max_jobs_auto: true,
    max_jobs_source: "auto",
    waiting_for_schedule: false,
  };

  it("isn't 'All caught up' while a file is still being copied in", () => {
    expect(overviewStatus(queue, undefined, null, { failed: 0, blocked: false, settling: 2 }).text).toBe(
      "Waiting for 2 files to finish copying",
    );
    expect(overviewStatus(queue, undefined, null, { failed: 3, blocked: false, settling: 1 })).toMatchObject({
      text: "Waiting for 1 file to finish copying",
      detail: "3 files to review",
    });
    expect(overviewStatus(queue, undefined, null, { failed: 0, blocked: false, settling: 0 }).text).toBe("All caught up");
    // A setup problem still comes first.
    expect(overviewStatus(queue, undefined, null, { failed: 0, blocked: true, settling: 2 }).text).toBe("Waiting for a fix");
  });

  it("counts the copies of the libraries that are being watched", () => {
    const watched = library({ stats: { settling: 2 } });
    const paused = library({ enabled: false, stats: { settling: 5 } });
    const missing = library({ path_error: "The folder is gone.", stats: { settling: 7 } });
    expect(copyingFiles([watched, paused, missing])).toBe(2);
    expect(copyingFiles([])).toBe(0);
  });
});
