import { QueryClient } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { request } from "./api";
import {
  Invalidator,
  MAX_BACKOFF_MS,
  POLL_GRACE_MS,
  POLL_INTERVAL_MS,
  UNAVAILABLE_AFTER,
  applyEvent,
  connectLive,
  jobMatchesList,
  patchJobList,
} from "./live";
import { keys } from "./queries";
import { useLive } from "./store";
import type {
  ActivityEntry,
  FileDetail,
  Job,
  Library,
  ListResponse,
  MediaFile,
  Overview,
  QueueState,
  ServerEvent,
} from "./types";

const JOB_ID = "11111111-1111-1111-1111-111111111111";
const FILE_ID = "22222222-2222-2222-2222-222222222222";
const LIB_ID = "33333333-3333-3333-3333-333333333333";

function job(partial: Partial<Job> = {}): Job {
  return {
    id: JOB_ID,
    file_id: FILE_ID,
    library_id: LIB_ID,
    file_name: "Movie.mkv",
    file_path: "/media/Movie.mkv",
    state: "running",
    stage: "transcoding",
    priority: 0,
    progress: 40,
    fps: 90,
    speed: 3,
    eta_secs: 300,
    encoder: "hevc_nvenc",
    hw_api: "nvenc",
    attempt: 1,
    input_size: 4e9,
    output_size: null,
    validation: null,
    error: null,
    skip_reason: null,
    command: null,
    log_tail: null,
    created_at: "2026-09-28T10:00:00Z",
    started_at: "2026-09-28T10:01:00Z",
    finished_at: null,
    ...partial,
  } as Job;
}

function file(partial: Partial<MediaFile> = {}): MediaFile {
  return {
    id: FILE_ID,
    library_id: LIB_ID,
    path: "/media/Movie.mkv",
    relative_path: "Movie.mkv",
    file_name: "Movie.mkv",
    size_bytes: 4e9,
    modified_at: "2026-01-01T00:00:00Z",
    status: "processing",
    container: "matroska",
    video_codec: "h264",
    audio_codec: "ac3",
    resolution: "1080p",
    hdr: null,
    duration_secs: 7200,
    bit_rate: 4_000_000,
    original_size_bytes: null,
    saved_bytes: null,
    skip_reason: null,
    error: null,
    job_id: JOB_ID,
    progress: 40,
    scanned_at: "2026-09-28T09:00:00Z",
    updated_at: "2026-09-28T10:00:00Z",
    ...partial,
  };
}

const probe = { container: "matroska", streams: [] } as unknown as MediaFile["probe"];

function setup() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const invalidate = new Invalidator(client);
  const schedule = vi.spyOn(invalidate, "schedule").mockImplementation(() => undefined);
  const apply = (event: ServerEvent) => applyEvent(client, invalidate, event);
  return { client, schedule, apply };
}

function scheduled(schedule: ReturnType<typeof setup>["schedule"]): string[] {
  return schedule.mock.calls.map(([key]) => JSON.stringify(key));
}

beforeEach(() => {
  useLive.setState({ connection: "connecting", polling: false, serverDown: false, jobs: {}, scans: {}, announcement: "" });
  window.history.replaceState(null, "", "/#/");
});

