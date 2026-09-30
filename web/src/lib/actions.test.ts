import { afterEach, describe, expect, it, vi } from "vitest";
import { FORCED_READ_MAX, forcedRetries, sendBulk } from "./actions";
import { ApiError, api } from "./api";
import type { Job, JobQuery, ListResponse } from "./types";

/** A finished job, as the history lists it (newest first). */
function finished(file_id: string, state: Job["state"], force = false): Job {
  return { id: `job-${file_id}-${state}-${force}`, file_id, state, force } as Job;
}

/** A history of these jobs, newest first, served page by page. */
function serveHistory(jobs: Job[]) {
  return vi.spyOn(api, "jobs").mockImplementation(async (query: JobQuery): Promise<ListResponse<Job>> => {
    const offset = query.offset ?? 0;
    const limit = query.limit ?? 100;
    return { items: jobs.slice(offset, offset + limit), total: jobs.length };
  });
}

describe("trying files again repeats Convert anyway", () => {
  afterEach(() => vi.restoreAllMocks());

  it("finds files whose latest conversion was Convert anyway and didn't finish", async () => {
    serveHistory([
      // Newest first: f1's latest try was forced and failed (a full disk, say).
      finished("f1", "failed", true),
      finished("f2", "failed"),
      // f3 was stopped while converting anyway.
      finished("f3", "cancelled", true),
      // An older forced try of f2 doesn't count: its latest one is plain.
      finished("f2", "failed", true),
    ]);
    await expect(forcedRetries(["f1", "f2", "f3"])).resolves.toEqual(new Set(["f1", "f3"]));
  });

  it("stops reading once every file's latest conversion was seen, and never reads past its limit", async () => {
    const recent = serveHistory([finished("f1", "failed", true), ...Array.from({ length: 900 }, (_, i) => finished(`x${i}`, "done"))]);
    await forcedRetries(["f1"]);
    expect(recent).toHaveBeenCalledTimes(1);
    vi.restoreAllMocks();
    const never = serveHistory(Array.from({ length: 10_000 }, (_, i) => finished(`x${i}`, "done")));
    await expect(forcedRetries(["gone"])).resolves.toEqual(new Set());
    expect(never.mock.calls.every(([q]) => (q.offset ?? 0) < FORCED_READ_MAX)).toBe(true);
  });

  it("queues forced files one by one with force and the rest in bulk", async () => {
    serveHistory([finished("f1", "failed", true), finished("f2", "failed")]);
    const bulk = vi.spyOn(api, "bulk").mockResolvedValue({ affected: 1, left_out: 0 });
    const queue = vi.spyOn(api, "queueFile").mockResolvedValue({} as Job);
    await expect(sendBulk({ action: "retry_failed", ids: ["f1", "f2"] })).resolves.toEqual({ affected: 2, leftOut: 0 });
    expect(bulk).toHaveBeenCalledWith({ action: "retry_failed", ids: ["f2"] });
    expect(queue).toHaveBeenCalledWith("f1", { force: true });
  });

  it("sends no bulk request when every file was forced, and skips one already queued", async () => {
    serveHistory([finished("f1", "failed", true), finished("f2", "cancelled", true)]);
    const bulk = vi.spyOn(api, "bulk");
    vi.spyOn(api, "queueFile").mockImplementation(async (id) => {
      if (id === "f2") throw new ApiError(409, "already_queued", "It's already in the queue.");
      return {} as Job;
    });
    await expect(sendBulk({ action: "retry_failed", ids: ["f1", "f2"] })).resolves.toEqual({ affected: 1, leftOut: 0 });
    expect(bulk).not.toHaveBeenCalled();
  });

  it("leaves other bulk requests as they are", async () => {
    const jobs = vi.spyOn(api, "jobs");
    const bulk = vi.spyOn(api, "bulk").mockResolvedValue({ affected: 3, left_out: 1 });
    await expect(sendBulk({ action: "queue", ids: ["a", "b", "c", "d"] })).resolves.toEqual({ affected: 3, leftOut: 1 });
    await sendBulk({ action: "retry_failed", library: "lib" });
    expect(jobs).not.toHaveBeenCalled();
    expect(bulk).toHaveBeenLastCalledWith({ action: "retry_failed", library: "lib" });
  });
});
