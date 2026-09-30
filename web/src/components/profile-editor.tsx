"use client";

/**
 * Edits a transcode profile: goal, quality and speed up front; formats,
 * resolution, tracks and thresholds behind "More format options". Choices
 * that a container cannot hold are never offered, and when a change forces
 * another (e.g. WebM → MKV for HEVC) the editor says so. A saved value this
 * machine can no longer produce stays visible as "(not available here)".
 */

import { ChevronRight, Info } from "lucide-react";
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { GoalPicker } from "@/components/goal-picker";
import { Field, Input, Segmented, Select, SwitchRow } from "@/components/ui/controls";
import { codecSpeedHint } from "@/lib/hardware";
import {
  AUDIO_CODEC_LABEL,
  CONTAINER_LABEL,
  MAX_HEIGHTS,
  MIN_SAVINGS,
  QUALITY_HELP,
  QUALITY_LABEL,
  RECOMMENDED_QUALITY,
  SPEED_HELP,
  SPEED_LABEL,
  VIDEO_CODEC_HELP,
  VIDEO_CODEC_LABEL,
} from "@/lib/labels";
import {
  allVideoCodecs,
  audioCodecsFor,
  containersFor,
  normalizeProfile,
  parseLanguages,
  parseQualityOverride,
  profileWithGoal,
  qualityOverrideMax,
} from "@/lib/profile";
import type {
  AudioCodec,
  Container,
  Goal,
  HardwareInfo,
  HwPreference,
  Presets,
  QualityLevel,
  SpeedPreset,
  TranscodeProfile,
  VideoCodec,
} from "@/lib/types";
import { QUALITY_LEVELS, SPEED_PRESETS } from "@/lib/types";
import type { ProfileErrors } from "@/lib/settings-form";
import { cn } from "@/lib/utils";

/** Fields shown under "More format options", which opens to show their errors. */
const MORE_FIELDS: readonly (keyof TranscodeProfile)[] = [
  "video_codec",
  "container",
  "audio_codec",
  "max_height",
  "min_savings_pct",
  "audio_languages",
  "subtitle_languages",
  "subtitles",
  "quality_override",
  "skip_efficient",
];

/** An error under a control that isn't a `Field` (choice rows, switches). */
function FieldError({ message }: { message: string | undefined }) {
  if (!message) return null;
  return (
    <p role="alert" className="mt-1.5 text-[0.8125rem] leading-snug font-medium text-danger">
      {message}
    </p>
  );
}

/** The option for a saved value this machine doesn't offer any more. */
function Unavailable({ value, label }: { value: string; label: string }) {
  return <option value={value}>{label} (not available here)</option>;
}

interface ProfileEditorProps {
  profile: TranscodeProfile;
  onChange: (profile: TranscodeProfile) => void;
  presets: Presets | undefined;
  hardware: HardwareInfo | undefined;
  hardwarePending?: boolean;
  preference?: HwPreference;
  /** Reports whether any field currently holds invalid input. */
  onValidityChange?: (valid: boolean) => void;
  /**
   * Change this (e.g. a counter bumped by Discard) to throw away text typed
   * into fields that never became a valid change.
   */
  resetKey?: number;
  /** Heading level of the group titles, to fit the page's outline. */
  headingLevel?: 2 | 3;
  /** Server errors by field (`profile.quality` → `quality`), shown under their control. */
  errors?: ProfileErrors;
}

function Group({
  title,
  description,
  children,
  level,
}: {
  title: string;
  description?: ReactNode;
  children: ReactNode;
  level: 2 | 3;
}) {
  const Heading = level === 2 ? "h2" : "h3";
  return (
    <section className="grid gap-3 border-t border-line py-6 first:border-t-0 first:pt-0 md:grid-cols-[14rem_1fr] md:gap-8">
      <div>
        <Heading className="text-sm font-semibold text-fg">{title}</Heading>
        {description ? <p className="mt-1 text-[0.8125rem] leading-snug text-muted">{description}</p> : null}
      </div>
      <div className="min-w-0">{children}</div>
    </section>
  );
}

/** Calls `report` whenever `valid` changes (and once on mount). */
function useReportValidity(valid: boolean, report: (valid: boolean) => void): void {
  const latest = useRef(report);
  useEffect(() => {
    latest.current = report;
  });
  useEffect(() => {
    latest.current(valid);
  }, [valid]);
}

