import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { SystemInfo } from "@/lib/types";
import { automaticWorkFolderText, specificWorkFolderText } from "./settings";

afterEach(cleanup);

const TIP = "On Unraid, map /temp to your cache pool; Automatic then uses it.";

function system(partial: Partial<SystemInfo> = {}): SystemInfo {
  return {
    version: "0.2.0",
    build: null,
    default_temp_dir: null,
    browse_roots: ["/"],
    data_dir: "/config",
    in_container: true,
    ...partial,
  };
}

function text(node: React.ReactNode): string {
  const { container } = render(<p>{node}</p>);
  return container.textContent ?? "";
}

describe("the work folder choices", () => {
  it("Automatic says how to give it a fast drive: map /temp, nothing more", () => {
    expect(text(automaticWorkFolderText(system()))).toBe(
      `Next to each file, which needs free space on the same drive as the video. ${TIP}`,
    );
  });

  it("Automatic already uses /temp once it is mapped, and still says why", () => {
    expect(text(automaticWorkFolderText(system({ default_temp_dir: "/temp" })))).toBe(
      `Uses /temp, the work folder this server was started with. ${TIP}`,
    );
  });

  it("leaves Unraid out when the server isn't in a container", () => {
    expect(text(automaticWorkFolderText(system({ in_container: false })))).toBe(
      "Next to each file, which needs free space on the same drive as the video.",
    );
  });

  it("A specific folder is another folder, not the /temp step", () => {
    for (const info of [system(), system({ default_temp_dir: "/temp" }), undefined]) {
      const said = specificWorkFolderText(info);
      expect(said).toBe("Pick another folder inside the container, for example another mapped path.");
      expect(said).not.toContain("map /temp");
      expect(said).not.toContain("choose it here");
    }
    expect(specificWorkFolderText(system({ in_container: false }))).toBe("Pick another folder on this server.");
  });
});
