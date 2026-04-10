"use client";

import { useCallback, useEffect, useState } from "react";
import { Play, Pause, RotateCw, FolderOpen, Activity } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";

import { ScrollArea } from "@/components/ui/scroll-area";
import { useAppStore } from "@/lib/store";
import {
  scanLibrary,
  startProcessing as apiStart,
  stopProcessing as apiStop,
} from "@/lib/api";
import { FileListSkeleton } from "./file-list-skeleton";
import { ActivityLog } from "./activity-log";
import { ThemeToggle } from "./theme-toggle";
import { OverviewTab } from "./tabs/overview-tab";
import { QueueTab } from "./tabs/queue-tab";
import { LibrarySettingsTab } from "./tabs/library-settings-tab";

const ONBOARDING_STEPS = [
  {
    num: 1,
    title: "Add media library paths in the sidebar",
    desc: "Point Chrysopoeia at your movie, TV, or anime folders.",
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

const TABS = [
  { key: "overview" as const, label: "Overview" },
  { key: "queue" as const, label: "Queue" },
  { key: "settings" as const, label: "Settings" },
];

export function Dashboard() {
  const isProcessing = useAppStore((s) => s.isProcessing);
  const isScanning = useAppStore((s) => s.isScanning);
  const wsConnected = useAppStore((s) => s.wsConnected);
  const libraryPaths = useAppStore((s) => s.library_paths);
  const isLoading = useAppStore((s) => s.isLoading);
  const setIsLoading = useAppStore((s) => s.setIsLoading);
  const files = useAppStore((s) => s.files);
  const activeTab = useAppStore((s) => s.activeTab);
  const setActiveTab = useAppStore((s) => s.setActiveTab);

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

  return (
    <div className="flex h-full flex-col">
      {/* Single header: tabs on left, actions on right */}
      <header
        className={`relative flex shrink-0 items-center border-b border-border px-4 md:px-6 py-2 transition-colors ${
          !wsConnected ? "bg-destructive/5" : ""
        }`}
      >
        {isScanning && (
          <div className="absolute inset-x-0 bottom-0 h-[2px] overflow-hidden">
            <div className="h-full w-1/4 bg-gold/50 rounded-full animate-[scan-sweep_2s_ease-in-out_infinite]" />
          </div>
        )}

        {/* Centered tabs */}
        <div className="absolute inset-0 flex items-center justify-center pointer-events-none">
          <nav className="flex items-center gap-1 rounded-lg bg-muted/50 p-0.5 pointer-events-auto" role="tablist">
            {TABS.map((tab) => (
              <button
                key={tab.key}
                type="button"
                role="tab"
                aria-selected={activeTab === tab.key}
                onClick={() => setActiveTab(tab.key)}
                className={`rounded-md px-3 py-1 text-xs font-medium transition-all ${
                  activeTab === tab.key
                    ? "bg-background text-foreground shadow-sm"
                    : "text-muted-foreground hover:text-foreground"
                }`}
              >
                {tab.label}
              </button>
            ))}
          </nav>
        </div>

        {/* Spacer to push actions right */}
        <div className="flex-1" />

        {/* Actions */}
        <div className="relative z-10 flex items-center gap-2">
          <ThemeToggle />
          <Button
            type="button"
            size="sm"
            variant="secondary"
            onClick={handleScan}
            disabled={isScanning}
            className="gap-1.5 text-xs"
          >
            <RotateCw
              className={`h-3 w-3 ${isScanning ? "animate-spin" : ""}`}
            />
            {isScanning ? "Scanning..." : "Scan"}
          </Button>

          {isProcessing ? (
            <Button
              type="button"
              size="sm"
              variant="secondary"
              onClick={handleStop}
              className="gap-1.5 text-xs bg-gold/15 text-gold border border-gold/20 hover:bg-gold/25 animate-pulse"
            >
              <Pause className="h-3 w-3" />
              Pause
            </Button>
          ) : (
            <Button
              type="button"
              size="sm"
              onClick={handleStart}
              className="gap-1.5 bg-gold text-gold-foreground hover:bg-gold/90 text-xs"
            >
              <Play className="h-3 w-3" />
              Start
            </Button>
          )}
        </div>
      </header>

      {/* Tab content — fixed area, no expanding */}
      <ScrollArea className="min-h-0 flex-1 overflow-hidden">
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
              Welcome to Chrysopoeia
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
        ) : (
          <div key={activeTab} className="animate-fade-in">
            {activeTab === "overview" && <OverviewTab />}
            {activeTab === "queue" && <QueueTab />}
            {activeTab === "settings" && <LibrarySettingsTab />}
          </div>
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
            aria-label="Show activity log"
            className="relative flex h-6 items-center gap-1 rounded-full border border-border/40 bg-card/80 px-2 text-[10px] font-medium text-muted-foreground/60 transition-all hover:text-muted-foreground hover:border-border focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-gold/40"
            title="Show activity log"
          >
            <Activity className="h-3 w-3" />
            Activity
            {hasErrors && (
              <span className="absolute -top-0.5 -right-0.5 h-2 w-2 rounded-full bg-destructive ring-1 ring-card" />
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
          aria-label="Keyboard shortcuts help"
          className="flex h-6 w-6 items-center justify-center rounded-full border border-border/40 bg-card/80 text-[10px] font-medium text-muted-foreground/40 opacity-0 transition-all hover:opacity-100 hover:text-muted-foreground hover:border-border focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-gold/40"
          title="Press ? for shortcuts"
        >
          ?
        </button>
      </div>
    </div>
  );
}
