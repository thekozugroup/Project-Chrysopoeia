"use client";

import { useCallback, useRef, useEffect, useState } from "react";
import {
  Check,
  Zap,
  Cpu,
  Shield,
  ShieldOff,
  RotateCcw,
  ChevronDown,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { useAppStore } from "@/lib/store";
import type {
  OutputFormat,
  OutputAudioFormat,
  OutputContainer,
} from "@/lib/types";

// ---- Constants ----

const AUDIO_OPTIONS: { codec: OutputAudioFormat; label: string; open: boolean }[] = [
  { codec: "opus", label: "Opus", open: true },
  { codec: "flac", label: "FLAC", open: true },
  { codec: "aac", label: "AAC", open: false },
  { codec: "copy", label: "Copy", open: true },
];

const CONTAINER_OPTIONS: { value: OutputContainer; label: string }[] = [
  { value: "mkv", label: ".mkv" },
  { value: "webm", label: ".webm" },
  { value: "mp4", label: ".mp4" },
];

function getCrfDescriptor(crf: number): { label: string; color: string } {
  if (crf <= 22) return { label: "Excellent", color: "text-emerald-400" };
  if (crf <= 26) return { label: "High", color: "text-gold" };
  if (crf <= 30) return { label: "Medium", color: "text-amber-400" };
  if (crf <= 35) return { label: "Low", color: "text-orange-400" };
  return { label: "Minimum", color: "text-destructive" };
}

// ---- Toggle component ----

function Toggle({
  label,
  sublabel,
  checked,
  onChange,
}: {
  label: string;
  sublabel?: string;
  checked: boolean;
  onChange: () => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      onClick={onChange}
      className="flex w-full items-center justify-between gap-2 rounded-md py-1 focus-visible:outline-2 focus-visible:outline-ring"
    >
      <div>
        <p className="text-[11px] text-foreground">{label}</p>
        {sublabel && (
          <p className="text-[9px] text-muted-foreground/60">{sublabel}</p>
        )}
      </div>
      <div
        className={`relative h-4 w-7 shrink-0 rounded-full transition-colors ${
          checked ? "bg-gold" : "bg-secondary"
        }`}
      >
        <div
          className={`absolute top-0.5 left-0.5 h-3 w-3 rounded-full bg-background transition-transform ${
            checked ? "translate-x-3" : ""
          }`}
        />
      </div>
    </button>
  );
}

// ---- Pill selector ----

function PillSelector<T extends string>({
  options,
  value,
  onChange,
  ariaLabel,
}: {
  options: { value: T; label: string; open?: boolean }[];
  value: T;
  onChange: (v: T) => void;
  ariaLabel: string;
}) {
  return (
    <div className="flex flex-wrap gap-1" role="radiogroup" aria-label={ariaLabel}>
      {options.map((opt) => {
        const selected = value === opt.value;
        return (
          <button
            type="button"
            key={opt.value}
            role="radio"
            aria-checked={selected}
            onClick={() => onChange(opt.value)}
            className={`rounded-md px-2.5 py-1 text-[11px] font-medium transition-all focus-visible:outline-2 focus-visible:outline-ring ${
              selected
                ? "bg-gold/15 text-gold ring-2 ring-gold/40"
                : "bg-secondary/40 text-muted-foreground hover:bg-secondary/60"
            }`}
          >
            {opt.label}
            {opt.open && selected && (
              <Shield className="ml-1 inline h-2.5 w-2.5 text-emerald-400/70" />
            )}
          </button>
        );
      })}
    </div>
  );
}

// ---- Collapsible section ----

function Section({
  title,
  defaultOpen = true,
  children,
}: {
  title: string;
  defaultOpen?: boolean;
  children: React.ReactNode;
}) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <div className="border-b border-border/40 last:border-b-0">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="flex w-full items-center justify-between px-4 md:px-6 py-3 text-xs font-semibold uppercase tracking-wider text-muted-foreground hover:text-foreground transition-colors"
      >
        {title}
        <ChevronDown
          className={`h-3.5 w-3.5 transition-transform ${open ? "rotate-180" : ""}`}
        />
      </button>
      {open && (
        <div className="px-4 md:px-6 pb-4 space-y-4">{children}</div>
      )}
    </div>
  );
}

