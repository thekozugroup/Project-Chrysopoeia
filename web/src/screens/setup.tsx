"use client";

/**
 * First-run setup: welcome → folder → goal → start. Three short steps, each
 * skippable, ending with a library that is already scanning.
 */

import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, Clock, Cpu, ShieldCheck } from "lucide-react";
import { useState, type ReactNode } from "react";
import { toast } from "sonner";
import { Brand } from "@/components/brand";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/display";
import { api, errorMessage } from "@/lib/api";
import { hasAnyHardware } from "@/lib/hardware";
import { keys, useHardwareInfo } from "@/lib/queries";
import { navigate } from "@/lib/router";
import type { HardwareInfo, Settings } from "@/lib/types";
import { cn } from "@/lib/utils";
import { FolderStep, GoalStep, StepHeading } from "./library-steps";

type Step = "welcome" | "folder" | "goal";
const STEPS: Step[] = ["welcome", "folder", "goal"];

function StepIndicator({ step }: { step: Step }) {
  const index = STEPS.indexOf(step);
  return (
    <div className="flex items-center gap-3" aria-label={`Step ${index + 1} of ${STEPS.length}`} role="img">
      <div className="flex gap-1.5" aria-hidden>
        {STEPS.map((s, i) => (
          <span
            key={s}
            className={cn(
              "h-1.5 rounded-full transition-colors duration-300",
              i === index ? "w-6 bg-accent-ink" : i < index ? "w-1.5 bg-accent-ink/60" : "w-1.5 bg-line-strong/60",
            )}
          />
        ))}
      </div>
      <span className="text-xs text-muted tabular" aria-hidden>
        Step {index + 1} of {STEPS.length}
      </span>
    </div>
  );
}

/** What this machine brings, in one line (the welcome, and an empty overview). */
export function HardwareLine({ hw, detecting }: { hw: HardwareInfo | undefined; detecting: boolean }): ReactNode {
  if (detecting) return "Checking your hardware… this takes a few seconds.";
  if (!hw) return <Skeleton className="mt-1 h-3.5 w-56" />;
  if (!hw.ffmpeg.found) return "ffmpeg wasn't found in the container, so nothing can be converted yet.";
  const gpu = hw.gpus[0];
  if (hasAnyHardware(hw) && gpu) return `Found ${gpu.name}. It will do most of the work.`;
  if (gpu) return `Found ${gpu.name}, but it can't encode video here yet. Settings › Hardware explains how to fix that.`;
  return `No GPU found. Your ${hw.cpu.logical_cores}-core CPU will do the work, which is slower but just as good.`;
}

function Fact({ icon, title, children }: { icon: ReactNode; title: string; children: ReactNode }) {
  return (
    <li className="flex gap-3.5">
      <span className="mt-0.5 grid size-8 shrink-0 place-items-center rounded-full bg-accent-soft text-accent-ink [&_svg]:size-4">
        {icon}
      </span>
      <div className="min-w-0">
        <p className="text-sm font-semibold text-fg">{title}</p>
        <div className="mt-0.5 text-[0.8125rem] leading-relaxed text-muted">{children}</div>
      </div>
    </li>
  );
}

export function SetupScreen({ settings }: { settings: Settings }) {
  const client = useQueryClient();
  const hardware = useHardwareInfo();
  const [step, setStep] = useState<Step>("welcome");
  // Focus follows each step change, but not the page load itself.
  const [moved, setMoved] = useState(false);
  const goTo = (next: Step) => {
    setMoved(true);
    setStep(next);
  };
  const stepLabel = (s: Step) => `Step ${STEPS.indexOf(s) + 1} of ${STEPS.length}`;
  const [path, setPath] = useState<string | null>(null);
  const [folderError, setFolderError] = useState<string | null>(null);

  const finishOnboarding = async () => {
    const next = await api.updateSettings({ onboarded: true });
    client.setQueryData(keys.settings, next);
  };

  const skip = useMutation({
    mutationFn: finishOnboarding,
    onError: (err) => toast.error("Couldn't skip setup", { description: errorMessage(err) }),
  });

  return (
    <div className="min-h-dvh px-4 py-6 sm:px-8 sm:py-10">
      <div className="mx-auto flex w-full max-w-3xl flex-col">
        <header className="flex items-center justify-between gap-4">
          <Brand />
          <StepIndicator step={step} />
        </header>

        <main id="main" className="mt-12 sm:mt-20">
          {step === "welcome" ? (
            <div>
              <StepHeading
                level={1}
                focusOnMount={moved}
                className="font-display text-[2.75rem] leading-[1.05] text-fg outline-none sm:text-[3.5rem]"
              >
                Make your video library smaller, safely.
              </StepHeading>
              <p className="mt-5 max-w-xl text-[0.9375rem] leading-relaxed text-muted">
                Point Chrysopoeia at a folder and choose a goal. It converts files in the background, checks each
                result against the original, and only then replaces it.
              </p>
              <ul className="mt-10 flex max-w-xl flex-col gap-5">
                <Fact icon={<ShieldCheck aria-hidden />} title="Nothing is replaced until it passes its checks">
                  Every new file is played through and compared with the original to catch corruption and visual
                  glitches{settings.output_mode === "folder" ? ". Your originals stay untouched." : "."}
                </Fact>
                <Fact icon={<Cpu aria-hidden />} title="Uses your hardware automatically">
                  <HardwareLine hw={hardware.hw} detecting={hardware.detecting} />
                </Fact>
                <Fact icon={<Clock aria-hidden />} title="About a minute to set up">
                  Everything can be changed later in Settings.
                </Fact>
              </ul>
              {/* Primary first in the DOM (and tab order); shown on the right from sm up. */}
              <div className="mt-12 flex flex-col gap-3 sm:flex-row-reverse sm:items-center sm:justify-between">
                <Button variant="primary" size="lg" onClick={() => goTo("folder")}>
                  Choose a folder
                </Button>
                <Button variant="quiet" onClick={() => skip.mutate()} loading={skip.isPending}>
                  Skip for now
                </Button>
              </div>
            </div>
          ) : null}

          {step === "folder" ? (
            <div>
              <FolderStep
                level={1}
                step={stepLabel("folder")}
                initialPath={path ?? undefined}
                error={folderError}
                onNavigate={() => setFolderError(null)}
                onPicked={(picked) => {
                  setPath(picked);
                  setFolderError(null);
                  goTo("goal");
                }}
              />
              <div className="mt-6">
                <Button variant="quiet" onClick={() => goTo("welcome")}>
                  <ArrowLeft aria-hidden />
                  Back
                </Button>
              </div>
            </div>
          ) : null}

          {step === "goal" && path ? (
            <GoalStep
              level={1}
              step={stepLabel("goal")}
              path={path}
              submitLabel="Start"
              onChangeFolder={() => goTo("folder")}
              onFolderError={(message) => {
                setFolderError(message);
                goTo("folder");
              }}
              afterCreate={finishOnboarding}
              onCreated={() => {
                // No toast: the overview already says it's looking through the folder.
                navigate("/", { replace: true });
                // The app replaces this screen: start it at the top, with
                // focus on its content rather than lost on the page.
                window.scrollTo({ top: 0 });
                requestAnimationFrame(() => document.getElementById("main")?.focus({ preventScroll: true }));
              }}
              secondary={
                <Button variant="quiet" onClick={() => goTo("folder")}>
                  <ArrowLeft aria-hidden />
                  Back
                </Button>
              }
            />
          ) : null}
        </main>
      </div>
    </div>
  );
}
