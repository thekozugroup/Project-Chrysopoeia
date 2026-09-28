import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  closeSheet,
  href,
  navigate,
  openSheet,
  parseRoute,
  registerGuard,
  resetRouterForTests,
  updateParams,
  usePendingNavigation,
  useRoute,
} from "./router";

/** Let jsdom deliver queued hashchange / popstate events. */
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

function useRouter() {
  return { route: useRoute(), prompt: usePendingNavigation() };
}

beforeEach(() => {
  resetRouterForTests();
  window.history.replaceState(null, "", "/#/settings/processing");
});

afterEach(() => {
  resetRouterForTests();
  document.body.innerHTML = "";
});

describe("parseRoute", () => {
  it("splits path segments and query parameters", () => {
    const route = parseRoute("#/library/abc/settings?status=failed&q=a%20b");
    expect(route.segments).toEqual(["library", "abc", "settings"]);
    expect(route.params.get("status")).toBe("failed");
    expect(route.params.get("q")).toBe("a b");
    expect(route.raw).toBe("/library/abc/settings?status=failed&q=a%20b");
  });

  it("treats an empty hash as the overview", () => {
    expect(parseRoute("").segments).toEqual([]);
    expect(parseRoute("#").raw).toBe("/");
  });

  it("survives malformed escapes", () => {
    expect(parseRoute("#/library/%E0%A4%A").segments).toEqual(["library", "%E0%A4%A"]);
  });

  it("decodes escaped segments and ignores stray slashes", () => {
    expect(parseRoute("#//library//a%2Fb/").segments).toEqual(["library", "a/b"]);
    expect(parseRoute("/queue/history?offset=50").segments).toEqual(["queue", "history"]);
  });

  it("keeps everything after the first ? as parameters", () => {
    const route = parseRoute("#/library/x?q=what?now&file=1");
    expect(route.params.get("q")).toBe("what?now");
    expect(route.params.get("file")).toBe("1");
  });
});

describe("href", () => {
  it("builds hash links and leaves out empty parameters", () => {
    expect(href("/queue")).toBe("#/queue");
    expect(href("queue/next")).toBe("#/queue/next");
    expect(href("/library/abc", { file: "f1", status: null, q: "", offset: 0 })).toBe("#/library/abc?file=f1&offset=0");
  });

  it("round-trips through parseRoute", () => {
    const route = parseRoute(href("/library/abc", { q: "Big Test (2020)", sort: "-size" }));
    expect(route.segments).toEqual(["library", "abc"]);
    expect(route.params.get("q")).toBe("Big Test (2020)");
    expect(route.params.get("sort")).toBe("-size");
  });
});

describe("navigation", () => {
  it("updates parameters in place without a new history entry", async () => {
    const { result } = renderHook(useRouter);
    const before = window.history.length;
    act(() => updateParams({ status: "failed", offset: null }));
    await act(flush);
    expect(window.location.hash).toBe("#/settings/processing?status=failed");
    expect(window.history.length).toBe(before);
    expect(result.current.route.params.get("status")).toBe("failed");
  });

  it("closes a sheet it opened by going back, leaving no duplicate entry", async () => {
    const { result } = renderHook(useRouter);
    act(() => navigate("/queue"));
    await act(flush);
    const length = window.history.length;
    act(() => openSheet({ job: "11111111-1111-1111-1111-111111111111" }));
    await act(flush);
    expect(result.current.route.params.get("job")).toBe("11111111-1111-1111-1111-111111111111");
    expect(window.history.length).toBe(length + 1);
    act(() => closeSheet(["job"]));
    await act(flush);
    await act(flush);
    expect(window.location.hash).toBe("#/queue");
    expect(result.current.route.params.get("job")).toBeNull();
  });

  it("closes a sheet from a shared link by removing its parameter", async () => {
    window.history.replaceState(null, "", "/#/queue?job=11111111-1111-1111-1111-111111111111");
    const { result } = renderHook(useRouter);
    act(() => closeSheet(["job"]));
    await act(flush);
    expect(window.location.hash).toBe("#/queue");
    expect(result.current.route.params.get("job")).toBeNull();
  });
});

