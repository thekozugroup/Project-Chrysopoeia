import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ApiError, api, errorFromBody, onReachability, request } from "./api";
import { countSavedFiles } from "./queries";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

describe("errorFromBody", () => {
  it("keeps the server's sentence, code and field", () => {
    const err = errorFromBody(400, {
      error: "Chrysopoeia can't write to the temporary folder /temp.",
      code: "invalid_settings",
      field: "temp_dir",
    });
    expect(err).toBeInstanceOf(ApiError);
    expect(err.message).toBe("Chrysopoeia can't write to the temporary folder /temp.");
    expect(err.code).toBe("invalid_settings");
    expect(err.field).toBe("temp_dir");
    expect(err.status).toBe(400);
  });

  it("has no field when the server names none (older servers)", () => {
    expect(errorFromBody(409, { error: "That folder is already the library Movies.", code: "library_exists" }).field).toBeNull();
  });

  it("falls back to a plain sentence for proxy error pages and odd bodies", () => {
    const gateway = errorFromBody(502, null);
    expect(gateway.code).toBe("http_502");
    expect(gateway.message).toMatch(/restarting/);
    expect(gateway.isUnavailable).toBe(true);
    const odd = errorFromBody(400, ["not", "an", "object"]);
    expect(odd.code).toBe("http_400");
    expect(odd.field).toBeNull();
    expect(errorFromBody(418, { error: "  " }).message).toBe("The server answered with an error (418).");
  });
});

describe("request", () => {
  const reachability: boolean[] = [];
  let stop: () => void = () => undefined;

  beforeEach(() => {
    reachability.length = 0;
    stop = onReachability((ok) => reachability.push(ok));
  });

  afterEach(() => {
    stop();
    vi.unstubAllGlobals();
  });

  it("decodes JSON and reports the server as reachable", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(jsonResponse(200, { ok: true, version: "0.2.0" })));
    await expect(api.health()).resolves.toEqual({ ok: true, version: "0.2.0" });
    expect(reachability).toEqual([true]);
  });

  it("turns error bodies into ApiError with the field, and still counts as reachable", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(jsonResponse(400, { error: "Jobs at once must be between 1 and 32.", code: "invalid_settings", field: "max_jobs" })),
    );
    const err = await api.updateSettings({ max_jobs: 99 }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).field).toBe("max_jobs");
    expect(reachability).toEqual([true]);
  });

  it("reports the server as unreachable when the connection fails or a proxy answers 503", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("Failed to fetch")));
    const network = await request("/queue").catch((e: unknown) => e);
    expect((network as ApiError).isNetwork).toBe(true);
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("<html>Bad gateway</html>", { status: 503 })));
    const gateway = await request("/queue").catch((e: unknown) => e);
    expect((gateway as ApiError).status).toBe(503);
    expect(reachability).toEqual([false, false]);
  });

  it("treats 204 as an empty answer", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(null, { status: 204 })));
    await expect(api.deleteLibrary("11111111-1111-1111-1111-111111111111")).resolves.toBeUndefined();
  });
});

describe("countSavedFiles", () => {
  afterEach(() => vi.unstubAllGlobals());

  const totals = (saved_bytes: number, done: number) => ({
    file_count: 10,
    total_bytes: 0,
    pending: 0,
    queued: 0,
    processing: 0,
    done,
    skipped: 0,
    failed: 0,
    saved_bytes,
    settling: 0,
  });

  /** A server whose files have these statuses and savings. */
  function serve(files: { status: string; saved_bytes: number | null }[]) {
    const fetch = vi.fn(async (input: RequestInfo | URL) => {
      const url = new URL(String(input), "http://localhost");
      const status = url.searchParams.get("status");
      const own = files.filter((f) => f.status === status);
      const limit = Number(url.searchParams.get("limit"));
      const offset = Number(url.searchParams.get("offset") ?? 0);
      return jsonResponse(200, { items: own.slice(offset, offset + limit), total: own.length });
    });
    vi.stubGlobal("fetch", fetch);
    return fetch;
  }

  it("counts every converted file the total adds up, and the ones that came out larger", async () => {
    serve([
      { status: "done", saved_bytes: 100 },
      { status: "done", saved_bytes: 50 },
      // "Convert anyway" that came out larger: converted, and it takes away from the total.
      { status: "done", saved_bytes: -20 },
      // Converted, now queued again: it keeps its savings.
      { status: "queued", saved_bytes: 70 },
      { status: "queued", saved_bytes: null },
    ]);
    // 200 = 100 + 50 - 20 + 70: four converted files, one of them larger, beside "Converted 3".
    await expect(countSavedFiles(totals(200, 3))).resolves.toEqual({ files: 4, larger: 1 });
  });

  it("doesn't read the queue when the converted files hold every byte saved", async () => {
    const fetch = serve([
      { status: "done", saved_bytes: 100 },
      { status: "queued", saved_bytes: null },
    ]);
    await expect(countSavedFiles(totals(100, 1))).resolves.toEqual({ files: 1, larger: 0 });
    expect(fetch.mock.calls.map(([url]) => new URL(String(url), "http://localhost").searchParams.get("status"))).toEqual([
      "done",
    ]);
  });
});
