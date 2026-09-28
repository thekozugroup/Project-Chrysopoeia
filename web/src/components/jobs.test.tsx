import { describe, expect, it } from "vitest";
import type { ValidationCheck } from "@/lib/types";
import { checkValueText, jobOverall, savingsText } from "./jobs";

const check = (id: string, value: number | null): ValidationCheck => ({
  id,
  label: id,
  status: "pass",
  detail: "",
  value,
});

describe("checkValueText", () => {
  it("gives each measured value its unit", () => {
    expect(checkValueText(check("duration", 1.25))).toBe("1.3 s off");
    expect(checkValueText(check("visual", 0.6123))).toBe("similarity 0.61");
    expect(checkValueText(check("black_frames", 2.4))).toBe("+2.4 s");
    expect(checkValueText(check("frozen_frames", 30))).toBe("+30 s");
  });

  it("leaves out numbers the sentence already covers or that mean nothing alone", () => {
    expect(checkValueText(check("duration", 0.01))).toBeNull();
    expect(checkValueText(check("black_frames", 0))).toBeNull();
    expect(checkValueText(check("size", 43.2))).toBeNull();
    expect(checkValueText(check("decode", 7200))).toBeNull();
    expect(checkValueText(check("visual", null))).toBeNull();
  });
});

describe("job summaries", () => {
  it("reports whole-file progress, not the stage's", () => {
    expect(jobOverall({ stage: "verifying", progress: 0 })).toBe(85);
    expect(jobOverall({ stage: "transcoding", progress: 100 })).toBe(85);
  });

  it("words savings and growth", () => {
    expect(savingsText(10e9, 4e9)).toEqual({ text: "Saved 6 GB (60%)", saved: true });
    expect(savingsText(1e9, 1.5e9)).toEqual({ text: "500 MB larger", saved: false });
    expect(savingsText(1e9, null)).toBeNull();
  });
});
