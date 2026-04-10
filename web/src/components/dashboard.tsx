"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { Play, Pause, RotateCw, Settings2, FolderOpen, Activity, ChevronUp } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { ScrollArea } from "@/components/ui/scroll-area";
import { useAppStore } from "@/lib/store";
import {
  scanLibrary,
  startProcessing as apiStart,
  stopProcessing as apiStop,
} from "@/lib/api";
import { FileList } from "./file-list";
import { FileListSkeleton } from "./file-list-skeleton";
import { ActivityLog } from "./activity-log";
import { SettingsPanel } from "./settings-panel";

function formatBytes(bytes: number): string {
  if (bytes >= 1e12) return `${(bytes / 1e12).toFixed(1)} TB`;
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  return `${(bytes / 1e6).toFixed(0)} MB`;
}

const ONBOARDING_STEPS = [
  {
    num: 1,
    title: "Add media library paths in the sidebar",
    desc: "Point Chrysopeia at your movie, TV, or anime folders.",
  },
  {
    num: 2,
    title: "Choose your output format (AV1 recommended)",
    desc: "Pick a codec, audio format, and container for the best results.",
  },
  {
    num: 3,
    title: "Press Start \u2014 we handle the rest",
    desc: "Sit back while your library is transmuted into open formats.",
  },
];