describe("applyEvent", () => {
  it("job.progress feeds the live store", () => {
    const { apply } = setup();
    apply({ type: "job.progress", job_id: JOB_ID, file_id: FILE_ID, stage: "verifying", progress: 12, fps: null, speed: null, eta_secs: 30, encoder: null, hw_api: null, attempt: 1 });
    expect(useLive.getState().jobs[JOB_ID]).toMatchObject({ stage: "verifying", progress: 12 });
  });

  it("job.updated patches the job, lists and file history, and clears finished progress", () => {
    const { client, schedule, apply } = setup();
    client.setQueryData<ListResponse<Job>>(keys.jobs({ state: "running" }), { items: [job()], total: 1 });
    client.setQueryData<FileDetail>(keys.file(FILE_ID), { file: file(), jobs: [job()] });
    useLive.getState().setJobProgress({ job_id: JOB_ID, file_id: FILE_ID, stage: "transcoding", progress: 90, fps: null, speed: null, eta_secs: null, encoder: null, hw_api: null, attempt: 1 });

    const done = job({ state: "done", progress: 100, output_size: 2e9, finished_at: "2026-09-28T11:00:00Z" });
    apply({ type: "job.updated", job: done });

    expect(client.getQueryData<Job>(keys.job(JOB_ID))?.state).toBe("done");
    // A finished job leaves "Converting now" at once (no card at 100% with Cancel).
    expect(client.getQueryData<ListResponse<Job>>(keys.jobs({ state: "running" }))).toEqual({ items: [], total: 0 });
    expect(client.getQueryData<FileDetail>(keys.file(FILE_ID))?.jobs[0].state).toBe("done");
    expect(useLive.getState().jobs[JOB_ID]).toBeUndefined();
    expect(scheduled(schedule)).toEqual(
      expect.arrayContaining([JSON.stringify(keys.jobs()), JSON.stringify(keys.overview), JSON.stringify(keys.file(FILE_ID))]),
    );
  });

  it("job.updated keeps the conversion notes the job sheet shows", () => {
    const { client, apply } = setup();
    client.setQueryData<ListResponse<Job>>(keys.jobs({ state: "history" }), { items: [job({ state: "done" })], total: 1 });
    const notes = ["Removed 2 picture-based subtitles because MP4 can't hold them"];
    apply({ type: "job.updated", job: job({ state: "done", notes }) });
    expect(client.getQueryData<Job>(keys.job(JOB_ID))?.notes).toEqual(notes);
    expect(client.getQueryData<ListResponse<Job>>(keys.jobs({ state: "history" }))?.items[0].notes).toEqual(notes);
  });

  it("job.updated keeps a job in lists it still belongs to and drops it from the others", () => {
    const { client, apply } = setup();
    const other = job({ id: "55555555-5555-5555-5555-555555555555", file_name: "Other.mkv" });
    client.setQueryData<ListResponse<Job>>(keys.jobs({ state: "active" }), { items: [job(), other], total: 2 });
    client.setQueryData<ListResponse<Job>>(keys.jobs({ state: "running", limit: 12 }), { items: [job(), other], total: 2 });
    apply({ type: "job.updated", job: job({ stage: "verifying", progress: 30 }) });
    expect(client.getQueryData<ListResponse<Job>>(keys.jobs({ state: "running", limit: 12 }))?.items.map((j) => j.stage)).toEqual([
      "verifying",
      "transcoding",
    ]);
    apply({ type: "job.updated", job: job({ state: "cancelled" }) });
    expect(client.getQueryData<ListResponse<Job>>(keys.jobs({ state: "running", limit: 12 }))).toEqual({ items: [other], total: 1 });
    expect(client.getQueryData<ListResponse<Job>>(keys.jobs({ state: "active" }))?.items).toEqual([other]);
  });

  it("job.updated leaves other lists and unknown jobs alone", () => {
    const { client, apply } = setup();
    const other = job({ id: "55555555-5555-5555-5555-555555555555", file_name: "Other.mkv" });
    const list = { items: [other], total: 1 };
    client.setQueryData<ListResponse<Job>>(keys.jobs({ state: "queued" }), list);
    apply({ type: "job.updated", job: job({ state: "running" }) });
    // Same object: nothing to patch, so no re-render either.
    expect(client.getQueryData(keys.jobs({ state: "queued" }))).toBe(list);
  });

  it("file.updated keeps tracks while the file is unchanged", () => {
    const { client, schedule, apply } = setup();
    client.setQueryData<FileDetail>(keys.file(FILE_ID), { file: file({ probe }), jobs: [] });
    client.setQueryData<ListResponse<MediaFile>>(keys.files({ library: LIB_ID }), { items: [file()], total: 1 });
    apply({ type: "file.updated", file: file({ progress: 55 }) });
    expect(client.getQueryData<FileDetail>(keys.file(FILE_ID))?.file.probe).toBe(probe);
    expect(client.getQueryData<ListResponse<MediaFile>>(keys.files({ library: LIB_ID }))?.items[0].progress).toBe(55);
    expect(client.getQueryData<ListResponse<MediaFile>>(keys.files({ library: LIB_ID }))?.items[0].probe).toBeUndefined();
    expect(scheduled(schedule)).not.toContain(JSON.stringify(keys.file(FILE_ID)));
  });

  it("file.updated refetches the tracks once the file was converted", () => {
    const { client, schedule, apply } = setup();
    client.setQueryData<FileDetail>(keys.file(FILE_ID), { file: file({ probe }), jobs: [] });
    apply({ type: "file.updated", file: file({ status: "done", size_bytes: 2e9, path: "/media/Movie.mkv" }) });
    expect(scheduled(schedule)).toContain(JSON.stringify(keys.file(FILE_ID)));
  });

  it("files.changed refreshes files, libraries and the overview", () => {
    const { schedule, apply } = setup();
    apply({ type: "files.changed", library_id: null });
    expect(scheduled(schedule)).toEqual(
      expect.arrayContaining([JSON.stringify(keys.files()), JSON.stringify(keys.libraries), JSON.stringify(keys.overview)]),
    );
  });

  it("library.updated and library.removed keep the list in step", () => {
    const { client, apply } = setup();
    const lib = { id: LIB_ID, name: "Movies" } as Library;
    const other = { id: "44444444-4444-4444-4444-444444444444", name: "TV" } as Library;
    client.setQueryData<Library[]>(keys.libraries, [other]);
    apply({ type: "library.updated", library: lib });
    expect(client.getQueryData<Library[]>(keys.libraries)?.map((l) => l.name)).toEqual(["TV", "Movies"]);
    apply({ type: "library.updated", library: { ...lib, name: "Films" } });
    expect(client.getQueryData<Library[]>(keys.libraries)?.map((l) => l.name)).toEqual(["TV", "Films"]);

    window.history.replaceState(null, "", `/#/library/${LIB_ID}/settings`);
    apply({ type: "library.removed", id: LIB_ID });
    expect(client.getQueryData<Library[]>(keys.libraries)?.map((l) => l.name)).toEqual(["TV"]);
    expect(window.location.hash).toBe("#/");
  });

  it("scan.progress tracks a scan and forgets it shortly after it's done", () => {
    vi.useFakeTimers();
    try {
      const { apply } = setup();
      apply({ type: "scan.progress", library_id: LIB_ID, library_name: "Movies", phase: "analyzing", discovered: 10, analyzed: 3, to_analyze: 10 });
      expect(useLive.getState().scans[LIB_ID]?.analyzed).toBe(3);
      apply({ type: "scan.progress", library_id: LIB_ID, library_name: "Movies", phase: "done", discovered: 10, analyzed: 10, to_analyze: 10 });
      vi.advanceTimersByTime(2000);
      expect(useLive.getState().scans[LIB_ID]).toBeUndefined();
    } finally {
      vi.useRealTimers();
    }
  });

  it("queue.state and stats.updated patch the queue and overview", () => {
    const { client, apply } = setup();
    client.setQueryData<Overview>(keys.overview, { totals: { done: 1 }, queue: { paused: false } } as unknown as Overview);
    const state: QueueState = { paused: true, running: 1, queued: 4, max_jobs: 2, max_jobs_auto: true, waiting_for_schedule: false };
    apply({ type: "queue.state", ...state });
    expect(client.getQueryData<QueueState>(keys.queue)).toEqual(state);
    expect(client.getQueryData<Overview>(keys.overview)?.queue).toEqual(state);
    apply({ type: "stats.updated", totals: { done: 5 } as Overview["totals"] });
    expect(client.getQueryData<Overview>(keys.overview)?.totals.done).toBe(5);
  });

  it("hardware.updated and settings.updated replace their caches", () => {
    const { client, apply } = setup();
    apply({ type: "hardware.updated", hardware: { encoders: [] } as never });
    expect(client.getQueryData(keys.hardware)).toEqual({ encoders: [] });
    apply({ type: "settings.updated", settings: { onboarded: true } as never });
    expect(client.getQueryData(keys.settings)).toEqual({ onboarded: true });
  });

  it("activity prepends new entries once", () => {
    const { client, apply } = setup();
    const entry = (id: number): ActivityEntry => ({ id, at: "2026-09-28T10:00:00Z", level: "info", message: `m${id}`, file_id: null, job_id: null, library_id: null });
    client.setQueryData(keys.activity, { items: [entry(1)] });
    apply({ type: "activity", entry: entry(2) });
    apply({ type: "activity", entry: entry(2) });
    expect(client.getQueryData<{ items: ActivityEntry[] }>(keys.activity)?.items.map((e) => e.id)).toEqual([2, 1]);
  });

  it("ignores event types it doesn't know", () => {
    const { apply } = setup();
    expect(() => apply({ type: "something.new" } as unknown as ServerEvent)).not.toThrow();
  });
});

