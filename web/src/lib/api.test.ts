import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ApiError, api, errorFromBody, onReachability, request } from "./api";
import { fetchSystem } from "./queries";

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

describe("fetchSystem", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("returns the server facts", async () => {
    const info = { version: "0.2.0", default_temp_dir: "/temp", browse_roots: ["/media"], data_dir: "/config", in_container: true };
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(jsonResponse(200, info)));
    await expect(fetchSystem()).resolves.toEqual(info);
  });

  it("is null, not an error, on a server without the endpoint", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(jsonResponse(404, { error: "There's no API endpoint at /api/system.", code: "not_found" })),
    );
    await expect(fetchSystem()).resolves.toBeNull();
  });

  it("still fails when the server can't be reached", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("Failed to fetch")));
    await expect(fetchSystem()).rejects.toBeInstanceOf(ApiError);
  });
});