// ---- Video codec card ----

function VideoCodecSelector({
  value,
  onChange,
}: {
  value: OutputFormat;
  onChange: (v: OutputFormat) => void;
}) {
  const hardware = useAppStore((s) => s.hardware);

  return (
    <div className="space-y-1" role="radiogroup" aria-label="Video codec">
      {hardware.formats.map((fmt) => {
        const selected = value === fmt.codec;
        return (
          <button
            type="button"
            key={fmt.codec}
            role="radio"
            aria-checked={selected}
            onClick={() => onChange(fmt.codec as OutputFormat)}
            className={`flex w-full items-center gap-2.5 rounded-md px-2.5 py-2 text-left transition-all focus-visible:outline-2 focus-visible:outline-ring ${
              selected
                ? "bg-gold/15 ring-2 ring-gold/40"
                : "hover:bg-secondary/50"
            }`}
          >
            <div
              className={`flex h-4 w-4 shrink-0 items-center justify-center rounded-full border transition-colors ${
                selected ? "border-gold bg-gold" : "border-muted-foreground/30"
              }`}
            >
              {selected && (
                <Check className="h-2.5 w-2.5 text-gold-foreground" />
              )}
            </div>
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-1.5">
                <span
                  className={`text-xs font-medium ${
                    selected ? "text-foreground" : "text-muted-foreground"
                  }`}
                >
                  {fmt.label}
                </span>
                {fmt.open ? (
                  <Shield
                    className="h-3 w-3 text-emerald-400/70"
                    aria-label="Patent-free"
                  />
                ) : (
                  <ShieldOff
                    className="h-3 w-3 text-amber-400/50"
                    aria-label="Patent-encumbered"
                  />
                )}
              </div>
              <p className="text-[10px] text-muted-foreground/70 leading-tight">
                {fmt.description}
              </p>
            </div>
            <div className="shrink-0">
              {fmt.hw.encode ? (
                <Badge
                  variant="secondary"
                  className="gap-1 px-1.5 py-0 text-[10px] font-mono uppercase text-emerald-400 bg-emerald-400/10 border-emerald-400/20"
                >
                  <Zap className="h-2.5 w-2.5" />
                  {fmt.hw.api}
                </Badge>
              ) : (
                <Badge
                  variant="secondary"
                  className="gap-1 px-1.5 py-0 text-[10px] font-mono uppercase text-muted-foreground"
                >
                  <Cpu className="h-2.5 w-2.5" />
                  cpu
                </Badge>
              )}
            </div>
          </button>
        );
      })}
    </div>
  );
}

// ---- CRF Slider ----

