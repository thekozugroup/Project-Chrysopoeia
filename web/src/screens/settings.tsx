"use client";

/**
 * Settings, in four plain sections. Edits collect in a draft shared by all
 * sections; the save bar sends only what changed, and API errors land on the
 * field they are about (including one inside the default profile).
 */

import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Minus, Plus } from "lucide-react";
import { useEffect, useId, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { FolderField } from "@/components/folder-field";
import { ProfileEditor } from "@/components/profile-editor";
import { SaveBar } from "@/components/save-bar";
import { PageHeader, ThemeSwitch } from "@/components/shell";
import { Button } from "@/components/ui/button";
import { ChoiceCard, Field, Input, Select, SwitchRow, Textarea } from "@/components/ui/controls";
import { Badge, Callout, CopyButton, Skeleton } from "@/components/ui/display";
import { ApiError, api, errorMessage } from "@/lib/api";
import { formatHour, plural } from "@/lib/format";
import { bugReportText, recommendedGoal } from "@/lib/hardware";
import { GOAL_LABEL, VALIDATION_HELP, VALIDATION_LABEL, VALIDATION_LEVELS } from "@/lib/labels";
import { defaultsCustomized, defaultsPresetGoal } from "@/lib/profile";
import { reasonsFor, setupFix, type SettingFocus, type SetupProblem } from "@/lib/outcomes";
import {
  keys,
  useFailures,
  useHardwareInfo,
  usePresets,
  useQueueState,
  useSettings,
  useSetupProblems,
  useSystem,
} from "@/lib/queries";
import { href, type Route } from "@/lib/router";
import {
  changedKeys,
  errorsFrom,
  keysToSave,
  profileFieldOf,
  rechosenAfter,
  saveBarMessage,
  SECTION_LABEL,
  sectionFor,
  type FieldErrors,
  type FolderSetting,
  type ProfileErrors,
  type SectionId,
} from "@/lib/settings-form";
import type { Goal, QueueState, Settings, SystemInfo, ValidationLevel } from "@/lib/types";
import { cn } from "@/lib/utils";
import { HardwareSection } from "./settings-hardware";

const SECTIONS: { id: SectionId; label: string; description: string }[] = [
  { id: "processing", label: SECTION_LABEL.processing, description: "How many files at once, and when." },
  { id: "output", label: SECTION_LABEL.output, description: "Where finished files go, and how they're checked first." },
  { id: "hardware", label: SECTION_LABEL.hardware, description: "What does the converting on this machine." },
  { id: "advanced", label: SECTION_LABEL.advanced, description: "Ignored files and defaults for new libraries." },
];

/**
 * What "Automatic" means for files at once, so Settings and the queue
 * never disagree: the queue's own number while the limit is automatic (the
 * container's MAX_JOBS, or the hardware's recommendation), else the
 * hardware's recommendation.
 */
export function automaticJobs(
  queue: QueueState | undefined,
  recommended: { total: number; reason: string } | undefined,
): { count: number | null; fromEnv: boolean; description: string } {
  const source = queue?.max_jobs_source;
  if (queue && source === "env") {
    return {
      count: queue.max_jobs,
      fromEnv: true,
      description: "Set by the container's MAX_JOBS variable. A number you choose here wins.",
    };
  }
  if (queue && source === "auto") {
    return {
      count: queue.max_jobs,
      fromEnv: false,
      description: recommended?.reason ?? "Chosen from your CPU cores, memory and GPU.",
    };
  }
  return {
    count: recommended?.total ?? null,
    fromEnv: false,
    description: recommended?.reason ?? "Chosen from your CPU cores, memory and GPU.",
  };
}

function Block({
  title,
  description,
  children,
  anchor,
}: {
  title: string;
  description?: ReactNode;
  children: ReactNode;
  /** The setting a link can bring into view (`?focus=`, see `SetupFix.setting`). */
  anchor?: SettingFocus;
}) {
  return (
    <section
      id={anchor ? `setting-${anchor}` : undefined}
      tabIndex={anchor ? -1 : undefined}
      className="grid scroll-mt-20 gap-4 border-t border-line py-7 outline-none first:border-t-0 first:pt-0 lg:grid-cols-[15rem_1fr] lg:gap-10"
    >
      <div>
        <h2 className="text-[1.0625rem] leading-snug font-semibold text-fg">{title}</h2>
        {description ? <p className="mt-1 text-[0.8125rem] leading-snug text-muted">{description}</p> : null}
      </div>
      <div className="flex min-w-0 max-w-2xl flex-col gap-5">{children}</div>
    </section>
  );
}

function Stepper({
  value,
  onChange,
  min,
  max,
  label,
}: {
  value: number;
  onChange: (value: number) => void;
  min: number;
  max: number;
  label: string;
}) {
  return (
    <div className="inline-flex items-center rounded-md border border-line-strong bg-surface shadow-card" role="group" aria-label={label}>
      <Button variant="quiet" size="icon" aria-label="Fewer" disabled={value <= min} onClick={() => onChange(Math.max(min, value - 1))}>
        <Minus />
      </Button>
      <input
        type="number"
        inputMode="numeric"
        aria-label={label}
        min={min}
        max={max}
        value={value}
        onChange={(e) => {
          const n = Math.round(Number(e.target.value));
          if (Number.isFinite(n)) onChange(Math.max(min, Math.min(max, n)));
        }}
        className="h-9 w-12 bg-transparent text-center text-sm font-semibold text-fg tabular [appearance:textfield] focus-visible:outline-none [&::-webkit-inner-spin-button]:appearance-none"
      />
      <Button variant="quiet" size="icon" aria-label="More" disabled={value >= max} onClick={() => onChange(Math.min(max, value + 1))}>
        <Plus />
      </Button>
    </div>
  );
}

const RESCAN: { value: number; label: string }[] = [
  { value: 0, label: "Never (watching only)" },
  { value: 1, label: "Every hour" },
  { value: 6, label: "Every 6 hours" },
  { value: 12, label: "Every 12 hours" },
  { value: 24, label: "Once a day" },
  { value: 48, label: "Every 2 days" },
  { value: 168, label: "Once a week" },
];

function ProcessingSection({ draft, onChange, errors }: SectionProps) {
  // The recommendation is a guess until detection has finished.
  const { hw } = useHardwareInfo();
  const queue = useQueueState();
  const name = useId();
  const auto = automaticJobs(queue.data, hw?.recommended_jobs);
  const hours = draft.active_hours;
  return (
    <>
      <Block title="Files at once" description="How many conversions run side by side.">
        <fieldset className="flex flex-col gap-3">
          <legend className="sr-only">Files at once</legend>
          <ChoiceCard
            name={name}
            value="auto"
            checked={draft.max_jobs === null}
            onChange={() => onChange({ max_jobs: null })}
            title={
              auto.count === null
                ? "Automatic"
                : auto.fromEnv
                  ? `Automatic (${auto.count}, from MAX_JOBS)`
                  : `Automatic (${auto.count})`
            }
            description={auto.description}
            badge={<Badge tone="accent">Recommended</Badge>}
          />
          <ChoiceCard
            name={name}
            value="manual"
            checked={draft.max_jobs !== null}
            onChange={() => onChange({ max_jobs: draft.max_jobs ?? auto.count ?? 2 })}
            title="Choose a number"
            description="More isn't always faster: GPUs limit parallel sessions, and CPUs share their cores."
          >
            {draft.max_jobs !== null ? (
              <span className="mt-3 flex items-center gap-3">
                <Stepper value={draft.max_jobs} onChange={(max_jobs) => onChange({ max_jobs })} min={1} max={32} label="Files at once" />
                {auto.count !== null && draft.max_jobs > auto.count * 2 ? (
                  <span className="text-[0.8125rem] text-warning">Much more than recommended ({auto.count}).</span>
                ) : null}
              </span>
            ) : null}
          </ChoiceCard>
          {errors.max_jobs ? <p role="alert" className="text-[0.8125rem] font-medium text-danger">{errors.max_jobs}</p> : null}
        </fieldset>
      </Block>

      <Block title="When to convert" description="Saved changes apply right away. Running files always finish.">
        <SwitchRow
          label="Only start new conversions during certain hours"
          description="Handy for keeping evenings free for streaming. Uses the server's time zone."
          checked={hours !== null}
          onCheckedChange={(on) => onChange({ active_hours: on ? (hours ?? { start: 1, end: 7 }) : null })}
        />
        {hours ? (
          <div className="flex flex-wrap items-end gap-3 pl-0 sm:pl-0">
            <Field label="From" className="w-32">
              <Select value={hours.start} onChange={(e) => onChange({ active_hours: { ...hours, start: Number(e.target.value) } })}>
                {Array.from({ length: 24 }, (_, h) => (
                  <option key={h} value={h}>
                    {formatHour(h)}
                  </option>
                ))}
              </Select>
            </Field>
            <Field label="Until" className="w-32">
              <Select value={hours.end} onChange={(e) => onChange({ active_hours: { ...hours, end: Number(e.target.value) } })}>
                {Array.from({ length: 24 }, (_, h) => (
                  <option key={h} value={h}>
                    {formatHour(h)}
                  </option>
                ))}
              </Select>
            </Field>
            <p className="pb-2 text-[0.8125rem] text-muted">
              {hours.start === hours.end
                ? "Same start and end means all day."
                : hours.start > hours.end
                  ? "Runs overnight, past midnight."
                  : null}
            </p>
          </div>
        ) : null}
        {errors.active_hours ? <p role="alert" className="text-[0.8125rem] font-medium text-danger">{errors.active_hours}</p> : null}
        <SwitchRow
          label="Run at low priority"
          description="Keeps the server responsive for streaming and other apps while converting."
          checked={draft.low_priority}
          onCheckedChange={(low_priority) => onChange({ low_priority })}
        />
      </Block>

      <Block title="Finding files" description="How new and changed files are picked up.">
        <SwitchRow
          label="Convert new files automatically"
          description="When off, new files wait in their library until you queue them."
          checked={draft.auto_queue}
          onCheckedChange={(auto_queue) => onChange({ auto_queue })}
        />
        <SwitchRow
          label="Watch folders for changes"
          description="Notices new and changed files within moments. Some network shares don't report changes; the full scan below catches those."
          checked={draft.watch_folders}
          onCheckedChange={(watch_folders) => onChange({ watch_folders })}
        />
        <Field label="Full rescan" description="A complete pass over every library, as a safety net.">
          <Select
            value={draft.rescan_interval_hours}
            onChange={(e) => onChange({ rescan_interval_hours: Number(e.target.value) })}
            className="max-w-64"
          >
            {RESCAN.some((r) => r.value === draft.rescan_interval_hours) ? null : (
              <option value={draft.rescan_interval_hours}>Every {draft.rescan_interval_hours} hours</option>
            )}
            {RESCAN.map((r) => (
              <option key={r.value} value={r.value}>
                {r.label}
              </option>
            ))}
          </Select>
        </Field>
      </Block>
    </>
  );
}

/** How to get a fast work folder under "Automatic" on Unraid: map /temp, nothing to choose. */
const UNRAID_TEMP_TIP = "On Unraid, map /temp to your cache pool; Automatic then uses it.";

/**
 * What "Automatic" means for the work folder on this server; the general
 * rule while it loads. In a container it also says how to give Automatic a
 * fast drive (map /temp), which is all it takes: the folder doesn't have to
 * be chosen again under "A specific folder".
 */
export function automaticWorkFolderText(system: SystemInfo | undefined): ReactNode {
  if (!system) {
    return "The server's work folder when it has one (the Docker image uses /temp when it's mapped), otherwise next to each file, which needs free space on the same drive as the video.";
  }
  if (system.default_temp_dir) {
    return (
      <>
        Uses <span className="font-mono text-[0.8125rem] text-fg">{system.default_temp_dir}</span>, the work folder this
        server was started with.
        {system.in_container ? <> {UNRAID_TEMP_TIP}</> : null}
      </>
    );
  }
  return system.in_container
    ? `Next to each file, which needs free space on the same drive as the video. ${UNRAID_TEMP_TIP}`
    : "Next to each file, which needs free space on the same drive as the video.";
}

/** What "A specific folder" means for the work folder: another folder than the one Automatic uses. */
export function specificWorkFolderText(system: SystemInfo | undefined): string {
  return system?.in_container === false
    ? "Pick another folder on this server."
    : "Pick another folder inside the container, for example another mapped path.";
}

/**
 * Bring the setting a problem's link points at (`?focus=temp_dir`) into
 * view, once the frame has scrolled to the top of the new screen.
 */
function useFocusSetting(focus: string | null) {
  useEffect(() => {
    if (!focus) return;
    const frame = requestAnimationFrame(() => {
      const el = document.getElementById(`setting-${focus}`);
      if (!el) return;
      el.scrollIntoView({ block: "start" });
      el.focus({ preventScroll: true });
    });
    return () => cancelAnimationFrame(frame);
  }, [focus]);
}

/**
 * What recent conversions ran into with this setting, in the server's own
 * words (which name the folder and the fix), so the setting that needs a
 * fix is marked where it's made. Files are tried again from the overview.
 */
function RecentProblem({ kinds }: { kinds: SetupProblem[] }) {
  const failures = useFailures();
  const { kept } = useSetupProblems();
  const settings = useSettings();
  for (const kind of kinds) {
    const items = [...(failures.setup[kind] ?? []), ...(kept[kind] ?? [])];
    if (!items.length) continue;
    const reason = reasonsFor(items, 1).reasons[0]?.text;
    const fix = setupFix(kind, settings.data?.output_mode);
    return (
      <Callout tone="warning" title={fix.title}>
        <p>{reason ?? fix.fix}</p>
        <p className="mt-1.5">
          {`${plural(items.length, "file")} waited on this.`} Once it&apos;s fixed, try{" "}
          {items.length === 1 ? "it" : "them"} again from the <a href={href("/")}>Overview</a>.
        </p>
      </Callout>
    );
  }
  return null;
}

function OutputSection({ draft, onChange, errors, focus }: SectionProps & { focus: string | null }) {
  const name = useId();
  const tempName = useId();
  const system = useSystem();
  useFocusSetting(focus);
  return (
    <>
      <Block
        title="Finished files"
        description="Nothing is written anywhere until the new file has passed its checks."
        anchor="output_folder"
      >
        <RecentProblem kinds={["destination"]} />
        <fieldset className="flex flex-col gap-3">
          <legend className="sr-only">Where finished files go</legend>
          <ChoiceCard
            name={name}
            value="replace"
            checked={draft.output_mode === "replace"}
            onChange={() => onChange({ output_mode: "replace" })}
            title="Replace the original"
            description="The new file takes the original's place, keeping your media server's library tidy. When the goal's format differs, the extension changes too (Movie.mp4 becomes Movie.mkv); Plex, Jellyfin and Emby pick that up at their next scan."
          />
          <ChoiceCard
            name={name}
            value="folder"
            checked={draft.output_mode === "folder"}
            onChange={() => onChange({ output_mode: "folder" })}
            title="Save to a separate folder"
            description="Originals stay untouched. New files mirror each library's folder structure, with the goal's extension (such as .mkv)."
          />
        </fieldset>
        {draft.output_mode === "folder" ? (
          <div>
            <p id={`${name}-label`} className="mb-1.5 text-sm font-medium text-fg">
              Output folder
            </p>
            <FolderField
              labelId={`${name}-label`}
              value={draft.output_folder}
              onChange={(output_folder) => onChange({ output_folder })}
              placeholder="No folder chosen"
              dialogTitle="Choose the output folder"
              dialogDescription="Finished files are written here, in the same folders as in the library."
              error={errors.output_folder ?? (draft.output_folder ? null : "Choose a folder, or switch back to replacing originals.")}
            />
          </div>
        ) : null}
        <SwitchRow
          label="Keep file dates"
          description="Gives the new file the original's modification date, so Plex, Jellyfin and Emby don't list it as newly added."
          checked={draft.keep_file_dates}
          onCheckedChange={(keep_file_dates) => onChange({ keep_file_dates })}
        />
      </Block>

      <ChecksBlock draft={draft} onChange={onChange} />

      <Block
        title="Work folder"
        description="Where files are written while they're being converted. A fast SSD or cache pool speeds things up."
        anchor="temp_dir"
      >
        <RecentProblem kinds={["work_folder", "disk_full"]} />
        <fieldset className="flex flex-col gap-3">
          <legend className="sr-only">Work folder</legend>
          <ChoiceCard
            name={tempName}
            value="next"
            checked={draft.temp_dir === null}
            onChange={() => onChange({ temp_dir: null })}
            title="Automatic"
            description={automaticWorkFolderText(system.data)}
          />
          <ChoiceCard
            name={tempName}
            value="folder"
            checked={draft.temp_dir !== null}
            onChange={() => onChange({ temp_dir: draft.temp_dir ?? "" })}
            title="A specific folder"
            description={specificWorkFolderText(system.data)}
          />
        </fieldset>
        {draft.temp_dir !== null ? (
          <FolderField
            value={draft.temp_dir || null}
            onChange={(temp_dir) => onChange({ temp_dir })}
            placeholder="No folder chosen"
            dialogTitle="Choose a work folder"
            dialogDescription="In-progress files are written here and moved into place when they pass their checks."
            error={errors.temp_dir ?? (draft.temp_dir ? null : "Choose a folder, or switch back to Automatic.")}
          />
        ) : null}
      </Block>
    </>
  );
}

/**
 * How carefully each result is checked before it replaces the original:
 * Quick → Standard → Thorough, each adding to the one before. "Off" is a
 * separate switch with a warning, not a fourth choice.
 */
function ChecksBlock({ draft, onChange }: Pick<SectionProps, "draft" | "onChange">) {
  const name = useId();
  const on = draft.validation !== "off";
  // Turning checks back on returns to the level chosen before.
  const [level, setLevel] = useState<Exclude<ValidationLevel, "off">>(
    draft.validation === "off" ? "standard" : draft.validation,
  );
  if (draft.validation !== "off" && draft.validation !== level) setLevel(draft.validation);
  return (
    <Block
      title="Checks before replacing"
      description="Each finished file is compared with its original. If a check fails, the original is kept and you're told why."
    >
      <SwitchRow
        label="Check new files before keeping them"
        description="Recommended. Without checks, a damaged conversion could replace a good original."
        checked={on}
        onCheckedChange={(checked) => onChange({ validation: checked ? level : "off" })}
      />
      {on ? (
        <fieldset className="flex flex-col gap-3">
          <legend className="sr-only">How carefully to check</legend>
          {VALIDATION_LEVELS.map((level) => (
            <ChoiceCard
              key={level}
              name={name}
              value={level}
              checked={draft.validation === level}
              onChange={() => onChange({ validation: level })}
              title={VALIDATION_LABEL[level]}
              description={VALIDATION_HELP[level]}
              badge={level === "standard" ? <Badge tone="accent">Recommended</Badge> : null}
            />
          ))}
        </fieldset>
      ) : draft.output_mode === "replace" ? (
        <Callout tone="warning" title="Originals will be replaced without any checks">
          A file damaged during conversion would replace a good one. Keep checks on unless you have backups.
        </Callout>
      ) : null}
    </Block>
  );
}

/**
 * Ignore rules every install starts with. They stay out of the text box
 * (so nobody deletes them by accident) and are described in words instead.
 */
const BUILT_IN_PATTERNS: Record<string, string> = {
  "**/.*": "hidden files and folders",
  "**/@eaDir/**": "Synology @eaDir folders",
  "**/#recycle/**": "#recycle bins",
  "**/*.partial~": "unfinished downloads (.partial~)",
};

/** Split saved patterns into the built-in ones and the user's own. */
export function splitPatterns(patterns: string[]): { builtIn: string[]; own: string[] } {
  return {
    builtIn: patterns.filter((p) => p in BUILT_IN_PATTERNS),
    own: patterns.filter((p) => !(p in BUILT_IN_PATTERNS)),
  };
}

function builtInText(builtIn: string[]): string | null {
  if (!builtIn.length) return null;
  const names = builtIn.map((p) => BUILT_IN_PATTERNS[p]);
  const list = names.length === 1 ? names[0] : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
  return `Always ignored: ${list}.`;
}

function AdvancedSection({ draft, onChange, errors, onValidity, resetKey, profileErrors }: SectionProps) {
  const presets = usePresets();
  const hardware = useHardwareInfo();
  const { builtIn, own } = splitPatterns(draft.ignore_patterns);
  const [patternsText, setPatternsText] = useState(own.join("\n"));
  const [lastPatterns, setLastPatterns] = useState(own.join("\n"));
  const joined = own.join("\n");
  if (joined !== lastPatterns) {
    setLastPatterns(joined);
    setPatternsText(joined);
  }
  return (
    <>
      <Block title="Ignored files" description="Files matching these are never scanned or converted.">
        <Field
          label="Your ignore patterns"
          description={[
            "One per line, relative to each library, e.g. **/Extras/** or **/*sample*.",
            builtInText(builtIn),
          ]
            .filter(Boolean)
            .join(" ")}
          error={errors.ignore_patterns}
        >
          <Textarea
            value={patternsText}
            rows={4}
            spellCheck={false}
            placeholder="**/Extras/**"
            className="font-mono text-[0.8125rem]"
            onChange={(e) => {
              setPatternsText(e.target.value);
              const patterns = e.target.value
                .split("\n")
                .map((p) => p.trim())
                .filter(Boolean);
              setLastPatterns(patterns.join("\n"));
              // The built-in rules are kept as they are.
              onChange({ ignore_patterns: [...builtIn, ...patterns.filter((p) => !builtIn.includes(p))] });
            }}
          />
        </Field>
        <Field
          label="Ignore files smaller than"
          description="New files smaller than this are skipped (samples, trailers). Files already listed stay. 0 includes every file."
          error={errors.min_file_size_mb}
        >
          <div className="flex items-center gap-2">
            <Input
              type="number"
              inputMode="numeric"
              min={0}
              value={draft.min_file_size_mb}
              onChange={(e) => {
                const n = Math.round(Number(e.target.value));
                onChange({ min_file_size_mb: Number.isFinite(n) && n > 0 ? n : 0 });
              }}
              className="w-28 font-mono"
            />
            <span className="text-sm text-muted">MB</span>
          </div>
        </Field>
      </Block>

      <section className="border-t border-line pt-7">
        <h2 className="text-[0.9375rem] font-semibold text-fg">Defaults for new libraries</h2>
        <p className="mt-1 mb-6 max-w-2xl text-[0.8125rem] text-muted">
          {newLibraryDefaultsText(
            defaultsCustomized(presets.data, draft.default_profile),
            recommendedGoal(hardware.hw),
            defaultsPresetGoal(presets.data, draft.default_profile),
          )}
        </p>
        <ProfileEditor
          profile={draft.default_profile}
          onChange={(default_profile) => onChange({ default_profile })}
          errors={profileErrors}
          presets={presets.data}
          hardware={hardware.hw}
          hardwarePending={hardware.pending}
          preference={draft.hardware}
          onValidityChange={onValidity}
          resetKey={resetKey}
        />
      </section>
    </>
  );
}

/**
 * What these defaults do, as "Add library" really applies them: untouched,
 * it suggests the goal that suits this machine instead (see `newLibraryStart`);
 * once changed, new libraries start with them, named by goal when they are
 * exactly that goal's preset ("Plays everywhere").
 */
export function newLibraryDefaultsText(
  customized: boolean,
  suggested: Exclude<Goal, "custom">,
  presetGoal: Exclude<Goal, "custom"> | null = null,
): string {
  const existing = "Existing libraries keep their own; change them in each library's Settings tab.";
  if (customized) {
    return presetGoal
      ? `New libraries start with ${GOAL_LABEL[presetGoal]}. ${existing}`
      : `New libraries start with these settings. ${existing}`;
  }
  return `Until you change these, Add library suggests the goal that suits this machine (${GOAL_LABEL[suggested]}); once you do, new libraries start with them. ${existing}`;
}

interface SectionProps {
  draft: Settings;
  onChange: (patch: Partial<Settings>) => void;
  errors: FieldErrors;
  onValidity: (valid: boolean) => void;
  /** Bumped by Discard, to clear text typed into free-text fields. */
  resetKey: number;
  /** A server error inside the default profile, under its control. */
  profileErrors: ProfileErrors;
}

function SettingsForm({ settings, section, focus }: { settings: Settings; section: SectionId; focus: string | null }) {
  const client = useQueryClient();
  const [base, setBase] = useState(settings);
  const [draft, setDraft] = useState(settings);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [profileErrors, setProfileErrors] = useState<ProfileErrors>({});
  const [valid, setValid] = useState(true);
  const [resetKey, setResetKey] = useState(0);
  // Folders picked again as they were: saved again (see `rechosenAfter`).
  const [rechosen, setRechosen] = useState<FolderSetting[]>([]);
  const changed = keysToSave(changedKeys(draft, base), rechosen);
  const dirty = changed.length > 0;

  // Adopt changes from elsewhere (another tab, the server) while nothing is edited.
  if (settings !== base && !dirty) {
    setBase(settings);
    setDraft(settings);
  }

  const onChange = (patch: Partial<Settings>) => {
    setDraft((d) => ({ ...d, ...patch }));
    setRechosen((r) => rechosenAfter(r, patch, base));
    if ("default_profile" in patch) setProfileErrors({});
    setErrors((e) => {
      const next = { ...e };
      for (const key of Object.keys(patch)) delete next[key as keyof Settings];
      delete next.general;
      return next;
    });
  };

  const save = useMutation({
    mutationFn: () => {
      const patch: Partial<Settings> = {};
      for (const key of changed) (patch as Record<string, unknown>)[key] = draft[key];
      if (patch.temp_dir === "") patch.temp_dir = null;
      return api.updateSettings(patch);
    },
    onSuccess: (next) => {
      client.setQueryData(keys.settings, next);
      void client.invalidateQueries({ queryKey: keys.queue });
      setBase(next);
      setDraft(next);
      setRechosen([]);
      setErrors({});
      setProfileErrors({});
      toast.success("Settings saved");
    },
    onError: (err) => {
      setErrors(errorsFrom(err, changed));
      const field = err instanceof ApiError ? profileFieldOf(err.field, "default_profile") : null;
      setProfileErrors(field ? { [field]: errorMessage(err) } : {});
    },
  });

  const sectionProps: SectionProps = { draft, onChange, errors, onValidity: setValid, resetKey, profileErrors };
  const bar = saveBarMessage({
    errors,
    draft,
    section,
    advancedValid: valid,
    profileFieldShown: Object.keys(profileErrors).length > 0,
  });

  return (
    <>
      {section === "processing" ? <ProcessingSection {...sectionProps} /> : null}
      {section === "output" ? <OutputSection {...sectionProps} focus={focus} /> : null}
      {section === "hardware" ? <HardwareSection draft={draft} onChange={onChange} /> : null}
      {/* Kept mounted while hidden, so text typed there (valid or not) isn't
          lost on switching sections, and invalid text keeps blocking Save. */}
      <div hidden={section !== "advanced"}>
        <AdvancedSection {...sectionProps} />
      </div>
      <SaveBar
        dirty={dirty}
        saving={save.isPending}
        onSave={() => save.mutate()}
        onDiscard={() => {
          setDraft(base);
          setRechosen([]);
          setErrors({});
          setProfileErrors({});
          setResetKey((k) => k + 1);
        }}
        error={bar.message}
        errorRole={bar.role}
        disabled={bar.blocked}
        // Moving between settings sections keeps the draft; leaving Settings doesn't.
        blocks={(target) => target.segments[0] !== "settings"}
        saveAndLeave={async () => {
          if (bar.blocked) return false;
          try {
            await save.mutateAsync();
            return true;
          } catch {
            return false;
          }
        }}
      />
    </>
  );
}

/** Version and build, and a copy of the facts a bug report needs. */
function About() {
  const system = useSystem();
  const { hw } = useHardwareInfo();
  const queue = useQueueState();
  const settings = useSettings();
  const info = system.data;
  if (!info) return null;
  const text = bugReportText({ system: info, hw, queue: queue.data, settings: settings.data });
  return (
    <section
      aria-labelledby="about-heading"
      className="mt-12 flex flex-col gap-3 border-t border-line pt-6 sm:flex-row sm:items-center sm:justify-between"
    >
      <div className="min-w-0">
        <h2 id="about-heading" className="text-sm font-semibold text-fg">
          About
        </h2>
        <p className="mt-0.5 text-[0.8125rem] text-muted">
          Chrysopoeia <span className="font-mono text-fg">{info.version}</span>
          {info.build ? (
            <>
              {" "}
              · build <span className="font-mono text-fg">{info.build}</span>
            </>
          ) : null}
        </p>
      </div>
      <CopyButton text={text} label="Copy for a bug report" className="self-start sm:self-auto" />
    </section>
  );
}

export function SettingsScreen({ route }: { route: Route }) {
  const settings = useSettings();
  const section = sectionFor(route.segments[1]);
  const current = SECTIONS.find((s) => s.id === section) ?? SECTIONS[0];

  return (
    <div>
      <PageHeader
        title="Settings"
        description={current.description}
        actions={
          <div className="flex items-center gap-3 md:hidden">
            <span className="text-[0.8125rem] text-muted">Appearance</span>
            <ThemeSwitch />
          </div>
        }
      />
      <div className="lg:grid lg:grid-cols-[11rem_1fr] lg:gap-10">
        <nav aria-label="Settings sections" className="hidden lg:block">
          <ul className="sticky top-8 flex flex-col gap-0.5">
            {SECTIONS.map((s) => (
              <li key={s.id}>
                <a
                  href={href(`/settings/${s.id}`)}
                  aria-current={s.id === section ? "page" : undefined}
                  className={cn(
                    "block rounded-md px-3 py-2 text-sm font-medium no-underline transition-colors",
                    s.id === section ? "bg-raised text-fg" : "text-muted hover:bg-raised/60 hover:text-fg",
                  )}
                >
                  {s.label}
                </a>
              </li>
            ))}
          </ul>
        </nav>
        {/* Narrow screens: every section visible at once, as wrapping pills. */}
        <nav aria-label="Settings sections" className="mb-7 lg:hidden">
          <ul className="flex flex-wrap gap-2">
            {SECTIONS.map((s) => (
              <li key={s.id}>
                <a
                  href={href(`/settings/${s.id}`)}
                  aria-current={s.id === section ? "page" : undefined}
                  className={cn(
                    "inline-flex h-9 items-center rounded-full border px-4 text-sm font-medium no-underline transition-colors pointer-coarse:h-11",
                    s.id === section
                      ? "border-accent-ink bg-accent-soft text-fg"
                      : "border-line-strong/50 bg-surface text-muted hover:border-line-strong hover:text-fg",
                  )}
                >
                  {s.label}
                </a>
              </li>
            ))}
          </ul>
        </nav>
        <div className="min-w-0">
          {settings.data ? (
            <SettingsForm settings={settings.data} section={section} focus={route.params.get("focus")} />
          ) : settings.error ? (
            <Callout tone="danger" title="Couldn't load settings">
              {errorMessage(settings.error)}
            </Callout>
          ) : (
            <div className="space-y-4">
              <Skeleton className="h-20 w-full max-w-2xl" />
              <Skeleton className="h-20 w-full max-w-2xl" />
            </div>
          )}
          <About />
        </div>
      </div>
    </div>
  );
}
