import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "@/lib/api";
import type { Library } from "@/lib/types";
import { FolderProblem } from "@/screens/library";

/** The problem callout at the top of a library's page, and what it lets the user do. */

const LIB = "44444444-4444-4444-4444-444444444444";

const DIFFERENT =
  "A different drive is mounted at /mnt/remotes/nas than before. Reconnect the usual one, or tell Chrysopoeia to use the one there now.";

function library(partial: Partial<Library> = {}): Library {
  return {
    id: LIB,
    name: "Movies",
    path: "/mnt/remotes/nas/Movies",
    enabled: true,
    profile: { goal: "balanced", min_savings_pct: 10 },
    stats: { failed: 0, settling: 0 },
    scanning: false,
    last_scan_at: null,
    path_error: null,
    changed_mount: null,
    created_at: "2026-10-01T05:10:00Z",
    ...partial,
  } as unknown as Library;
}

function renderWithClient(ui: React.ReactElement) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

afterEach(cleanup);

describe("a library whose drive was swapped", () => {
  it("says so, and uses the drive there now only after the user agrees to what that means", async () => {
    const relearn = vi
      .spyOn(api, "relearnMounts")
      .mockResolvedValue(library({ path_error: null, changed_mount: null }));
    renderWithClient(<FolderProblem library={library({ path_error: DIFFERENT, changed_mount: "/mnt/remotes/nas" })} />);
    expect(screen.getByText("A different drive is mounted")).toBeTruthy();
    expect(screen.getByText(DIFFERENT)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Use the drive that's there now" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(within(dialog).getByRole("heading", { name: "Use the drive that's there now?" })).toBeTruthy();
    expect(dialog.textContent).toContain("Chrysopoeia will take the drive mounted at /mnt/remotes/nas as the usual one");
    expect(dialog.textContent).toContain("saves new files to it");
    expect(dialog.textContent).toContain("Only do this if you replaced the drive or share on purpose");
    expect(dialog.textContent).toContain("reconnect it instead");
    // Nothing changes until it's confirmed.
    expect(relearn).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "Use this drive" }));
    await waitFor(() => expect(relearn).toHaveBeenCalledWith(LIB));
  });

  it("can be called off", async () => {
    const relearn = vi.spyOn(api, "relearnMounts");
    renderWithClient(<FolderProblem library={library({ path_error: DIFFERENT, changed_mount: "/mnt/remotes/nas" })} />);
    fireEvent.click(screen.getByRole("button", { name: "Use the drive that's there now" }));
    const dialog = await screen.findByRole("alertdialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(relearn).not.toHaveBeenCalled();
  });
});

describe("other folder problems", () => {
  it("show the server's sentence without offering to use another drive", () => {
    const notConnected =
      "The drive or share mounted at /mnt/remotes/nas isn't connected. Reconnect it, and its conversions continue.";
    renderWithClient(<FolderProblem library={library({ path_error: notConnected })} />);
    expect(screen.getByText("Chrysopoeia can't read this folder")).toBeTruthy();
    expect(screen.getByText(notConnected)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Use the drive that's there now" })).toBeNull();
  });

  it("show nothing when the folder is fine, also from a server that doesn't send the drive", () => {
    const fine = library();
    delete (fine as Partial<Library>).changed_mount;
    const { container } = renderWithClient(<FolderProblem library={fine} />);
    expect(container.textContent).toBe("");
  });
});