function CrfSlider({
  value,
  onChange,
}: {
  value: number;
  onChange: (v: number) => void;
}) {
  const crfDesc = getCrfDescriptor(value);
  const pct = ((value - 18) / (40 - 18)) * 100;

  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between">
        <span className="text-[11px] text-muted-foreground">Quality (CRF)</span>
        <div className="flex items-center gap-2">
          <span className={`text-[10px] font-medium ${crfDesc.color}`}>
            {crfDesc.label}
          </span>
          <span className="text-[11px] tabular-nums font-mono text-foreground">
            {value}
          </span>
        </div>
      </div>
      <input
        type="range"
        min={18}
        max={40}
        value={value}
        aria-label={`Quality CRF value: ${value}`}
        onChange={(e) => onChange(Number(e.target.value))}
        style={{
          background: `linear-gradient(to right, var(--gold) 0%, var(--gold) ${pct}%, var(--input) ${pct}%, var(--input) 100%)`,
        }}
        className="w-full h-1.5 rounded-full appearance-none cursor-pointer
          [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:w-3 [&::-webkit-slider-thumb]:h-3 [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-gold [&::-webkit-slider-thumb]:shadow-sm
          [&::-moz-range-thumb]:w-3 [&::-moz-range-thumb]:h-3 [&::-moz-range-thumb]:rounded-full [&::-moz-range-thumb]:bg-gold [&::-moz-range-thumb]:border-0"
      />
      <div className="flex justify-between text-[9px] text-muted-foreground/50">
        <span>Higher quality</span>
        <span>Smaller files</span>
      </div>
    </div>
  );
}

// ---- Jobs Slider ----

function JobsSlider({
  value,
  onChange,
}: {
  value: number;
  onChange: (v: number) => void;
}) {
  const pct = ((value - 1) / (8 - 1)) * 100;

  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between">
        <span className="text-[11px] text-muted-foreground">Concurrent Jobs</span>
        <span className="text-[11px] tabular-nums font-mono text-foreground">
          {value}
        </span>
      </div>
      <input
        type="range"
        min={1}
        max={8}
        value={value}
        aria-label={`Concurrent jobs: ${value}`}
        onChange={(e) => onChange(Number(e.target.value))}
        style={{
          background: `linear-gradient(to right, var(--gold) 0%, var(--gold) ${pct}%, var(--input) ${pct}%, var(--input) 100%)`,
        }}
        className="w-full h-1.5 rounded-full appearance-none cursor-pointer
          [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:w-3 [&::-webkit-slider-thumb]:h-3 [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-gold [&::-webkit-slider-thumb]:shadow-sm
          [&::-moz-range-thumb]:w-3 [&::-moz-range-thumb]:h-3 [&::-moz-range-thumb]:rounded-full [&::-moz-range-thumb]:bg-gold [&::-moz-range-thumb]:border-0"
      />
      <div className="flex items-center justify-between px-[2px]">
        {Array.from({ length: 8 }, (_, i) => i + 1).map((slot) => (
          <div
            key={slot}
            className={`h-1.5 w-1.5 rounded-full transition-colors ${
              slot <= value ? "bg-gold" : "bg-muted-foreground/20"
            }`}
            title={`${slot} job${slot > 1 ? "s" : ""}`}
          />
        ))}
      </div>
    </div>
  );
}

// ---- Main Settings Panel ----

export function SettingsPanel() {
  const settingsOpen = useAppStore((s) => s.settingsOpen);
  const globalSettings = useAppStore((s) => s.globalSettings);
  const updateGlobalSettings = useAppStore((s) => s.updateGlobalSettings);
  const libraryPaths = useAppStore((s) => s.library_paths);
  const updateLibraryConfig = useAppStore((s) => s.updateLibraryConfig);

  const panelRef = useRef<HTMLDivElement>(null);
  const [height, setHeight] = useState<number | undefined>(undefined);

  // Measure inner content height for smooth animation
  useEffect(() => {
    if (!panelRef.current) return;
    if (settingsOpen) {
      const inner = panelRef.current.querySelector(
        "[data-settings-inner]",
      ) as HTMLElement;
      if (inner) {
        setHeight(inner.scrollHeight);
      }
    } else {
      setHeight(0);
    }
  }, [settingsOpen]);

  // Reset all libraries to global defaults
  const applyDefaultsToAll = useCallback(() => {
    for (const lp of libraryPaths) {
      updateLibraryConfig(lp.id, {
        output_video: globalSettings.default_video,
        output_audio: globalSettings.default_audio,
        output_container: globalSettings.default_container,
        crf: globalSettings.default_crf,
        skip_open_formats: globalSettings.default_skip_open,
      });
    }
  }, [libraryPaths, globalSettings, updateLibraryConfig]);

  // Reset one library to defaults
  const resetLibraryToDefaults = useCallback(
    (libraryId: string) => {
      updateLibraryConfig(libraryId, {
        output_video: globalSettings.default_video,
        output_audio: globalSettings.default_audio,
        output_container: globalSettings.default_container,
        crf: globalSettings.default_crf,
        skip_open_formats: globalSettings.default_skip_open,
      });
    },
    [globalSettings, updateLibraryConfig],
  );

  return (
    <div
      ref={panelRef}
      className="overflow-hidden border-b border-border bg-card/50 transition-[max-height] duration-300 ease-in-out"
      style={{ maxHeight: settingsOpen ? (height ?? "none") : 0 }}
    >
      <div data-settings-inner>
        {/* Global Defaults */}
        <Section title="Default Output Format">
          <div className="space-y-4">
            <div>
              <p className="mb-1.5 text-[10px] text-muted-foreground/80 uppercase tracking-wider font-medium">
                Video Codec
              </p>
              <VideoCodecSelector
                value={globalSettings.default_video}
                onChange={(v) => updateGlobalSettings({ default_video: v })}
              />
            </div>

            <div>
              <p className="mb-1.5 text-[10px] text-muted-foreground/80 uppercase tracking-wider font-medium">
                Audio Codec
              </p>
              <PillSelector
                options={AUDIO_OPTIONS.map((a) => ({
                  value: a.codec,
                  label: a.label,
                  open: a.open,
                }))}
                value={globalSettings.default_audio}
                onChange={(v) => updateGlobalSettings({ default_audio: v })}
                ariaLabel="Audio codec"
              />
            </div>

            <div>
              <p className="mb-1.5 text-[10px] text-muted-foreground/80 uppercase tracking-wider font-medium">
                Container
              </p>
              <PillSelector
                options={CONTAINER_OPTIONS}
                value={globalSettings.default_container}
                onChange={(v) => updateGlobalSettings({ default_container: v })}
                ariaLabel="Container format"
              />
            </div>

            <CrfSlider
              value={globalSettings.default_crf}
              onChange={(v) => updateGlobalSettings({ default_crf: v })}
            />

            <Toggle
              label="Skip open formats"
              sublabel="Don't re-encode AV1, VP9, Opus, FLAC"
              checked={globalSettings.default_skip_open}
              onChange={() =>
                updateGlobalSettings({
                  default_skip_open: !globalSettings.default_skip_open,
                })
              }
            />
          </div>
        </Section>

        {/* Per-Library Overrides */}
        <Section title="Per-Library Overrides">
          <div className="space-y-3">
            {libraryPaths.length === 0 ? (
              <p className="text-xs text-muted-foreground/60 italic">
                No libraries configured. Add libraries in the sidebar.
              </p>
            ) : (
              <>
                <div className="overflow-x-auto">
                  <table className="w-full text-xs">
                    <thead>
                      <tr className="border-b border-border/30 text-[10px] uppercase tracking-wider text-muted-foreground/60">
                        <th className="pb-2 text-left font-medium">Path</th>
                        <th className="pb-2 text-left font-medium">Video</th>
                        <th className="pb-2 text-left font-medium">Audio</th>
                        <th className="pb-2 text-left font-medium">Container</th>
                        <th className="pb-2 text-left font-medium">CRF</th>
                        <th className="pb-2 text-right font-medium" />
                      </tr>
                    </thead>
                    <tbody>
                      {libraryPaths.map((lp) => (
                        <tr
                          key={lp.id}
                          className="border-b border-border/20 last:border-b-0 transition-colors hover:bg-secondary/30"
                        >
                          <td className="py-1.5 pr-3">
                            <span className="font-mono text-[11px] text-foreground truncate block max-w-48">
                              {lp.path}
                            </span>
                          </td>
                          <td className="py-1.5 pr-2">
                            <select
                              value={lp.transcode.output_video}
                              onChange={(e) =>
                                updateLibraryConfig(lp.id, {
                                  output_video: e.target
                                    .value as OutputFormat,
                                })
                              }
                              className="h-6 rounded border border-border/50 bg-card px-1 text-[10px] text-foreground focus:outline-none focus:ring-1 focus:ring-gold/40"
                            >
                              <option value="av1">AV1</option>
                              <option value="vp9">VP9</option>
                              <option value="hevc">HEVC</option>
                              <option value="h264">H.264</option>
                            </select>
                          </td>
                          <td className="py-1.5 pr-2">
                            <select
                              value={lp.transcode.output_audio}
                              onChange={(e) =>
                                updateLibraryConfig(lp.id, {
                                  output_audio: e.target
                                    .value as OutputAudioFormat,
                                })
                              }
                              className="h-6 rounded border border-border/50 bg-card px-1 text-[10px] text-foreground focus:outline-none focus:ring-1 focus:ring-gold/40"
                            >
                              <option value="opus">Opus</option>
                              <option value="flac">FLAC</option>
                              <option value="aac">AAC</option>
                              <option value="copy">Copy</option>
                            </select>
                          </td>
                          <td className="py-1.5 pr-2">
                            <select
                              value={lp.transcode.output_container}
                              onChange={(e) =>
                                updateLibraryConfig(lp.id, {
                                  output_container: e.target
                                    .value as OutputContainer,
                                })
                              }
                              className="h-6 rounded border border-border/50 bg-card px-1 text-[10px] text-foreground focus:outline-none focus:ring-1 focus:ring-gold/40"
                            >
                              <option value="mkv">.mkv</option>
                              <option value="webm">.webm</option>
                              <option value="mp4">.mp4</option>
                            </select>
                          </td>
                          <td className="py-1.5 pr-2">
                            <input
                              type="number"
                              min={18}
                              max={40}
                              value={lp.transcode.crf}
                              onChange={(e) =>
                                updateLibraryConfig(lp.id, {
                                  crf: Number(e.target.value),
                                })
                              }
                              className="h-6 w-12 rounded border border-border/50 bg-card px-1 text-center text-[10px] tabular-nums text-foreground focus:outline-none focus:ring-1 focus:ring-gold/40"
                            />
                          </td>
                          <td className="py-1.5 text-right">
                            <Button
                              size="sm"
                              variant="outline"
                              onClick={() => resetLibraryToDefaults(lp.id)}
                              className="h-6 gap-1 px-2 text-[10px] text-muted-foreground hover:text-foreground hover:border-gold/30"
                            >
                              <RotateCcw className="h-2.5 w-2.5" />
                              Reset
                            </Button>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
                <div className="flex justify-end">
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={applyDefaultsToAll}
                    className="gap-1.5 text-[10px] border-gold/20 text-gold hover:bg-gold/10 hover:border-gold/30"
                  >
                    <RotateCcw className="h-3 w-3" />
                    Apply defaults to all
                  </Button>
                </div>
              </>
            )}
          </div>
        </Section>

        {/* Performance */}
        <Section title="Performance">
          <div className="space-y-4">
            <JobsSlider
              value={globalSettings.concurrent_jobs}
              onChange={(v) => updateGlobalSettings({ concurrent_jobs: v })}
            />
            <Toggle
              label="Auto-scan"
              sublabel="Watch folders for new files"
              checked={globalSettings.auto_scan}
              onChange={() =>
                updateGlobalSettings({ auto_scan: !globalSettings.auto_scan })
              }
            />
            <Toggle
              label="Auto-transcode"
              sublabel="Start processing new files automatically"
              checked={globalSettings.auto_transcode}
              onChange={() =>
                updateGlobalSettings({
                  auto_transcode: !globalSettings.auto_transcode,
                })
              }
            />
          </div>
        </Section>
      </div>
    </div>
  );
}