function LanguageField({
  label,
  description,
  value,
  onChange,
  onValidity,
  serverError,
}: {
  label: string;
  description: string;
  value: string[];
  onChange: (codes: string[]) => void;
  onValidity: (valid: boolean) => void;
  serverError?: string;
}) {
  const joined = value.join(", ");
  const [text, setText] = useState(joined);
  const [error, setError] = useState<string | null>(null);
  const [lastExternal, setLastExternal] = useState(joined);

  // The profile was replaced from outside (discard, goal change): follow it.
  if (joined !== lastExternal) {
    setLastExternal(joined);
    setText(joined);
    setError(null);
  }

  // Validity follows the error, so a reset from outside clears it too.
  useReportValidity(error === null, onValidity);

  return (
    <Field label={label} description={description} error={error ?? serverError}>
      <Input
        value={text}
        placeholder="All languages"
        spellCheck={false}
        autoCapitalize="off"
        autoCorrect="off"
        onChange={(e) => {
          const next = e.target.value;
          setText(next);
          const parsed = parseLanguages(next);
          setError(parsed.error);
          if (!parsed.error) {
            setLastExternal(parsed.codes.join(", "));
            onChange(parsed.codes);
          }
        }}
        className="font-mono text-[0.8125rem]"
      />
    </Field>
  );
}

/** Raw encoder quality, checked against the range the codec's encoders accept. */
function QualityOverrideField({
  value,
  codec,
  onChange,
  onValidity,
  serverError,
}: {
  value: number | null;
  codec: VideoCodec;
  onChange: (value: number | null) => void;
  onValidity: (valid: boolean) => void;
  serverError?: string;
}) {
  const external = value === null ? "" : String(value);
  const [text, setText] = useState(external);
  const [lastExternal, setLastExternal] = useState(external);
  const [lastCodec, setLastCodec] = useState(codec);
  if (external !== lastExternal || codec !== lastCodec) {
    setLastExternal(external);
    setLastCodec(codec);
    setText(external);
  }
  const error = parseQualityOverride(text, codec).error;
  useReportValidity(error === null, onValidity);
  const max = qualityOverrideMax(codec);
  return (
    <Field
      label="Encoder quality value"
      description={`Raw CRF / CQ / QP, 0 to ${max} for ${VIDEO_CODEC_LABEL[codec]}. Overrides Quality. Lower means higher quality.`}
      error={error ?? serverError}
    >
      <Input
        type="text"
        inputMode="numeric"
        placeholder="Automatic"
        value={text}
        onChange={(e) => {
          const next = e.target.value;
          setText(next);
          const parsed = parseQualityOverride(next, codec);
          if (!parsed.error) {
            setLastExternal(parsed.value === null ? "" : String(parsed.value));
            onChange(parsed.value);
          }
        }}
        className="max-w-40 font-mono"
      />
    </Field>
  );
}

