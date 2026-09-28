"use client";

/**
 * Settings, in five plain sections. Edits collect in a draft shared by all
 * sections; the save bar sends only what changed, and API errors land on the
 * field they are about.
 */

import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Minus, Plus } from "lucide-react";
import { useId, useState, type ReactNode } from "react";
import { toast } from "sonner";
import { FolderField } from "@/components/folder-field";
import { ProfileEditor } from "@/components/profile-editor";
import { SaveBar } from "@/components/save-bar";
import { PageHeader, ThemeSwitch } from "@/components/shell";
import { Button } from "@/components/ui/button";
import { ChoiceCard, Field, Input, Select, SwitchRow, Textarea } from "@/components/ui/controls";
import { Badge, Callout, Skeleton } from "@/components/ui/display";
import { NavTabs } from "@/components/ui/nav-tabs";
import { api, errorMessage } from "@/lib/api";
import { formatHour } from "@/lib/format";
import { VALIDATION_HELP, VALIDATION_LABEL } from "@/lib/labels";
import { keys, useHardwareInfo, usePresets, useSettings } from "@/lib/queries";
import { href, type Route } from "@/lib/router";
import { changedKeys, errorsFrom, type FieldErrors } from "@/lib/settings-form";
import type { Settings, ValidationLevel } from "@/lib/types";
import { cn } from "@/lib/utils";
import { HardwareSection } from "./settings-hardware";

type SectionId = "processing" | "output" | "verification" | "hardware" | "advanced";

const SECTIONS: { id: SectionId; label: string; description: string }[] = [
  { id: "processing", label: "Processing", description: "How many files at once, and when." },
  { id: "output", label: "Output", description: "Where finished files go." },
  { id: "verification", label: "Verification", description: "How carefully each result is checked." },
  { id: "hardware", label: "Hardware", description: "Your CPU, GPU and which encoders work." },
  { id: "advanced", label: "Advanced", description: "Ignored files and defaults for new libraries." },
];

/** Which section shows each setting, to point at it from the save bar. */
const SECTION_OF: Partial<Record<keyof Settings, SectionId>> = {
  max_jobs: "processing",
  active_hours: "processing",
  low_priority: "processing",
  auto_queue: "processing",
  watch_folders: "processing",
  rescan_interval_hours: "processing",
  output_mode: "output",
  output_folder: "output",
  keep_file_dates: "output",
  temp_dir: "output",
  validation: "verification",
  hardware: "hardware",
  cpu_fallback: "hardware",
  ignore_patterns: "advanced",
  min_file_size_mb: "advanced",
  default_profile: "advanced",
};

