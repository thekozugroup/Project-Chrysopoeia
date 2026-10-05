import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "@/lib/api";
import type { FolderStatus } from "@/lib/types";
import { FolderDriveNotice } from "./settings";

/**
 * Settings says when the drive the output or work folder sits on isn't
 * connected as it was, and lets the user take another drive put there on
 * purpose (saving the same folder again doesn't).
 */

const DIFFERENT =
  "A different drive is mounted at /mnt/remotes/nas than before. Reconnect the usual one, or tell Szalinski to use the one there now.";
const NOT_CONNECTED =
  "The drive or share mounted at /mnt/remotes/nas isn't connected. Reconnect it, and its conversions continue.";

function folders(output: Partial<FolderStatus> = {}, work: Partial<FolderStatus> = {}): FolderStatus[] {
  return [
    { setting: "output_folder", path: "/mnt/remotes/nas/out", problem: null, changed_mount: null, ...output },
    { setting: "temp_dir", path: "/temp", problem: null, changed_mount: null, ...work },
  ];
}

function renderWithClient(ui: React.ReactElement) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("the output folder's drive swapped", () => {
  it("says so, and uses the drive there now only after the user agrees to what that means", async () => {
    vi.spyOn(api, "settingsFolders").mockResolvedValue(
      folders({ problem: DIFFERENT, changed_mount: "/mnt/remotes/nas" }),
    );
    const relearn = vi.spyOn(api, "relearnFolderMounts").mockResolvedValue(folders());
    renderWithClient(<FolderDriveNotice setting="output_folder" shown />);
    expect(await screen.findByText("A different drive is mounted")).toBeTruthy();
    expect(screen.getByText(DIFFERENT)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Use the drive that's there now" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog.textContent).toContain("Szalinski will take the drive mounted at /mnt/remotes/nas as the usual one");
    expect(dialog.textContent).toContain("Only do this if you replaced the drive or share on purpose");
    expect(relearn).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "Use this drive" }));
    await waitFor(() => expect(relearn).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(screen.queryByText("A different drive is mounted")).toBeNull());
  });

  it("is told for the work folder on its own", async () => {
    vi.spyOn(api, "settingsFolders").mockResolvedValue(
      folders({}, { path: "/mnt/cache", problem: DIFFERENT, changed_mount: "/mnt/cache" }),
    );
    const { container } = renderWithClient(<FolderDriveNotice setting="output_folder" shown />);
    renderWithClient(<FolderDriveNotice setting="temp_dir" shown />);
    expect(await screen.findByText("A different drive is mounted")).toBeTruthy();
    expect(container.textContent).toBe("");
  });

  it("isn't shown while another folder is picked", async () => {
    const asked = vi
      .spyOn(api, "settingsFolders")
      .mockResolvedValue(folders({ problem: DIFFERENT, changed_mount: "/mnt/remotes/nas" }));
    const { container } = renderWithClient(<FolderDriveNotice setting="output_folder" shown={false} />);
    await waitFor(() => expect(asked).toHaveBeenCalled());
    expect(container.textContent).toBe("");
  });
});

describe("other folder states", () => {
  it("a share that isn't connected is told without offering to use another drive", async () => {
    vi.spyOn(api, "settingsFolders").mockResolvedValue(folders({ problem: NOT_CONNECTED }));
    renderWithClient(<FolderDriveNotice setting="output_folder" shown />);
    expect(await screen.findByText("This folder's drive isn't connected")).toBeTruthy();
    expect(screen.getByText(NOT_CONNECTED)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Use the drive that's there now" })).toBeNull();
  });

  it("nothing is shown while the folders are fine", async () => {
    const asked = vi.spyOn(api, "settingsFolders").mockResolvedValue(folders());
    const { container } = renderWithClient(<FolderDriveNotice setting="output_folder" shown />);
    await waitFor(() => expect(asked).toHaveBeenCalled());
    expect(container.textContent).toBe("");
  });
});