export function ProfileEditor({
  profile,
  onChange,
  presets,
  hardware,
  hardwarePending,
  preference = "auto",
  onValidityChange,
  resetKey = 0,
  headingLevel = 3,
  errors = {},
}: ProfileEditorProps) {
  const [notes, setNotes] = useState<string[]>([]);
  const [invalid, setInvalid] = useState<Record<string, boolean>>({});
  const [advancedOpen, setAdvancedOpen] = useState(profile.goal === "custom");
  const [lastReset, setLastReset] = useState(resetKey);
  const advancedId = useId();
  // A server error about a field in "More format options" opens it.
  const hiddenError = MORE_FIELDS.some((f) => errors[f]);
  const [openedFor, setOpenedFor] = useState(false);
  if (hiddenError !== openedFor) {
    setOpenedFor(hiddenError);
    if (hiddenError) setAdvancedOpen(true);
  }

  // Discard: the free-text fields remount (below, keyed by resetKey) and
  // report themselves valid again; forget their old verdicts now.
  if (resetKey !== lastReset) {
    setLastReset(resetKey);
    setInvalid({});
    setNotes([]);
  }

  useEffect(() => {
    onValidityChange?.(!Object.values(invalid).some(Boolean));
  }, [invalid, onValidityChange]);

  const setValidity = (key: string) => (valid: boolean) =>
    setInvalid((prev) => (prev[key] === !valid ? prev : { ...prev, [key]: !valid }));

  const update = (patch: Partial<TranscodeProfile>, makeCustom = false) => {
    const merged: TranscodeProfile = { ...profile, ...patch, goal: makeCustom ? "custom" : profile.goal };
    const { profile: fixed, notes: changeNotes } = normalizeProfile(presets, merged);
    setNotes(changeNotes);
    onChange(fixed);
  };

  const pickGoal = (goal: Exclude<Goal, "custom">) => {
    setNotes([]);
    // Track and resolution choices are about the library, not the goal.
    onChange(profileWithGoal(presets, goal, profile));
  };

  const videoCodecs = allVideoCodecs(presets);
  const containers = containersFor(presets, profile.video_codec);
  const audioCodecs = audioCodecsFor(presets, profile.container);
  const hint = hardware ? codecSpeedHint(hardware, profile.video_codec, preference) : null;

  return (
    <div>
      <Group level={headingLevel} title="Goal" description="What matters most. You can change it any time.">
        <GoalPicker
          label="Goal"
          value={profile.goal}
          onChange={pickGoal}
          presets={presets}
          hardware={hardware}
          hardwarePending={hardwarePending}
          preference={preference}
          profile={profile}
        />
        <FieldError message={errors.goal} />
      </Group>

      <Group
        level={headingLevel}
        title="Quality"
        description={`How closely the new file matches the original. ${QUALITY_LABEL[RECOMMENDED_QUALITY]} suits most libraries.`}
      >
        <Segmented<QualityLevel>
          label="Quality"
          value={profile.quality}
          onChange={(quality) => update({ quality })}
          options={QUALITY_LEVELS.map((q) => ({
            value: q,
            label: QUALITY_LABEL[q],
            ariaLabel: q === RECOMMENDED_QUALITY ? `${QUALITY_LABEL[q]} (recommended)` : undefined,
          }))}
          stackOnPhones
        />
        <div className="mt-2 hidden justify-between text-xs text-muted sm:flex" aria-hidden>
          <span>Smaller files</span>
          <span>Closer to the original</span>
        </div>
        <p className="mt-2 text-[0.8125rem] text-fg/85">{QUALITY_HELP[profile.quality]}</p>
        <FieldError message={errors.quality} />
      </Group>

      <Group level={headingLevel} title="Speed" description="Encoder effort. Slower settings squeeze files a little more.">
        <Segmented<SpeedPreset>
          label="Speed"
          value={profile.speed}
          onChange={(speed) => update({ speed })}
          options={SPEED_PRESETS.map((s) => ({ value: s, label: SPEED_LABEL[s] }))}
          className="sm:max-w-md"
        />
        <p className="mt-2 text-[0.8125rem] text-fg/85">{SPEED_HELP[profile.speed]}</p>
        <FieldError message={errors.speed} />
      </Group>

      <section className="border-t border-line pt-4">
        <button
          type="button"
          aria-expanded={advancedOpen}
          aria-controls={advancedId}
          onClick={() => setAdvancedOpen((v) => !v)}
          className="flex w-full items-center gap-2 rounded-md py-2 text-left text-sm font-semibold text-fg hover:text-accent-ink"
        >
          <ChevronRight
            className={cn("size-4 text-muted transition-transform duration-200", advancedOpen && "rotate-90")}
            aria-hidden
          />
          More format options
          <span className="hidden font-normal text-muted sm:inline">Formats, resolution, tracks and thresholds</span>
        </button>

        <div id={advancedId} hidden={!advancedOpen} className="pt-4">
          {notes.length ? (
            <div role="status" className="mb-5 flex gap-2 rounded-md bg-info-soft px-3 py-2.5 text-[0.8125rem] text-fg">
              <Info className="mt-0.5 size-4 shrink-0 text-info" aria-hidden />
              <div>
                {notes.map((note) => (
                  <p key={note}>{note}</p>
                ))}
              </div>
            </div>
          ) : null}

          <div className="grid gap-5 sm:grid-cols-3">
            <Field
              label="Video format"
              description={
                <>
                  {VIDEO_CODEC_HELP[profile.video_codec]}
                  {hint ? <span className="mt-1 block">{hint.text}</span> : null}
                </>
              }
              error={errors.video_codec}
            >
              <Select
                value={profile.video_codec}
                onChange={(e) => update({ video_codec: e.target.value as VideoCodec }, true)}
              >
                {videoCodecs.includes(profile.video_codec) ? null : (
                  <Unavailable value={profile.video_codec} label={VIDEO_CODEC_LABEL[profile.video_codec]} />
                )}
                {videoCodecs.map((codec) => {
                  const preset = presets?.video_codecs.find((v) => v.codec === codec);
                  return (
                    <option key={codec} value={codec}>
                      {VIDEO_CODEC_LABEL[codec]}
                      {preset?.hw_accelerated ? " (GPU)" : ""}
                    </option>
                  );
                })}
              </Select>
            </Field>
            <Field
              label="Container"
              description="MKV holds every kind of track. MP4 suits Apple devices."
              error={errors.container}
            >
              <Select
                value={profile.container}
                onChange={(e) => update({ container: e.target.value as Container }, true)}
              >
                {containers.includes(profile.container) ? null : (
                  <Unavailable value={profile.container} label={CONTAINER_LABEL[profile.container]} />
                )}
                {containers.map((c) => (
                  <option key={c} value={c}>
                    {presets?.containers.find((p) => p.container === c)?.label ?? CONTAINER_LABEL[c]}
                  </option>
                ))}
              </Select>
            </Field>
            <Field
              label="Audio"
              description="Keep original copies every track untouched where it fits."
              error={errors.audio_codec}
            >
              <Select
                value={profile.audio_codec}
                onChange={(e) => update({ audio_codec: e.target.value as AudioCodec }, true)}
              >
                {audioCodecs.includes(profile.audio_codec) ? null : (
                  <Unavailable value={profile.audio_codec} label={AUDIO_CODEC_LABEL[profile.audio_codec]} />
                )}
                {audioCodecs.map((a) => (
                  <option key={a} value={a}>
                    {presets?.audio_codecs.find((p) => p.codec === a)?.label ?? AUDIO_CODEC_LABEL[a]}
                  </option>
                ))}
              </Select>
            </Field>
          </div>

          <div className="mt-6 grid gap-5 sm:grid-cols-2">
            <Field
              label="Limit resolution"
              description="Larger videos are scaled down. Smaller ones are never scaled up."
              error={errors.max_height}
            >
              <Select
                value={profile.max_height === null ? "" : String(profile.max_height)}
                onChange={(e) => update({ max_height: e.target.value ? Number(e.target.value) : null })}
              >
                {MAX_HEIGHTS.map((h) => (
                  <option key={h.label} value={h.value === null ? "" : String(h.value)}>
                    {h.label}
                  </option>
                ))}
              </Select>
            </Field>
            <Field
              label="Keep the result only if it's smaller"
              description="Otherwise the original stays and the file is marked skipped."
              error={errors.min_savings_pct}
            >
              <Select
                value={profile.min_savings_pct === null ? "" : String(profile.min_savings_pct)}
                onChange={(e) => update({ min_savings_pct: e.target.value ? Number(e.target.value) : null })}
              >
                {MIN_SAVINGS.some((m) => m.value === profile.min_savings_pct) ? null : (
                  <option value={String(profile.min_savings_pct)}>At least {profile.min_savings_pct}% smaller</option>
                )}
                {MIN_SAVINGS.map((m) => (
                  <option key={m.label} value={m.value === null ? "" : String(m.value)}>
                    {m.label}
                  </option>
                ))}
              </Select>
            </Field>
          </div>

          <div className="mt-6 grid gap-5 sm:grid-cols-2">
            <LanguageField
              key={`audio-${resetKey}`}
              label="Audio languages to keep"
              description="Codes like eng, jpn. Empty keeps all. Untagged tracks and at least one track are always kept."
              value={profile.audio_languages}
              onChange={(audio_languages) => update({ audio_languages })}
              onValidity={setValidity("audio_languages")}
              serverError={errors.audio_languages}
            />
            <LanguageField
              key={`subs-${resetKey}`}
              label="Subtitle languages to keep"
              description="Codes like eng, spa. Empty keeps all subtitles."
              value={profile.subtitle_languages}
              onChange={(subtitle_languages) => update({ subtitle_languages })}
              onValidity={setValidity("subtitle_languages")}
              serverError={errors.subtitle_languages}
            />
          </div>

          <div className="mt-6 grid gap-5 sm:grid-cols-2">
            <div className="flex flex-col gap-1.5">
              <span id={`${advancedId}-subs`} className="text-sm font-medium text-fg">
                Subtitles
              </span>
              <Segmented
                label="Subtitles"
                value={profile.subtitles}
                onChange={(subtitles) => update({ subtitles })}
                options={[
                  { value: "keep", label: "Keep" },
                  { value: "drop", label: "Remove" },
                ]}
              />
              <p className="text-[0.8125rem] leading-snug text-muted">
                {profile.subtitles === "keep"
                  ? "Text subtitles are converted when the container needs it. Picture subtitles MP4 can't hold are dropped."
                  : "Every subtitle track is removed."}
              </p>
              <FieldError message={errors.subtitles} />
            </div>
            <QualityOverrideField
              key={`quality-${resetKey}`}
              value={profile.quality_override}
              codec={profile.video_codec}
              onChange={(quality_override) => update({ quality_override })}
              onValidity={setValidity("quality_override")}
              serverError={errors.quality_override}
            />
          </div>

          <SwitchRow
            className="mt-6"
            label="Skip files that are already efficient"
            description="For example, leave AV1 files alone when the goal is HEVC. When off, only files already in exactly this format are skipped."
            checked={profile.skip_efficient}
            onCheckedChange={(skip_efficient) => update({ skip_efficient })}
          />
          <FieldError message={errors.skip_efficient} />
        </div>
      </section>
    </div>
  );
}
