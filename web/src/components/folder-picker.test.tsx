import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "@/lib/api";
import type { FsBrowse, FsEntry, UserFolder } from "@/lib/types";
import { FolderPicker, folderRefusal, groupEntries, quickFolders } from "./folder-picker";

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

/** What a container on Unraid lists at "/": the folders it was given, its own system, and a few plain ones. */
const MINE: UserFolder[] = [
  { name: "config", path: "/config", library_blocked: SETTINGS },
  { name: "media", path: "/media" },
  { name: "output", path: "/output" },
  { name: "temp", path: "/temp" },
];

function folder(name: string, partial: Partial<FsEntry> = {}): FsEntry {
  return { name, path: `/${name}`, is_dir: true, ...partial };
}

function containerRoot(partial: Partial<FsBrowse> = {}): FsBrowse {
  const system = ["app", "bin", "dev", "etc", "proc", "usr"].map((n) => folder(n, { system: true }));
  return listing("/", {
    entries: [
      ...system.slice(0, 2),
      folder("config"),
      ...system.slice(2, 4),
      folder("home"),
      folder("media", { media_count: 12 }),
      folder("mnt"),
      folder("output"),
      ...system.slice(4),
      folder("temp"),
    ],
    user_folders: MINE,
    library_blocked: WHOLE_SERVER,
    ...partial,
  });
}

const names = (entries: FsEntry[]) => entries.map((e) => e.name);

describe("quickFolders", () => {
  it("offers every folder given to the container, and leaves the settings folder out for a library", () => {
    expect(quickFolders({ user_folders: MINE }, false).map((f) => f.path)).toEqual([
      "/config",
      "/media",
      "/output",
      "/temp",
    ]);
    expect(quickFolders({ user_folders: MINE }, true).map((f) => f.path)).toEqual(["/media", "/output", "/temp"]);
    expect(quickFolders({}, false)).toEqual([]);
    expect(quickFolders({ user_folders: [] }, true)).toEqual([]);
  });
});

describe("groupEntries", () => {
  it("puts your folders first, the container's own last and the rest between", () => {
    const groups = groupEntries(containerRoot(), false);
    expect(names(groups.mine)).toEqual(["config", "media", "output", "temp"]);
    expect(names(groups.other)).toEqual(["home", "mnt"]);
    expect(names(groups.system)).toEqual(["app", "bin", "dev", "etc", "proc", "usr"]);
  });

  it("counts the settings folder with the system's, not with yours, for a library", () => {
    const groups = groupEntries(containerRoot(), true);
    expect(names(groups.mine)).toEqual(["media", "output", "temp"]);
    expect(names(groups.system)).toContain("config");
  });

  it("lists everything as the server sent it outside a container", () => {
    const data = listing("/", { entries: [folder("bin"), folder("media"), folder("mnt")] });
    expect(groupEntries(data, false)).toEqual({ mine: [], other: data.entries, system: [] });
    expect(groupEntries({ ...data, user_folders: [] }, true).other).toEqual(data.entries);
  });

  it("doesn't fold away a listing that is all system folders", () => {
    const data = listing("/usr", { entries: [folder("bin", { system: true }), folder("lib", { system: true })] });
    expect(groupEntries(data, false)).toEqual({ mine: [], other: data.entries, system: [] });
  });
});

