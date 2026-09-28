"use client";

/**
 * Goal cards: Save space / Balanced / Plays everywhere / Archive, each with a
 * one-line trade-off, the formats as secondary detail, and what this
 * machine's hardware means for speed.
 */

import { Gauge, Hourglass, TriangleAlert, Zap } from "lucide-react";
import { useId } from "react";
import { ChoiceCard } from "@/components/ui/controls";
import { Badge, Skeleton } from "@/components/ui/display";
import { codecSpeedHint, recommendedGoal, type SpeedHint } from "@/lib/hardware";
import { GOAL_LABEL, GOAL_SUMMARY, GOALS, profileSummary } from "@/lib/labels";
import { presetProfile } from "@/lib/profile";
import type { Goal, HardwareInfo, HwPreference, Presets, TranscodeProfile } from "@/lib/types";
import { cn } from "@/lib/utils";

function SpeedLine({ hint }: { hint: SpeedHint }) {
  const icon =
    hint.tone === "fast" ? (
      <Zap aria-hidden />
    ) : hint.tone === "slow" ? (
      <Hourglass aria-hidden />
    ) : (
      <TriangleAlert aria-hidden />
    );
  return (
    <span
      className={cn(
        "mt-2 flex items-start gap-1.5 text-[0.8125rem] leading-snug [&_svg]:mt-0.5 [&_svg]:size-3.5 [&_svg]:shrink-0",
        hint.tone === "fast" && "text-success",
        hint.tone === "slow" && "text-muted",
        hint.tone === "blocked" && "text-danger",
      )}
    >
      {icon}
      {hint.text}
    </span>
  );
}

interface GoalPickerProps {
  value: Goal;
  onChange: (goal: Exclude<Goal, "custom">) => void;
  presets: Presets | undefined;
  hardware: HardwareInfo | undefined;
  /** True while hardware detection has not answered yet. */
  hardwarePending?: boolean;
  preference?: HwPreference;
  /** The current profile, to describe a custom setup. */
  profile?: TranscodeProfile;
  label: string;
  className?: string;
  /** Two columns on wide screens (default) or always one. */
  columns?: 1 | 2;
}

export function GoalPicker({
  value,
  onChange,
  presets,
  hardware,
  hardwarePending,
  preference = "auto",
  profile,
  label,
  className,
  columns = 2,
}: GoalPickerProps) {
  const name = useId();
  const recommended = hardware ? recommendedGoal(hardware) : null;
  return (
    <fieldset className={className}>
      <legend className="sr-only">{label}</legend>
      <div className={cn("grid gap-3", columns === 2 && "sm:grid-cols-2")}>
        {GOALS.map((goal) => {
          const preset = presets?.goals.find((g) => g.goal === goal);
          const goalProfile = presetProfile(presets, goal);
          return (
            <ChoiceCard
              key={goal}
              name={name}
              value={goal}
              checked={value === goal}
              onChange={() => onChange(goal)}
              title={preset?.title ?? GOAL_LABEL[goal]}
              description={preset?.summary ?? GOAL_SUMMARY[goal]}
              badge={
                recommended === goal ? (
                  <Badge tone="accent" icon={<Gauge aria-hidden />}>
                    Best fit
                  </Badge>
                ) : null
              }
            >
              <span className="block font-mono text-xs text-muted">{profileSummary(goalProfile)}</span>
              {hardware ? (
                <SpeedLine hint={codecSpeedHint(hardware, goalProfile.video_codec, preference)} />
              ) : hardwarePending ? (
                <Skeleton className="mt-2.5 h-3.5 w-3/4" />
              ) : null}
            </ChoiceCard>
          );
        })}
        {value === "custom" && profile ? (
          <ChoiceCard
            name={name}
            value="custom"
            checked
            onChange={() => undefined}
            title="Custom"
            description="You changed the format under Advanced. Pick a goal to start from its defaults again."
            className={cn(columns === 2 && "sm:col-span-2")}
          >
            <span className="block font-mono text-xs text-muted">{profileSummary(profile)}</span>
          </ChoiceCard>
        ) : null}
      </div>
    </fieldset>
  );
}