function Block({ title, description, children }: { title: string; description?: ReactNode; children: ReactNode }) {
  return (
    <section className="grid gap-4 border-t border-line py-7 first:border-t-0 first:pt-0 lg:grid-cols-[15rem_1fr] lg:gap-10">
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
    <div className="inline-flex items-center rounded-md border border-line-strong/70 bg-surface shadow-card" role="group" aria-label={label}>
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
  const name = useId();
  const auto = hw?.recommended_jobs;
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
            title={auto ? `Automatic (${auto.total})` : "Automatic"}
            description={auto ? auto.reason : "Chosen from your CPU cores, memory and GPU."}
            badge={<Badge tone="accent">Recommended</Badge>}
          />
          <ChoiceCard
            name={name}
            value="manual"
            checked={draft.max_jobs !== null}
            onChange={() => onChange({ max_jobs: draft.max_jobs ?? auto?.total ?? 2 })}
            title="Choose a number"
            description="More isn't always faster: GPUs limit parallel sessions, and CPUs share their cores."
          >
            {draft.max_jobs !== null ? (
              <span className="mt-3 flex items-center gap-3">
                <Stepper value={draft.max_jobs} onChange={(max_jobs) => onChange({ max_jobs })} min={1} max={32} label="Files at once" />
                {auto && draft.max_jobs > auto.total * 2 ? (
                  <span className="text-[0.8125rem] text-warning">Much more than recommended ({auto.total}).</span>
                ) : null}
              </span>
            ) : null}
          </ChoiceCard>
          {errors.max_jobs ? <p role="alert" className="text-[0.8125rem] font-medium text-danger">{errors.max_jobs}</p> : null}
        </fieldset>
      </Block>

      <Block title="When to convert" description="Changes take effect right away. Running files always finish.">
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

function OutputSection({ draft, onChange, errors }: SectionProps) {
  const name = useId();
  const tempName = useId();
  return (
    <>
      <Block title="Finished files" description="Nothing is written anywhere until the new file has passed its checks.">
        <fieldset className="flex flex-col gap-3">
          <legend className="sr-only">Where finished files go</legend>
          <ChoiceCard
            name={name}
            value="replace"
            checked={draft.output_mode === "replace"}
            onChange={() => onChange({ output_mode: "replace" })}
            title="Replace the original"
            description="The new file takes the original's place, keeping your media server's library tidy."
          />
          <ChoiceCard
            name={name}
            value="folder"
            checked={draft.output_mode === "folder"}
            onChange={() => onChange({ output_mode: "folder" })}
            title="Save to a separate folder"
            description="Originals stay untouched. New files mirror each library's folder structure."
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

      <Block
        title="Work folder"
        description="Where files are written while they're being converted. A fast SSD or cache pool speeds things up."
      >
        <fieldset className="flex flex-col gap-3">
          <legend className="sr-only">Work folder</legend>
          <ChoiceCard
            name={tempName}
            value="next"
            checked={draft.temp_dir === null}
            onChange={() => onChange({ temp_dir: null })}
            title="Automatic"
            description="The server's work folder when it has one (the Docker image uses /temp when it's mapped), otherwise next to each file, which needs free space on the same drive as the video."
          />
          <ChoiceCard
            name={tempName}
            value="folder"
            checked={draft.temp_dir !== null}
            onChange={() => onChange({ temp_dir: draft.temp_dir ?? "" })}
            title="A specific folder"
            description="On Unraid, map /temp to a folder on your cache pool and choose it here."
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

const LEVELS: ValidationLevel[] = ["standard", "thorough", "quick", "off"];

function VerificationSection({ draft, onChange }: SectionProps) {
  const name = useId();
  return (
    <Block
      title="Checks before replacing"
      description="Each finished file is compared with its original. If a check fails, the original is kept and you're told why."
    >
      <fieldset className="flex flex-col gap-3">
        <legend className="sr-only">Verification level</legend>
        {LEVELS.map((level) => (
          <ChoiceCard
            key={level}
            name={name}
            value={level}
            checked={draft.validation === level}
            onChange={() => onChange({ validation: level })}
            title={VALIDATION_LABEL[level]}
            description={VALIDATION_HELP[level]}
            badge={
              level === "standard" ? (
                <Badge tone="accent">Recommended</Badge>
              ) : level === "off" ? (
                <Badge tone="warning">Risky</Badge>
              ) : null
            }
          />
        ))}
      </fieldset>
      {draft.validation === "off" && draft.output_mode === "replace" ? (
        <Callout tone="warning" title="Originals will be replaced without any checks">
          A file damaged during conversion would replace a good one. Keep at least Quick unless you have backups.
        </Callout>
      ) : null}
    </Block>
  );
}

function AdvancedSection({ draft, onChange, errors, onValidity, resetKey }: SectionProps) {
  const presets = usePresets();
  const hardware = useHardwareInfo();
  const [patternsText, setPatternsText] = useState(draft.ignore_patterns.join("\n"));
  const [lastPatterns, setLastPatterns] = useState(draft.ignore_patterns.join("\n"));
  const joined = draft.ignore_patterns.join("\n");
  if (joined !== lastPatterns) {
    setLastPatterns(joined);
    setPatternsText(joined);
  }
  return (
    <>
      <Block title="Ignored files" description="Files matching these are never scanned or converted.">
        <Field
          label="Ignore patterns"
          description="One per line, relative to each library, e.g. **/Extras/** or **/*sample*. Hidden files and NAS system folders are ignored by default."
          error={errors.ignore_patterns}
        >
          <Textarea
            value={patternsText}
            rows={6}
            spellCheck={false}
            className="font-mono text-[0.8125rem]"
            onChange={(e) => {
              setPatternsText(e.target.value);
              const patterns = e.target.value
                .split("\n")
                .map((p) => p.trim())
                .filter(Boolean);
              setLastPatterns(patterns.join("\n"));
              onChange({ ignore_patterns: patterns });
            }}
          />
        </Field>
        <Field
          label="Ignore files smaller than"
          description="Skips samples and trailers. 0 includes every file."
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
          New libraries start with these settings. Existing libraries keep their own; change them in each
          library&apos;s Settings tab.
        </p>
        <ProfileEditor
          profile={draft.default_profile}
          onChange={(default_profile) => onChange({ default_profile })}
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

interface SectionProps {
  draft: Settings;
  onChange: (patch: Partial<Settings>) => void;
  errors: FieldErrors;
  onValidity: (valid: boolean) => void;
  /** Bumped by Discard, to clear text typed into free-text fields. */
  resetKey: number;
}

function SettingsForm({ settings, section }: { settings: Settings; section: SectionId }) {
  const client = useQueryClient();
  const [base, setBase] = useState(settings);
  const [draft, setDraft] = useState(settings);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [valid, setValid] = useState(true);
  const [resetKey, setResetKey] = useState(0);
  const changed = changedKeys(draft, base);
  const dirty = changed.length > 0;

  // Adopt changes from elsewhere (another tab, the server) while nothing is edited.
  if (settings !== base && !dirty) {
    setBase(settings);
    setDraft(settings);
  }

  const onChange = (patch: Partial<Settings>) => {
    setDraft((d) => ({ ...d, ...patch }));
    setErrors((e) => {
      const next = { ...e };
      for (const key of Object.keys(patch)) delete next[key as keyof Settings];
      delete next.general;
      return next;
    });
  };

  const missingFolder =
    (draft.output_mode === "folder" && !draft.output_folder) || (draft.temp_dir !== null && draft.temp_dir === "");

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
      setErrors({});
      toast.success("Settings saved");
    },
    onError: (err) => setErrors(errorsFrom(err, changed)),
  });

  const sectionProps: SectionProps = { draft, onChange, errors, onValidity: setValid, resetKey };
  // Only the Advanced section has free-text fields that can be invalid.
  const fieldsValid = section !== "advanced" || valid;
  const blocked = missingFolder ? "Choose a folder to save." : !fieldsValid ? "Fix the highlighted fields to save." : null;

  // A field error names its section when that section isn't on screen.
  const [firstField, firstMessage] =
    (Object.entries(errors).find(([key]) => key !== "general") as [keyof Settings, string] | undefined) ?? [];
  const fieldSection = firstField ? SECTION_OF[firstField] : undefined;
  const fieldError = firstMessage
    ? fieldSection && fieldSection !== section
      ? `${firstMessage} (${SECTIONS.find((x) => x.id === fieldSection)?.label ?? "another section"})`
      : firstMessage
    : null;

  return (
    <>
      {section === "processing" ? <ProcessingSection {...sectionProps} /> : null}
      {section === "output" ? <OutputSection {...sectionProps} /> : null}
      {section === "verification" ? <VerificationSection {...sectionProps} /> : null}
      {section === "hardware" ? <HardwareSection draft={draft} onChange={onChange} /> : null}
      {section === "advanced" ? <AdvancedSection {...sectionProps} /> : null}
      <SaveBar
        dirty={dirty}
        saving={save.isPending}
        onSave={() => save.mutate()}
        onDiscard={() => {
          setDraft(base);
          setErrors({});
          setResetKey((k) => k + 1);
        }}
        error={errors.general ?? fieldError ?? blocked}
        disabled={Boolean(blocked)}
        // Moving between settings sections keeps the draft; leaving Settings doesn't.
        blocks={(target) => target.segments[0] !== "settings"}
        saveAndLeave={async () => {
          if (blocked) return false;
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

export function SettingsScreen({ route }: { route: Route }) {
  const settings = useSettings();
  const requested = route.segments[1] as SectionId | undefined;
  const section: SectionId = SECTIONS.some((s) => s.id === requested) ? (requested as SectionId) : "processing";
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
        <NavTabs
          label="Settings sections"
          className="mb-7 lg:hidden"
          tabs={SECTIONS.map((s) => ({ href: href(`/settings/${s.id}`), label: s.label, active: s.id === section }))}
        />
        <div className="min-w-0">
          {settings.data ? (
            <SettingsForm settings={settings.data} section={section} />
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
        </div>
      </div>
    </div>
  );
}
