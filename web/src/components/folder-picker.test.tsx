import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "@/lib/api";
import type { FsBrowse } from "@/lib/types";
import { FolderPicker, folderRefusal } from "./folder-picker";

const WHOLE_SERVER =
  "The whole server can't be a library: it includes Chrysopoeia's own files and every share. Choose the folder that holds your videos.";
const SETTINGS = "/config is where Chrysopoeia keeps its database and settings. Choose the folder that holds your videos.";

function listing(path: string, partial: Partial<FsBrowse> = {}): FsBrowse {
  return {
    path,
    parent: path === "/" ? null : path.slice(0, path.lastIndexOf("/")) || "/",
    roots: ["/"],
    entries: [],
    media_count: 0,
    media_count_capped: false,
    ...partial,
  };
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function pick(data: FsBrowse, props: Partial<Parameters<typeof FolderPicker>[0]> = {}) {
  vi.spyOn(api, "browse").mockResolvedValue(data);
  const onSelect = vi.fn();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <FolderPicker initialPath={data.path} onSelect={onSelect} {...props} />
    </QueryClientProvider>,
  );
  return onSelect;
}

const useButton = () => screen.getByRole("button", { name: /^Use / }) as HTMLButtonElement;

describe("folderRefusal", () => {
  it("gives the server's reason for a library's folder, and nothing for other folders", () => {
    const blocked = listing("/", { library_blocked: WHOLE_SERVER });
    expect(folderRefusal(blocked, [], true)).toBe(WHOLE_SERVER);
    // Output and work folders may be anywhere.
    expect(folderRefusal(blocked, [], false)).toBeNull();
    expect(folderRefusal(listing("/media"), [], true)).toBeNull();
    expect(folderRefusal(listing("/media", { library_blocked: null }), [], true)).toBeNull();
  });

  it("still reports a clash with an existing library", () => {
    const libs = [{ name: "Movies", path: "/media/movies" }];
    expect(folderRefusal(listing("/media/movies"), libs, true)).toBe("This folder is already the library “Movies”.");
    // The server's reason comes first when both apply.
    expect(folderRefusal(listing("/", { library_blocked: WHOLE_SERVER }), libs, true)).toBe(WHOLE_SERVER);
  });
});

describe("FolderPicker for a library's folder", () => {
  it("can't use the server's root: the button is off and says why", async () => {
    const onSelect = pick(listing("/", { library_blocked: WHOLE_SERVER }), { forLibrary: true });
    await screen.findByText(WHOLE_SERVER);
    const button = useButton();
    expect(button.textContent).toContain("Use “/”");
    expect(button.disabled).toBe(true);
    // The reason is read out with the button.
    const reason = screen.getByText(WHOLE_SERVER);
    expect(reason.id).not.toBe("");
    expect(button.getAttribute("aria-describedby")).toBe(reason.id);
    fireEvent.click(button);
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("can't use the settings folder or anything inside it", async () => {
    pick(listing("/config/backups", { library_blocked: SETTINGS }), { forLibrary: true });
    await screen.findByText(SETTINGS);
    expect(useButton().disabled).toBe(true);
  });

  it("uses an ordinary folder", async () => {
    const onSelect = pick(listing("/media/movies", { media_count: 12 }), { forLibrary: true });
    await waitFor(() => expect(useButton().disabled).toBe(false));
    expect(document.body.textContent).not.toContain("can't be a library");
    fireEvent.click(useButton());
    expect(onSelect).toHaveBeenCalledWith("/media/movies");
  });
});

describe("FolderPicker for an output or work folder", () => {
  it("still lets the root be chosen", async () => {
    const onSelect = pick(listing("/", { library_blocked: WHOLE_SERVER }));
    await waitFor(() => expect(useButton().disabled).toBe(false));
    expect(screen.queryByText(WHOLE_SERVER)).toBeNull();
    fireEvent.click(useButton());
    expect(onSelect).toHaveBeenCalledWith("/");
  });
});
