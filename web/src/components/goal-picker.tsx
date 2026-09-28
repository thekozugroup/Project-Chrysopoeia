"use client";

/**
 * Goal cards: Save space / Balanced / Plays everywhere / Archive. Each card
 * says what you get in one line, how fast it converts on this machine in a
 * word, and the formats as small secondary detail. The "Best fit" badge
 * carries the hardware recommendation, so nothing else repeats it.
 */

import { Gauge, Hourglass, TriangleAlert, Zap } from "lucide-react";
import { useId } from "react";
import { ChoiceCard } from "@/components/ui/controls";
import { Badge, Skeleton } from "@/components/ui/display";
import { recommendedGoal, speedWord, type SpeedWord } from "@/lib/hardware";
import { GOAL_LABEL, GOAL_SUMMARY, GOALS, profileSummary } from "@/lib/labels";
import { presetProfile } from "@/lib/profile";
import type { Goal, HardwareInfo, HwPreference, Presets, TranscodeProfile } from "@/lib/types";
import { cn } from "@/lib/utils";

const SPEED_ICON: Record<SpeedWord["tone"], typeof Zap> = {
  fast: Zap,
  medium: Gauge,
  slow: Hourglass,
  blocked: TriangleAlert,
};

/** "AV1 · Opus · MKV" on the left, "Slow here" on the right. */
function Formats({
  profile,
  hardware,
  hardwarePending,
  preference,
}: {
  profile: TranscodeProfile;
  hardware: HardwareInfo | undefined;
  hardwarePending?: boolean;
  preference: HwPreference;
}) {
  const speed = speedWord(hardware, profile.video_codec, preference);
  const Icon = speed ? SPEED_ICON[speed.tone] : null;
  return (
    <span className="mt-1.5 flex flex-wrap items-center justify-between gap-x-3 gap-y-1">
      <span className="font-mono text-xs text-muted">{profileSummary(profile)}</span>
      {speed && Icon ? (
        <span
          className={cn(
            "inline-flex items-center gap-1 text-[0.8125rem] font-medium",
            speed.tone === "fast" ? "text-success" : speed.tone === "blocked" ? "text-danger" : "text-muted",
          )}
        >
          <Icon className="size-3.5" aria-hidden />
          <span className="sr-only">Speed on this machine: </span>
          {speed.label}
        </span>
      ) : hardwarePending ? (
        <Skeleton className="h-3.5 w-16" />
      ) : null}
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
  /**
   * Offer the defaults for new libraries (Settings › Advanced) as the first
   * card, when they were customized. While it is selected no goal is.
   */
  defaults?: { profile: TranscodeProfile; selected: boolean; onSelect: () => void };
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
  defaults,
}: GoalPickerProps) {
  const name = useId();
  const recommended = hardware ? recommendedGoal(hardware) : null;
  const formats = (p: TranscodeProfile) => (
    <Formats profile={p} hardware={hardware} hardwarePending={hardwarePending} preference={preference} />
  );
  return (
    <fieldset className={className}>
      <legend className="sr-only">{label}</legend>
      <div className={cn("grid gap-3", columns === 2 && "sm:grid-cols-2")}>
        {defaults ? (
          <ChoiceCard
            name={name}
            value="defaults"
            checked={defaults.selected}
            onChange={defaults.onSelect}
            title="Your defaults"
            description="The settings for new libraries you chose in Settings › Advanced."
            className={cn(columns === 2 && "sm:col-span-2")}
          >
            {formats(defaults.profile)}
          </ChoiceCard>
        ) : null}
        {GOALS.map((goal) => {
          const preset = presets?.goals.find((g) => g.goal === goal);
          return (
            <ChoiceCard
              key={goal}
              name={name}
              value={goal}
              checked={!defaults?.selected && value === goal}
              onChange={() => onChange(goal)}
              title={preset?.title ?? GOAL_LABEL[goal]}
              // Our own outcome line: the server's summary names formats and
              // speeds, which the line below already shows.
              description={GOAL_SUMMARY[goal]}
              badge={
                recommended === goal ? (
                  <Badge tone="accent" icon={<Gauge aria-hidden />}>
                    Best fit
                  </Badge>
                ) : null
              }
            >
              {formats(presetProfile(presets, goal))}
            </ChoiceCard>
          );
        })}
        {value === "custom" && profile && !defaults ? (
          <ChoiceCard
            name={name}
            value="custom"
            checked
            onChange={() => undefined}
            title="Custom"
            description="You changed the format under More format options. Pick a goal to start from its defaults again."
            className={cn(columns === 2 && "sm:col-span-2")}
          >
            {formats(profile)}
          </ChoiceCard>
        ) : null}
      </div>
    </fieldset>
  );
}
