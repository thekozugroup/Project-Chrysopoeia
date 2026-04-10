"use client";

import { useCallback, useState } from "react";
import {
  Check,
  Zap,
  Cpu,
  Shield,
  ShieldOff,
  RotateCcw,
  ChevronDown,
  Trash2,
  AlertTriangle,
  Monitor,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  CardDescription,
} from "@/components/ui/card";
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

const SCAN_INTERVALS = [
  { value: "1h", label: "Every 1 hour" },
  { value: "6h", label: "Every 6 hours" },
  { value: "12h", label: "Every 12 hours" },
  { value: "24h", label: "Every 24 hours" },
  { value: "manual", label: "Manual only" },
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
      className="flex w-full items-center justify-between gap-2 rounded-md py-1.5 focus-visible:outline-2 focus-visible:outline-ring"
    >
      <div>
        <p className="text-sm text-foreground">{label}</p>
        {sublabel && (
          <p className="text-xs text-muted-foreground/60">{sublabel}</p>
        )}
      </div>
      <div
        className={`relative h-5 w-9 shrink-0 rounded-full transition-colors ${
          checked ? "bg-gold" : "bg-secondary"
        }`}
      >
        <div
          className={`absolute top-0.5 left-0.5 h-4 w-4 rounded-full bg-background transition-transform ${
            checked ? "translate-x-4" : ""
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
    <div className="flex flex-wrap gap-1.5" role="radiogroup" aria-label={ariaLabel}>
      {options.map((opt) => {
        const selected = value === opt.value;
        return (
          <button
            type="button"
            key={opt.value}
            role="radio"
            aria-checked={selected}
            onClick={() => onChange(opt.value)}
            className={`rounded-lg px-3 py-1.5 text-xs font-medium transition-all focus-visible:outline-2 focus-visible:outline-ring ${
              selected
                ? "bg-gold/15 text-gold ring-2 ring-gold/40"
                : "bg-secondary/40 text-muted-foreground hover:bg-secondary/60"
            }`}
          >
            {opt.label}
            {opt.open && selected && (
              <Shield className="ml-1 inline h-3 w-3 text-emerald-400/70" />
            )}
          </button>
        );
      })}
    </div>
  );
}

// ---- Collapsible section (used for danger zone) ----

function CollapsibleSection({
  title,
  icon,
  defaultOpen = true,
  children,
}: {
  title: string;
  icon?: React.ReactNode;
  defaultOpen?: boolean;
  children: React.ReactNode;
}) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <div>
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="flex w-full items-center justify-between py-2 text-sm font-medium text-muted-foreground hover:text-foreground transition-colors"
      >
        <span className="flex items-center gap-2">
          {icon}
          {title}
        </span>
        <ChevronDown
          className={`h-3.5 w-3.5 transition-transform ${open ? "rotate-180" : ""}`}
        />
      </button>
      {open && <div className="pt-2">{children}</div>}
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
    <div className="space-y-2" role="radiogroup" aria-label="Video codec">
      {hardware.formats.map((fmt) => {
        const selected = value === fmt.codec;
        return (
          <button
            type="button"
            key={fmt.codec}
            role="radio"
            aria-checked={selected}
            onClick={() => onChange(fmt.codec as OutputFormat)}
            className={`flex w-full items-center gap-3 rounded-lg border px-4 py-3 text-left transition-all focus-visible:outline-2 focus-visible:outline-ring ${
              selected
                ? "border-gold/40 bg-gold/10 ring-1 ring-gold/30"
                : "border-border/60 bg-card hover:border-border hover:bg-secondary/30"
            }`}
          >
            <div
              className={`flex h-4 w-4 shrink-0 items-center justify-center rounded-full border-2 transition-colors ${
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
                  className={`text-sm font-medium ${
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
              <p className="text-[11px] text-muted-foreground/70 leading-tight">
                {fmt.description}
              </p>
            </div>
            <div className="shrink-0">
              {fmt.hw.encode ? (
                <Badge
                  variant="secondary"
                  className="gap-1 px-2 py-0.5 text-[10px] font-mono uppercase text-emerald-400 bg-emerald-400/10 border-emerald-400/20"
                >
                  <Zap className="h-2.5 w-2.5" />
                  {fmt.hw.api}
                </Badge>
              ) : (
                <Badge
                  variant="secondary"
                  className="gap-1 px-2 py-0.5 text-[10px] font-mono uppercase text-muted-foreground"
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
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <span className="text-sm text-muted-foreground">Quality (CRF)</span>
        <div className="flex items-baseline gap-2">
          <span className={`font-heading text-lg font-medium ${crfDesc.color}`}>
            {crfDesc.label}
          </span>
          <span className="text-sm tabular-nums font-mono text-foreground">
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
          [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:w-3.5 [&::-webkit-slider-thumb]:h-3.5 [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-gold [&::-webkit-slider-thumb]:shadow-sm
          [&::-moz-range-thumb]:w-3.5 [&::-moz-range-thumb]:h-3.5 [&::-moz-range-thumb]:rounded-full [&::-moz-range-thumb]:bg-gold [&::-moz-range-thumb]:border-0"
      />
      <div className="flex justify-between text-[10px] text-muted-foreground/50">
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
  recommended,
}: {
  value: number;
  onChange: (v: number) => void;
  recommended: number;
}) {
  const pct = ((value - 1) / (8 - 1)) * 100;

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <span className="text-sm text-muted-foreground">Concurrent Jobs</span>
        <span className="text-sm tabular-nums font-mono text-foreground">
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
          [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:w-3.5 [&::-webkit-slider-thumb]:h-3.5 [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-gold [&::-webkit-slider-thumb]:shadow-sm
          [&::-moz-range-thumb]:w-3.5 [&::-moz-range-thumb]:h-3.5 [&::-moz-range-thumb]:rounded-full [&::-moz-range-thumb]:bg-gold [&::-moz-range-thumb]:border-0"
      />
      <div className="flex items-center justify-between px-[2px]">
        {Array.from({ length: 8 }, (_, i) => i + 1).map((slot) => (
          <div
            key={slot}
            className={`h-2 w-2 rounded-full transition-colors ${
              slot <= value ? "bg-gold" : "bg-muted-foreground/20"
            }`}
            title={`${slot} job${slot > 1 ? "s" : ""}`}
          />
        ))}
      </div>
      <p className="text-[10px] text-muted-foreground/50">
        Recommended: {recommended} concurrent jobs based on your hardware
      </p>
    </div>
  );
}

// ---- Main Library Settings Tab ----

export function LibrarySettingsTab() {
  const hardware = useAppStore((s) => s.hardware);
  const globalSettings = useAppStore((s) => s.globalSettings);
  const updateGlobalSettings = useAppStore((s) => s.updateGlobalSettings);
  const selectedLibraryId = useAppStore((s) => s.selectedLibraryId);
  const libraryPaths = useAppStore((s) => s.library_paths);
  const updateLibraryConfig = useAppStore((s) => s.updateLibraryConfig);
  const removeLibraryPath = useAppStore((s) => s.removeLibraryPath);
  const [scanInterval, setScanInterval] = useState("6h");

  const selectedLibrary = selectedLibraryId
    ? libraryPaths.find((lp) => lp.id === selectedLibraryId)
    : null;

  // Determine effective config: library-specific or global defaults
  const effectiveConfig = selectedLibrary
    ? selectedLibrary.transcode
    : {
        output_video: globalSettings.default_video,
        output_audio: globalSettings.default_audio,
        output_container: globalSettings.default_container,
        crf: globalSettings.default_crf,
        skip_open_formats: globalSettings.default_skip_open,
      };

  const handleVideoChange = useCallback(
    (v: OutputFormat) => {
      if (selectedLibrary) {
        updateLibraryConfig(selectedLibrary.id, { output_video: v });
      } else {
        updateGlobalSettings({ default_video: v });
      }
    },
    [selectedLibrary, updateLibraryConfig, updateGlobalSettings],
  );

  const handleAudioChange = useCallback(
    (v: OutputAudioFormat) => {
      if (selectedLibrary) {
        updateLibraryConfig(selectedLibrary.id, { output_audio: v });
      } else {
        updateGlobalSettings({ default_audio: v });
      }
    },
    [selectedLibrary, updateLibraryConfig, updateGlobalSettings],
  );

  const handleContainerChange = useCallback(
    (v: OutputContainer) => {
      if (selectedLibrary) {
        updateLibraryConfig(selectedLibrary.id, { output_container: v });
      } else {
        updateGlobalSettings({ default_container: v });
      }
    },
    [selectedLibrary, updateLibraryConfig, updateGlobalSettings],
  );

  const handleCrfChange = useCallback(
    (v: number) => {
      if (selectedLibrary) {
        updateLibraryConfig(selectedLibrary.id, { crf: v });
      } else {
        updateGlobalSettings({ default_crf: v });
      }
    },
    [selectedLibrary, updateLibraryConfig, updateGlobalSettings],
  );

  const handleSkipOpenChange = useCallback(() => {
    if (selectedLibrary) {
      updateLibraryConfig(selectedLibrary.id, {
        skip_open_formats: !effectiveConfig.skip_open_formats,
      });
    } else {
      updateGlobalSettings({
        default_skip_open: !globalSettings.default_skip_open,
      });
    }
  }, [
    selectedLibrary,
    effectiveConfig,
    globalSettings,
    updateLibraryConfig,
    updateGlobalSettings,
  ]);

  const resetToDefaults = useCallback(() => {
    if (selectedLibrary) {
      updateLibraryConfig(selectedLibrary.id, {
        output_video: globalSettings.default_video,
        output_audio: globalSettings.default_audio,
        output_container: globalSettings.default_container,
        crf: globalSettings.default_crf,
        skip_open_formats: globalSettings.default_skip_open,
      });
    }
  }, [selectedLibrary, globalSettings, updateLibraryConfig]);

  // Recommended concurrent jobs based on GPU
  const recommendedJobs = hardware.gpu_name ? 2 : Math.max(1, Math.floor(hardware.cpu_cores / 4));

  // Device options
  const deviceOptions = [
    ...(hardware.gpu_name
      ? [
          {
            id: "gpu",
            label: `${hardware.gpu_name} (NVENC)`,
            icon: Zap,
            recommended: true,
          },
        ]
      : []),
    {
      id: "cpu",
      label: `CPU (Software) - ${hardware.cpu_cores} cores`,
      icon: Cpu,
      recommended: !hardware.gpu_name,
    },
  ];

  const [selectedDevice, setSelectedDevice] = useState(
    hardware.gpu_name ? "gpu" : "cpu",
  );

  return (
    <div className="max-w-3xl mx-auto px-6 py-6 space-y-6">
      {/* Header */}
      <div>
        <h2 className="font-heading text-xl tracking-tight text-foreground">
          {selectedLibrary
            ? `Settings for ${selectedLibrary.path}`
            : "Global Default Settings"}
        </h2>
        <p className="text-sm text-muted-foreground mt-1">
          {selectedLibrary
            ? "These settings override the defaults for this library."
            : "These settings apply to all libraries without custom overrides."}
        </p>
      </div>

      {/* Output Format Card */}
      <Card>
        <CardHeader>
          <CardTitle>Output Format</CardTitle>
          <CardDescription>Video, audio, and container settings</CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          <div>
            <p className="mb-2.5 text-xs text-muted-foreground/80 uppercase tracking-wider font-medium">
              Video Codec
            </p>
            <VideoCodecSelector
              value={effectiveConfig.output_video}
              onChange={handleVideoChange}
            />
          </div>

          <div>
            <p className="mb-2.5 text-xs text-muted-foreground/80 uppercase tracking-wider font-medium">
              Audio Codec
            </p>
            <PillSelector
              options={AUDIO_OPTIONS.map((a) => ({
                value: a.codec,
                label: a.label,
                open: a.open,
              }))}
              value={effectiveConfig.output_audio}
              onChange={handleAudioChange}
              ariaLabel="Audio codec"
            />
          </div>

          <div>
            <p className="mb-2.5 text-xs text-muted-foreground/80 uppercase tracking-wider font-medium">
              Container
            </p>
            <PillSelector
              options={CONTAINER_OPTIONS}
              value={effectiveConfig.output_container}
              onChange={handleContainerChange}
              ariaLabel="Container format"
            />
          </div>

          <CrfSlider
            value={effectiveConfig.crf}
            onChange={handleCrfChange}
          />
        </CardContent>
      </Card>

      {/* Scan Settings Card */}
      <Card>
        <CardHeader>
          <CardTitle>Scan Settings</CardTitle>
          <CardDescription>Library watching and file discovery</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <Toggle
            label="Auto-scan"
            sublabel="Automatically watch library folders for new files"
            checked={globalSettings.auto_scan}
            onChange={() =>
              updateGlobalSettings({ auto_scan: !globalSettings.auto_scan })
            }
          />

          {globalSettings.auto_scan && (
            <div>
              <p className="mb-2.5 text-xs text-muted-foreground/80 uppercase tracking-wider font-medium">
                Scan Interval
              </p>
              <div className="flex flex-wrap gap-1.5" role="radiogroup" aria-label="Scan interval">
                {SCAN_INTERVALS.map((opt) => {
                  const selected = scanInterval === opt.value;
                  return (
                    <button
                      type="button"
                      key={opt.value}
                      role="radio"
                      aria-checked={selected}
                      onClick={() => setScanInterval(opt.value)}
                      className={`rounded-lg px-3 py-1.5 text-xs font-medium transition-all focus-visible:outline-2 focus-visible:outline-ring ${
                        selected
                          ? "bg-gold/15 text-gold ring-2 ring-gold/40"
                          : "bg-secondary/40 text-muted-foreground hover:bg-secondary/60"
                      }`}
                    >
                      {opt.label}
                    </button>
                  );
                })}
              </div>
            </div>
          )}
        </CardContent>
      </Card>

      {/* Processing Card */}
      <Card>
        <CardHeader>
          <CardTitle>Processing</CardTitle>
          <CardDescription>Encoding device and job configuration</CardDescription>
        </CardHeader>
        <CardContent className="space-y-5">
          {/* Device selector */}
          <div>
            <p className="mb-2.5 text-xs text-muted-foreground/80 uppercase tracking-wider font-medium">
              Encoding Device
            </p>
            <div className="space-y-2" role="radiogroup" aria-label="Encoding device">
              {deviceOptions.map((dev) => {
                const selected = selectedDevice === dev.id;
                const DevIcon = dev.icon;
                return (
                  <button
                    type="button"
                    key={dev.id}
                    role="radio"
                    aria-checked={selected}
                    onClick={() => setSelectedDevice(dev.id)}
                    className={`flex w-full items-center gap-3 rounded-lg border px-4 py-3 text-left transition-all focus-visible:outline-2 focus-visible:outline-ring ${
                      selected
                        ? "border-gold/40 bg-gold/10 ring-1 ring-gold/30"
                        : "border-border/60 bg-card hover:border-border hover:bg-secondary/30"
                    }`}
                  >
                    <div
                      className={`flex h-4 w-4 shrink-0 items-center justify-center rounded-full border-2 transition-colors ${
                        selected
                          ? "border-gold bg-gold"
                          : "border-muted-foreground/30"
                      }`}
                    >
                      {selected && (
                        <Check className="h-2.5 w-2.5 text-gold-foreground" />
                      )}
                    </div>
                    <DevIcon
                      className={`h-4 w-4 ${
                        selected ? "text-gold" : "text-muted-foreground"
                      }`}
                    />
                    <span
                      className={`text-sm ${
                        selected ? "text-foreground font-medium" : "text-muted-foreground"
                      }`}
                    >
                      {dev.label}
                    </span>
                    {dev.recommended && (
                      <Badge
                        variant="secondary"
                        className="ml-auto text-[10px] text-emerald-400 bg-emerald-400/10"
                      >
                        Recommended
                      </Badge>
                    )}
                  </button>
                );
              })}
            </div>
            <p className="mt-2 text-[10px] text-muted-foreground/50 flex items-center gap-1.5">
              <Monitor className="h-3 w-3" />
              Recommended: {recommendedJobs} concurrent jobs based on{" "}
              {hardware.gpu_name ?? "CPU"}
            </p>
          </div>

          {/* Concurrent jobs */}
          <JobsSlider
            value={globalSettings.concurrent_jobs}
            onChange={(v) => updateGlobalSettings({ concurrent_jobs: v })}
            recommended={recommendedJobs}
          />

          <Toggle
            label="Auto-transcode"
            sublabel="Automatically start processing newly scanned files"
            checked={globalSettings.auto_transcode}
            onChange={() =>
              updateGlobalSettings({
                auto_transcode: !globalSettings.auto_transcode,
              })
            }
          />

          <Toggle
            label="Skip open formats"
            sublabel="Don't re-encode files already using AV1, VP9, Opus, or FLAC"
            checked={effectiveConfig.skip_open_formats}
            onChange={handleSkipOpenChange}
          />
        </CardContent>
      </Card>

      {/* Danger Zone Card */}
      <Card className="ring-destructive/20">
        <CardContent>
          <CollapsibleSection
            title="Danger Zone"
            icon={<AlertTriangle className="h-4 w-4 text-destructive/70" />}
            defaultOpen={false}
          >
            <div className="space-y-3">
              <div className="rounded-lg border border-destructive/20 bg-destructive/5 p-4 space-y-3">
                <div className="flex items-center justify-between">
                  <div>
                    <p className="text-sm text-foreground">Reset to defaults</p>
                    <p className="text-xs text-muted-foreground/60">
                      {selectedLibrary
                        ? "Reset this library's settings to the global defaults."
                        : "Reset all global settings to their factory values."}
                    </p>
                  </div>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={resetToDefaults}
                    className="gap-1.5 text-xs border-muted-foreground/20 hover:border-amber-400/30 hover:text-amber-400"
                  >
                    <RotateCcw className="h-3 w-3" />
                    Reset
                  </Button>
                </div>

                {selectedLibrary && (
                  <>
                    <div className="h-px bg-destructive/20" />
                    <div className="flex items-center justify-between">
                      <div>
                        <p className="text-sm text-destructive">Remove library</p>
                        <p className="text-xs text-muted-foreground/60">
                          Remove this library from Chrysopoeia. Files on disk are not
                          affected.
                        </p>
                      </div>
                      <Button
                        size="sm"
                        variant="outline"
                        onClick={() => removeLibraryPath(selectedLibrary.id)}
                        className="gap-1.5 text-xs border-destructive/30 text-destructive hover:bg-destructive/10 hover:border-destructive/50"
                      >
                        <Trash2 className="h-3 w-3" />
                        Remove
                      </Button>
                    </div>
                  </>
                )}
              </div>

              <div className="flex items-start gap-2 px-1">
                <AlertTriangle className="h-3.5 w-3.5 text-amber-400/60 shrink-0 mt-0.5" />
                <p className="text-[10px] text-muted-foreground/50">
                  These actions cannot be undone. Proceed with caution.
                </p>
              </div>
            </div>
          </CollapsibleSection>
        </CardContent>
      </Card>
    </div>
  );
}