/** A WebSocket stand-in the tests open, fail and message by hand. */
class FakeSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  static all: FakeSocket[] = [];
  readyState = FakeSocket.CONNECTING;
  onopen: ((e: Event) => void) | null = null;
  onmessage: ((e: MessageEvent) => void) | null = null;
  onclose: ((e: CloseEvent) => void) | null = null;
  onerror: ((e: Event) => void) | null = null;
  closed = false;

  constructor(public url: string) {
    FakeSocket.all.push(this);
  }

  close() {
    this.closed = true;
    this.readyState = FakeSocket.CLOSED;
  }

  accept() {
    this.readyState = FakeSocket.OPEN;
    this.onopen?.(new Event("open"));
  }

  drop() {
    this.readyState = FakeSocket.CLOSED;
    this.onclose?.(new CloseEvent("close"));
  }

  emit(event: object) {
    this.onmessage?.(new MessageEvent("message", { data: JSON.stringify(event) }));
  }
}

describe("connectLive", () => {
  let stop: (() => void) | null = null;

  beforeEach(() => {
    FakeSocket.all = [];
    vi.useFakeTimers();
    vi.stubGlobal("WebSocket", FakeSocket);
  });

  afterEach(() => {
    stop?.();
    stop = null;
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  const latest = () => FakeSocket.all[FakeSocket.all.length - 1];

  it("falls back to polling when the socket can't connect, and stops once it does", () => {
    const client = new QueryClient();
    const refetch = vi.spyOn(client, "invalidateQueries").mockResolvedValue();
    stop = connectLive(client);

    for (let i = 0; i < UNAVAILABLE_AFTER; i += 1) {
      latest().drop();
      vi.advanceTimersByTime(MAX_WAIT);
    }
    expect(useLive.getState().connection).toBe("unavailable");
    expect(useLive.getState().polling).toBe(true);
    const polled = refetch.mock.calls.length;
    expect(polled).toBeGreaterThan(0);

    vi.advanceTimersByTime(POLL_INTERVAL_MS);
    expect(refetch.mock.calls.length).toBeGreaterThan(polled);

    latest().accept();
    expect(useLive.getState().connection).toBe("open");
    expect(useLive.getState().polling).toBe(false);
    const afterOpen = refetch.mock.calls.length;
    vi.advanceTimersByTime(POLL_INTERVAL_MS * 3);
    expect(refetch.mock.calls.length).toBe(afterOpen);
  });

  it("doesn't poll during a short blip", () => {
    const client = new QueryClient();
    const refetch = vi.spyOn(client, "invalidateQueries").mockResolvedValue();
    stop = connectLive(client);
    latest().accept();
    latest().drop();
    expect(useLive.getState().connection).toBe("reconnecting");
    vi.advanceTimersByTime(1000);
    latest().accept();
    vi.advanceTimersByTime(POLL_GRACE_MS * 2);
    expect(useLive.getState().polling).toBe(false);
    // One full refetch after reconnecting, because events were missed.
    expect(refetch).toHaveBeenCalledWith();
  });

  it("forgets live progress when the connection drops", () => {
    const client = new QueryClient();
    vi.spyOn(client, "invalidateQueries").mockResolvedValue();
    stop = connectLive(client);
    latest().accept();
    latest().emit({ type: "job.progress", job_id: JOB_ID, file_id: FILE_ID, stage: "transcoding", progress: 50, fps: null, speed: null, eta_secs: null, encoder: null, hw_api: null, attempt: 1 });
    latest().emit({ type: "scan.progress", library_id: LIB_ID, library_name: "Movies", phase: "analyzing", discovered: 10, analyzed: 3, to_analyze: 10 });
    expect(useLive.getState().jobs[JOB_ID]).toBeDefined();
    expect(useLive.getState().scans[LIB_ID]).toBeDefined();
    latest().drop();
    expect(useLive.getState().jobs).toEqual({});
    expect(useLive.getState().scans).toEqual({});
  });

  it("reconnects as soon as a request reaches the server again, without waiting out the backoff", async () => {
    const client = new QueryClient();
    vi.spyOn(client, "invalidateQueries").mockResolvedValue();
    stop = connectLive(client);
    latest().accept();
    latest().drop();
    // The server is gone: sockets and requests fail for a while.
    for (let i = 0; i < 5; i += 1) {
      latest().drop();
      vi.advanceTimersByTime(MAX_BACKOFF_MS);
    }
    const fetchMock = vi.fn().mockRejectedValue(new TypeError("Failed to fetch"));
    vi.stubGlobal("fetch", fetchMock);
    await request("/queue").catch(() => undefined);
    expect(useLive.getState().serverDown).toBe(true);
    latest().drop();
    const before = FakeSocket.all.length;

    // The server is back: the next poll succeeds and a socket opens at once.
    fetchMock.mockResolvedValue(new Response("{}", { status: 200 }));
    await request("/queue");
    expect(useLive.getState().serverDown).toBe(false);
    expect(FakeSocket.all.length).toBe(before + 1);
    latest().accept();
    expect(useLive.getState().connection).toBe("open");
  });

  it("caps the wait between attempts so a restarted server is found within seconds", () => {
    const client = new QueryClient();
    vi.spyOn(client, "invalidateQueries").mockResolvedValue();
    stop = connectLive(client);
    latest().accept();
    for (let i = 0; i < 10; i += 1) latest().drop();
    const count = FakeSocket.all.length;
    vi.advanceTimersByTime(MAX_BACKOFF_MS);
    expect(FakeSocket.all.length).toBe(count + 1);
  });

  it("a successful request while the socket is merely unsupported doesn't cause reconnect storms", async () => {
    const client = new QueryClient();
    vi.spyOn(client, "invalidateQueries").mockResolvedValue();
    stop = connectLive(client);
    latest().drop();
    const count = FakeSocket.all.length;
    vi.stubGlobal("fetch", vi.fn().mockImplementation(async () => new Response("{}", { status: 200 })));
    await request("/queue");
    await request("/queue");
    expect(FakeSocket.all.length).toBe(count);
  });

  it("never keeps two sockets, even when an old one closes late", () => {
    const client = new QueryClient();
    vi.spyOn(client, "invalidateQueries").mockResolvedValue();
    stop = connectLive(client);
    const first = latest();
    first.accept();
    // The old socket is closing when the browser comes back online.
    first.readyState = FakeSocket.CLOSING;
    const lateClose = first.onclose;
    window.dispatchEvent(new Event("online"));
    const second = latest();
    expect(second).not.toBe(first);
    expect(first.closed).toBe(true);
    second.accept();

    // The superseded socket's close arrives afterwards: nothing may happen.
    lateClose?.call(first as unknown as WebSocket, new CloseEvent("close"));
    vi.advanceTimersByTime(MAX_WAIT);
    expect(FakeSocket.all).toHaveLength(2);
    expect(useLive.getState().connection).toBe("open");

    // And its messages are ignored.
    first.onmessage?.(new MessageEvent("message", { data: JSON.stringify({ type: "queue.state", paused: true }) }));
    expect(client.getQueryData(keys.queue)).toBeUndefined();
  });
});

/** Longer than any single backoff step the tests go through. */
const MAX_WAIT = 20_000;

describe("job list filters", () => {
  it("match the server's list states", () => {
    expect(jobMatchesList("running", "running")).toBe(true);
    expect(jobMatchesList("done", "running")).toBe(false);
    expect(jobMatchesList("queued", "active")).toBe(true);
    expect(jobMatchesList("running", "active")).toBe(true);
    expect(jobMatchesList("skipped", "active")).toBe(false);
    expect(jobMatchesList("queued", "queued")).toBe(true);
    expect(jobMatchesList("failed", "history")).toBe(true);
    expect(jobMatchesList("running", "history")).toBe(false);
    expect(jobMatchesList("done", undefined)).toBe(true);
  });

  it("patchJobList returns the same list when the job isn't in it", () => {
    const list = { items: [job({ id: "55555555-5555-5555-5555-555555555555" })], total: 1 };
    expect(patchJobList(list, job({ state: "done" }), "running")).toBe(list);
    expect(patchJobList(undefined, job(), "running")).toBeUndefined();
  });
});