export function Dashboard() {
  const stats = useAppStore((s) => s.stats);
  const isProcessing = useAppStore((s) => s.isProcessing);
  const isScanning = useAppStore((s) => s.isScanning);
  const wsConnected = useAppStore((s) => s.wsConnected);
  const globalSettings = useAppStore((s) => s.globalSettings);
  const libraryPaths = useAppStore((s) => s.library_paths);
  const isLoading = useAppStore((s) => s.isLoading);
  const setIsLoading = useAppStore((s) => s.setIsLoading);
  const files = useAppStore((s) => s.files);
  const settingsOpen = useAppStore((s) => s.settingsOpen);
  const setSettingsOpen = useAppStore((s) => s.setSettingsOpen);
  const selectedLibraryId = useAppStore((s) => s.selectedLibraryId);

  // Find selected library name
  const selectedLibrary = libraryPaths.find((lp) => lp.id === selectedLibraryId);
  const headerTitle = selectedLibrary
    ? selectedLibrary.path.split("/").pop() ?? selectedLibrary.path
    : "Library";

  // Filter files by selected library
  const filteredFiles = useMemo(() => {
    if (!selectedLibraryId) return files;
    const lib = libraryPaths.find((lp) => lp.id === selectedLibraryId);
    if (!lib) return files;
    return files.filter((f) => f.library_path === lib.path);
  }, [files, selectedLibraryId, libraryPaths]);

  // Compute filtered stats
  const filteredStats = useMemo(() => {
    if (!selectedLibraryId) return stats;
    return {
      total_files: filteredFiles.length,
      pending: filteredFiles.filter((f) => f.status === "pending").length,
      queued: filteredFiles.filter((f) => f.status === "queued").length,
      transcoding: filteredFiles.filter((f) => f.status === "transcoding").length,
      complete: filteredFiles.filter((f) => f.status === "complete").length,
      skipped: filteredFiles.filter((f) => f.status === "skipped").length,
      errored: filteredFiles.filter((f) => f.status === "error").length,
      total_size_bytes: filteredFiles.reduce((a, f) => a + f.size_bytes, 0),
      saved_bytes: filteredFiles.reduce(
        (a, f) =>
          a +
          (f.status === "complete" && f.output_size_bytes != null
            ? f.size_bytes - f.output_size_bytes
            : 0),
        0,
      ),
    };
  }, [selectedLibraryId, filteredFiles, stats]);

  // Simulate initial data load
  useEffect(() => {
    const timer = setTimeout(() => setIsLoading(false), 1500);
    return () => clearTimeout(timer);
  }, [setIsLoading]);

  const logEntries = useAppStore((s) => s.logEntries);
  const [logOpen, setLogOpen] = useState(false);
  const hasErrors = logEntries.some((e) => e.level === "error");

  const showOnboarding =
    !isLoading && libraryPaths.length === 0 && files.length === 0;

  const handleScan = useCallback(async () => {
    useAppStore.getState().startScan();
    try {
      const result = await scanLibrary();
      // scanComplete fires its own toast
      setTimeout(() => useAppStore.getState().scanComplete(result.queued), 500);
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "Scan failed");
      setTimeout(() => useAppStore.getState().scanComplete(), 500);
    }
  }, []);

  const handleStart = useCallback(async () => {
    try {
      await apiStart();
      useAppStore.getState().startProcessing();
    } catch (err) {
      toast.error(
        err instanceof Error ? err.message : "Failed to start processing",
      );
    }
  }, []);

  const handleStop = useCallback(async () => {
    try {
      await apiStop();
      useAppStore.getState().stopProcessing();
    } catch (err) {
      toast.error(
        err instanceof Error ? err.message : "Failed to pause processing",
      );
    }
  }, []);

  const displayStats = filteredStats;
  const completionPct =
    displayStats.total_files > 0
      ? Math.round(
          ((displayStats.complete + displayStats.skipped) / displayStats.total_files) * 100
        )
      : 0;

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <header
        className={`relative flex shrink-0 flex-col sm:flex-row sm:items-center sm:justify-between gap-3 border-b border-border px-4 md:px-6 py-4 overflow-hidden transition-colors ${
          !wsConnected ? "bg-destructive/5" : ""
        }`}
      >
        {isScanning && (
          <div className="absolute inset-x-0 bottom-0 h-[2px] overflow-hidden">
            <div className="h-full w-1/4 bg-gold/50 rounded-full animate-[scan-sweep_2s_ease-in-out_infinite]" />
          </div>
        )}
        <div>
          <div className="flex items-center gap-3">
            <div className="flex items-center gap-2">
              <h2 className="font-heading text-xl tracking-tight">{headerTitle}</h2>
              <span
                className={`inline-block h-2 w-2 rounded-full ${
                  wsConnected
                    ? "status-pulse bg-emerald-400"
                    : "bg-destructive animate-pulse"
                }`}
                title={wsConnected ? "Connected" : "Disconnected"}
              />
              {!wsConnected && (
                <span className="text-[10px] text-destructive">
                  Reconnecting...
                </span>
              )}
            </div>
            <span className="text-xs tabular-nums text-muted-foreground">
              {displayStats.total_files.toLocaleString()} files &middot;{" "}
              {formatBytes(displayStats.total_size_bytes)}
            </span>
          </div>

          {/* Size overlay bar: total original size as full, compressed as gold overlay */}
          <div className="mt-2">
            <div className="relative h-2 w-40 md:w-64 rounded-full bg-muted-foreground/15 overflow-hidden">
              <div
                className="absolute inset-y-0 left-0 rounded-full bg-gold/60 transition-all duration-700"
                style={{ width: `${completionPct}%` }}
              />
            </div>
            <div className="flex items-center gap-3 mt-1">
              <span className="text-[10px] tabular-nums text-muted-foreground">
                {completionPct}% complete
              </span>
              {displayStats.saved_bytes > 0 && (
                <span className="text-[10px] text-gold tabular-nums">
                  {formatBytes(displayStats.saved_bytes)} saved
                </span>
              )}
            </div>
          </div>
        </div>

        {/* Controls */}
        <div className="flex items-center gap-2 self-start sm:self-auto">
          <Button
            size="sm"
            variant={settingsOpen ? "default" : "secondary"}
            onClick={() => setSettingsOpen(!settingsOpen)}
            className={`gap-1.5 text-xs transition-transform hover:scale-[1.02] ${
              settingsOpen
                ? "bg-gold/15 text-gold border border-gold/20"
                : ""
            }`}
            aria-label="Toggle settings"
            aria-expanded={settingsOpen}
          >
            {settingsOpen ? (
              <ChevronUp className="h-3 w-3" />
            ) : (
              <Settings2 className="h-3 w-3" />
            )}
          </Button>
          <Button
            size="sm"
            variant="secondary"
            onClick={handleScan}
            disabled={isScanning}
            className="gap-1.5 text-xs transition-transform hover:scale-[1.02]"
          >
            <RotateCw
              className={`h-3 w-3 ${isScanning ? "animate-spin" : ""}`}
            />
            {isScanning ? "Scanning..." : "Scan"}
          </Button>

          {isProcessing ? (
            <Button
              size="sm"
              variant="secondary"
              onClick={handleStop}
              className="gap-1.5 text-xs bg-gold/15 text-gold border border-gold/20 hover:bg-gold/25 animate-pulse transition-transform hover:scale-[1.02]"
            >
              <Pause className="h-3 w-3" />
              Pause
            </Button>
          ) : (
            <Button
              size="sm"
              onClick={handleStart}
              className="gap-1.5 bg-gold text-gold-foreground hover:bg-gold/90 text-xs shadow-[0_0_12px_oklch(0.78_0.12_75_/_0.3)] hover:shadow-[0_0_16px_oklch(0.78_0.12_75_/_0.4)] transition-transform hover:scale-[1.02]"
            >
              <Play className="h-3 w-3" />
              Start
            </Button>
          )}
        </div>
      </header>

      {/* Settings panel */}
      <SettingsPanel />

      {/* Stats row */}
      <div aria-live="polite" className="flex shrink-0 flex-wrap items-center gap-y-2 border-b border-border px-4 md:px-6 py-3">
        <div className="flex flex-wrap items-center divide-x divide-border">
          <div className="pr-4">
            <Stat label="Transcoding" value={displayStats.transcoding} active />
          </div>
          <div className="px-4">
            <Stat label="Queued" value={displayStats.queued} />
          </div>
          <div className="px-4">
            <Stat label="Pending" value={displayStats.pending} />
          </div>
          <div className="px-4">
            <Stat label="Complete" value={displayStats.complete} accent />
          </div>
          <div className="px-4">
            <Stat label="Skipped" value={displayStats.skipped} />
          </div>
          {displayStats.errored > 0 && (
            <div className="pl-4">
              <Stat label="Errors" value={displayStats.errored} error />
            </div>
          )}
        </div>
        <div className="ml-auto flex items-center gap-1.5">
          <span className="inline-flex items-center gap-1 rounded-full bg-gold/10 px-2.5 py-0.5 text-[10px] font-mono uppercase text-gold ring-1 ring-inset ring-gold/20">
            {globalSettings.default_video}
            <span className="text-gold/40">/</span>
            {globalSettings.default_audio}
            <span className="text-gold/40">/</span>
            .{globalSettings.default_container}
          </span>
        </div>
      </div>

      {/* Separator */}
      <div className="h-px bg-border/40" />

      {/* File list, skeleton, onboarding, or empty state */}
      <ScrollArea className="min-h-0 flex-1">
        {isLoading ? (
          <FileListSkeleton />
        ) : showOnboarding ? (
          <div className="flex flex-col items-center justify-center py-20 px-6 text-center">
            {/* Logo */}
            <div className="animate-fade-up mb-8 flex h-16 w-16 items-center justify-center rounded-2xl bg-gold/10">
              <svg
                viewBox="0 0 24 24"
                className="h-8 w-8 text-gold"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.5"
              >
                <circle cx="12" cy="12" r="9" />
                <circle cx="12" cy="12" r="4" />
                <line x1="12" y1="3" x2="12" y2="8" />
                <line x1="12" y1="16" x2="12" y2="21" />
                <line x1="3" y1="12" x2="8" y2="12" />
                <line x1="16" y1="12" x2="21" y2="12" />
              </svg>
            </div>

            <h2 className="animate-fade-up stagger-1 font-heading text-2xl tracking-tight text-foreground">
              Welcome to Chrysopeia
            </h2>
            <p className="animate-fade-up stagger-2 mt-2 max-w-sm text-sm text-muted-foreground">
              Transmute your media library into modern, open formats.
            </p>

            {/* Steps */}
            <div className="mt-10 w-full max-w-md space-y-5">
              {ONBOARDING_STEPS.map((step) => (
                <div
                  key={step.num}
                  className={`animate-fade-up stagger-${step.num + 2} flex items-start gap-4 text-left`}
                >
                  <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-gold/10 text-sm font-semibold text-gold tabular-nums">
                    {step.num}
                  </span>
                  <div className="pt-0.5">
                    <p className="text-sm font-medium text-foreground">
                      {step.title}
                    </p>
                    <p className="mt-0.5 text-xs text-muted-foreground/70">
                      {step.desc}
                    </p>
                  </div>
                </div>
              ))}
            </div>
          </div>
        ) : libraryPaths.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-24 text-center gap-4">
            <div className="rounded-full bg-secondary/60 p-4">
              <FolderOpen className="h-8 w-8 text-muted-foreground/40" />
            </div>
            <div>
              <p className="text-sm font-medium text-muted-foreground">No libraries configured</p>
              <p className="text-xs text-muted-foreground/60 mt-1.5 max-w-xs mx-auto">
                Add library paths in the sidebar to scan your media collection and start transcoding
              </p>
            </div>
          </div>
        ) : !globalSettings.default_video ? (
          <div className="flex flex-col items-center justify-center py-24 text-center gap-4">
            <div className="rounded-full bg-secondary/60 p-4">
              <Settings2 className="h-8 w-8 text-muted-foreground/40" />
            </div>
            <div>
              <p className="text-sm font-medium text-muted-foreground">Select an output format</p>
              <p className="text-xs text-muted-foreground/60 mt-1.5 max-w-xs mx-auto">
                Choose a video codec in the sidebar to configure your transcoding pipeline
              </p>
            </div>
          </div>
        ) : (
          <FileList />
        )}
      </ScrollArea>

      {/* Activity log panel */}
      <ActivityLog isOpen={logOpen} onToggle={() => setLogOpen((v) => !v)} />

      {/* Bottom-right controls */}
      <div className="absolute bottom-3 right-3 flex items-center gap-1.5">
        {/* Activity toggle button */}
        {!logOpen && (
          <button
            type="button"
            onClick={() => setLogOpen(true)}
            className="relative flex h-6 items-center gap-1 rounded-full border border-border/40 bg-card/80 px-2 text-[10px] font-medium text-muted-foreground/60 transition-all hover:text-muted-foreground hover:border-border focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-gold/40"
            title="Show activity log"
          >
            <Activity className="h-3 w-3" />
            Activity
            {hasErrors && (
              <span className="absolute -top-0.5 -right-0.5 h-2 w-2 rounded-full bg-red-400 ring-1 ring-card" />
            )}
          </button>
        )}

        {/* Floating help button */}
        <button
          type="button"
          onClick={() => {
            document.dispatchEvent(
              new KeyboardEvent("keydown", { key: "?" }),
            );
          }}
          className="flex h-6 w-6 items-center justify-center rounded-full border border-border/40 bg-card/80 text-[10px] font-medium text-muted-foreground/40 opacity-0 transition-all hover:opacity-100 hover:text-muted-foreground hover:border-border focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-gold/40"
          title="Press ? for shortcuts"
        >
          ?
        </button>
      </div>
    </div>
  );
}

function Stat({
  label,
  value,
  accent,
  active,
  error,
}: {
  label: string;
  value: number;
  accent?: boolean;
  active?: boolean;
  error?: boolean;
}) {
  let valueClass = "text-foreground";
  if (accent) valueClass = "text-gold";
  if (active) valueClass = "text-emerald-400";
  if (error) valueClass = "text-destructive";

  return (
    <div className={`flex flex-col items-center gap-0.5 ${error ? "animate-error-pulse rounded-md px-1.5 py-0.5 -mx-1.5 -my-0.5" : ""}`}>
      <span className={`text-base font-semibold tabular-nums leading-none ${valueClass}`}>
        {value.toLocaleString()}
      </span>
      <span className="text-[9px] text-muted-foreground/70 uppercase tracking-wider">{label}</span>
    </div>
  );
}
