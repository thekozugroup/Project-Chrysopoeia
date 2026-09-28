import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it } from "vitest";
import { profileForGoal, sameProfile } from "@/lib/profile";
import type { TranscodeProfile } from "@/lib/types";
import { ProfileEditor } from "./profile-editor";

/** The editor as a settings form uses it: a draft, a saved base and Discard. */
function Form({ saved }: { saved: TranscodeProfile }) {
  const [draft, setDraft] = useState(saved);
  const [valid, setValid] = useState(true);
  const [resetKey, setResetKey] = useState(0);
  const dirty = !sameProfile(draft, saved);
  return (
    <div>
      <ProfileEditor
        profile={draft}
        onChange={setDraft}
        presets={undefined}
        hardware={undefined}
        onValidityChange={setValid}
        resetKey={resetKey}
      />
      <output data-testid="state">{`${valid ? "valid" : "invalid"} ${dirty ? "dirty" : "clean"}`}</output>
      <button
        type="button"
        onClick={() => {
          setDraft(saved);
          setResetKey((k) => k + 1);
        }}
      >
        Discard
      </button>
    </div>
  );
}

const state = () => screen.getByTestId("state").textContent;

afterEach(cleanup);

describe("ProfileEditor", () => {
  it("is valid again after Discard, even when invalid text was typed after a valid change", () => {
    render(<Form saved={profileForGoal("balanced")} />);
    fireEvent.click(screen.getByRole("button", { name: /Advanced/ }));
    const audio = screen.getByLabelText("Audio languages to keep");

    fireEvent.change(audio, { target: { value: "eng" } });
    expect(state()).toBe("valid dirty");
    fireEvent.change(audio, { target: { value: "eng xx1" } });
    expect(state()).toBe("invalid dirty");

    fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    expect(state()).toBe("valid clean");
    expect((screen.getByLabelText("Audio languages to keep") as HTMLInputElement).value).toBe("");
  });

  it("clears invalid text in an otherwise untouched form on Discard", () => {
    render(<Form saved={profileForGoal("balanced")} />);
    fireEvent.click(screen.getByRole("button", { name: /Advanced/ }));
    fireEvent.change(screen.getByLabelText("Subtitle languages to keep"), { target: { value: "english" } });
    expect(state()).toBe("invalid clean");
    expect(screen.getByRole("alert").textContent).toMatch(/Not valid: english/);

    fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    expect(state()).toBe("valid clean");
    expect((screen.getByLabelText("Subtitle languages to keep") as HTMLInputElement).value).toBe("");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("rejects raw quality values the codec's encoders don't accept", () => {
    render(<Form saved={profileForGoal("balanced")} />);
    fireEvent.click(screen.getByRole("button", { name: /Advanced/ }));
    const raw = screen.getByLabelText("Encoder quality value");
    fireEvent.change(raw, { target: { value: "55" } });
    expect(state()).toBe("invalid clean");
    expect(screen.getByRole("alert").textContent).toMatch(/0 to 51/);
    fireEvent.change(raw, { target: { value: "24" } });
    expect(state()).toBe("valid dirty");
  });

  it("names each goal card by its title alone", () => {
    render(<Form saved={profileForGoal("balanced")} />);
    const radio = screen.getByRole("radio", { name: "Balanced" });
    expect(radio).toBeTruthy();
    expect((radio as HTMLInputElement).checked).toBe(true);
    expect(radio.getAttribute("aria-describedby")).toBeTruthy();
  });
});