describe("navigation guard", () => {
  const guard = () => ({
    blocks: vi.fn((target: ReturnType<typeof parseRoute>) => target.segments[0] !== "settings"),
    save: vi.fn(async () => true),
    discard: vi.fn(),
  });

  it("holds a link click that would leave the form, without touching history", async () => {
    const { result } = renderHook(useRouter);
    const g = guard();
    const unregister = registerGuard(g);
    const link = document.createElement("a");
    link.href = "#/queue";
    link.setAttribute("href", "#/queue");
    document.body.appendChild(link);
    const length = window.history.length;
    act(() => link.click());
    await act(flush);
    expect(window.location.hash).toBe("#/settings/processing");
    expect(window.history.length).toBe(length);
    expect(result.current.prompt.pending).toBe("#/queue");
    expect(result.current.route.segments).toEqual(["settings", "processing"]);
    unregister();
  });

  it("lets moves within the guarded area through", async () => {
    const { result } = renderHook(useRouter);
    const unregister = registerGuard(guard());
    act(() => navigate("/settings/output"));
    await act(flush);
    expect(result.current.route.segments).toEqual(["settings", "output"]);
    expect(result.current.prompt.pending).toBeNull();
    unregister();
  });

  it("puts the address back after back/forward or a typed address, and asks", async () => {
    const { result } = renderHook(useRouter);
    const unregister = registerGuard(guard());
    act(() => {
      window.location.hash = "#/queue";
    });
    await act(flush);
    expect(window.location.hash).toBe("#/settings/processing");
    expect(result.current.route.segments).toEqual(["settings", "processing"]);
    expect(result.current.prompt.pending).toBe("#/queue");
    unregister();
  });

  it("discards and goes on", async () => {
    const { result } = renderHook(useRouter);
    const g = guard();
    registerGuard(g);
    act(() => navigate("/queue"));
    await act(flush);
    act(() => result.current.prompt.discard());
    await act(flush);
    expect(g.discard).toHaveBeenCalledOnce();
    expect(result.current.route.segments).toEqual(["queue"]);
    expect(result.current.prompt.pending).toBeNull();
  });

  it("saves and goes on, or stays when saving fails", async () => {
    const { result } = renderHook(useRouter);
    const failing = { ...guard(), save: vi.fn(async () => false) };
    registerGuard(failing);
    act(() => navigate("/queue"));
    await act(flush);
    await act(() => result.current.prompt.save());
    expect(failing.save).toHaveBeenCalledOnce();
    expect(result.current.route.segments).toEqual(["settings", "processing"]);
    expect(result.current.prompt.pending).toBeNull();

    const ok = guard();
    registerGuard(ok);
    act(() => navigate("/queue"));
    await act(flush);
    await act(() => result.current.prompt.save());
    await act(flush);
    expect(ok.save).toHaveBeenCalledOnce();
    expect(result.current.route.segments).toEqual(["queue"]);
  });

  it("stays when asked to keep editing", async () => {
    const { result } = renderHook(useRouter);
    const g = guard();
    const unregister = registerGuard(g);
    act(() => navigate("/queue"));
    await act(flush);
    act(() => result.current.prompt.stay());
    expect(result.current.prompt.pending).toBeNull();
    expect(result.current.route.segments).toEqual(["settings", "processing"]);
    expect(g.discard).not.toHaveBeenCalled();
    unregister();
  });

  it("stops guarding once removed", async () => {
    const { result } = renderHook(useRouter);
    const unregister = registerGuard(guard());
    unregister();
    act(() => navigate("/queue"));
    await act(flush);
    expect(result.current.route.segments).toEqual(["queue"]);
  });
});
