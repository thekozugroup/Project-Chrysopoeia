import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { FileName } from "./file-name";

afterEach(cleanup);

describe("FileName", () => {
  it("keeps a late episode in view and gives screen readers and the tooltip the whole name", () => {
    const name = "Das außergewöhnlich lange Serienfinale einer Show (2024) - S01E04 - Pilot.mkv";
    const { container } = render(<FileName name={name} />);
    // The visible pieces are hidden from assistive technology; the whole name is read once.
    expect(screen.getByText(name).className).toContain("sr-only");
    expect(container.querySelector("[title]")?.getAttribute("title")).toBe(name);
    expect(container.querySelector("[data-file-tail]")?.textContent).toBe("S01E04 - Pilot.mkv");
    expect(container.querySelectorAll("[aria-hidden]").length).toBe(2);
  });

  it("cuts any other name at its end", () => {
    const name = "The Office (US) - S02E03 - The Dundies Extended Cut Bluray-1080p.mkv";
    const { container } = render(<FileName name={name} />);
    expect(container.querySelector("[data-file-tail]")).toBeNull();
    const shown = screen.getByText(name);
    expect(shown.className).toContain("truncate");
    expect(shown.getAttribute("title")).toBe(name);
  });
});