describe("FolderPicker in a container", () => {
  const rows = () => screen.getAllByRole("button").filter((b) => b.hasAttribute("data-index"));

  it("offers the folders given to it as quick choices, with the system folders folded away", async () => {
    pick(containerRoot());
    const group = await screen.findByRole("group", { name: "Your folders" });
    const chips = Array.from(group.querySelectorAll("button")).map((b) => b.textContent);
    expect(chips).toEqual(["/config", "/media", "/output", "/temp"]);
    // Yours come first in the list, then the others; the system's stay folded.
    await waitFor(() => expect(rows().length).toBeGreaterThan(0));
    expect(rows().map((b) => b.textContent?.replace(/\s+/g, " ").trim())).toEqual([
      "config",
      "media12 videos",
      "output",
      "temp",
      "home",
      "mnt",
    ]);
    const toggle = screen.getByRole("button", { name: /System folders/ });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(toggle.textContent).toContain("(6)");
    expect(screen.queryByRole("button", { name: "etc" })).toBeNull();
    fireEvent.click(toggle);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(rows().map((b) => b.textContent?.trim()).slice(-6)).toEqual(["app", "bin", "dev", "etc", "proc", "usr"]);
  });

  it("leaves the settings folder out of the quick choices for a library, but keeps it findable", async () => {
    pick(containerRoot(), { forLibrary: true });
    const group = await screen.findByRole("group", { name: "Your folders" });
    expect(Array.from(group.querySelectorAll("button")).map((b) => b.textContent)).toEqual(["/media", "/output", "/temp"]);
    fireEvent.click(screen.getByRole("button", { name: /System folders/ }));
    expect(rows().some((b) => b.textContent === "config")).toBe(true);
  });

  it("opens a quick choice", async () => {
    const browse = vi.spyOn(api, "browse").mockImplementation(async (path) =>
      path === "/temp" ? listing("/temp", { user_folders: MINE }) : containerRoot(),
    );
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={client}>
        <FolderPicker initialPath="/" onSelect={() => {}} />
      </QueryClientProvider>,
    );
    const group = await screen.findByRole("group", { name: "Your folders" });
    fireEvent.click(Array.from(group.querySelectorAll("button")).find((b) => b.textContent === "/temp") as HTMLElement);
    await waitFor(() => expect(browse).toHaveBeenCalledWith("/temp", expect.anything()));
    // The folder you are in is the pressed choice, from any depth.
    await waitFor(() => {
      const pressed = Array.from(group.querySelectorAll("button")).filter((b) => b.getAttribute("aria-pressed") === "true");
      expect(pressed.map((b) => b.textContent)).toEqual(["/temp"]);
    });
  });

  it("keeps a way to the whole server when the roots are more than your folders", async () => {
    pick(containerRoot({ roots: ["/media", "/"] }));
    const group = await screen.findByRole("group", { name: "Your folders" });
    expect(Array.from(group.querySelectorAll("button")).map((b) => b.textContent)).toEqual([
      "/config",
      "/media",
      "/output",
      "/temp",
      "All folders",
    ]);
  });

  it("doesn't bring the settings folder back through the roots when a library's folder is chosen", async () => {
    pick(containerRoot({ roots: ["/media", "/temp", "/output", "/config"] }), { forLibrary: true });
    const group = await screen.findByRole("group", { name: "Your folders" });
    expect(Array.from(group.querySelectorAll("button")).map((b) => b.textContent)).toEqual(["/media", "/output", "/temp"]);
  });

  it("lists folders as before when nothing was given to it", async () => {
    pick(listing("/", { entries: [folder("bin"), folder("media"), folder("mnt")], user_folders: [] }));
    await waitFor(() => expect(rows()).toHaveLength(3));
    expect(screen.queryByRole("group", { name: "Your folders" })).toBeNull();
    expect(screen.queryByRole("button", { name: /System folders/ })).toBeNull();
    expect(screen.getByRole("button", { name: /Type a path/ })).toBeTruthy();
  });

  it("keeps the arrow keys on the folders shown", async () => {
    pick(containerRoot());
    await waitFor(() => expect(rows().length).toBeGreaterThan(0));
    const list = screen.getByRole("list", { name: "Folders in /" });
    fireEvent.keyDown(list, { key: "ArrowDown" });
    expect(document.activeElement?.textContent).toBe("config");
    fireEvent.keyDown(list, { key: "End" });
    // Folded away: the last one shown is the last of the others.
    expect(document.activeElement?.textContent).toBe("mnt");
  });
});
