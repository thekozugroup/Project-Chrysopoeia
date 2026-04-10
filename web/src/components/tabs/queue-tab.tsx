"use client";

import { useMemo } from "react";
import { Activity, TrendingDown, Clock, HardDrive } from "lucide-react";
import { useAppStore } from "@/lib/store";
import { FileList } from "@/components/file-list";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";

function formatBytes(bytes: number): string {
  if (bytes >= 1e12) return `${(bytes / 1e12).toFixed(1)} TB`;
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  return `${(bytes / 1e6).toFixed(0)} MB`;
}

function formatEtaHours(secs: number): string {
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (h > 0) return `~${h}h ${m}m remaining`;
  return `~${m}m remaining`;
}

export function QueueTab() {
  const files = useAppStore((s) => s.files);
  const stats = useAppStore((s) => s.stats);

  const activeFiles = useMemo(
    () => files.filter((f) => f.status === "transcoding"),
    [files],
  );

  // Compute compression summary from all files that have output sizes
  const compressionSummary = useMemo(() => {
    const totalOriginal = stats.total_size_bytes;
    const savedBytes = stats.saved_bytes;
    const totalNew = totalOriginal - savedBytes;
    const reductionPct =
      totalOriginal > 0
        ? Math.round((savedBytes / totalOriginal) * 100)
        : 0;
    const compressedPct =
      totalOriginal > 0
        ? Math.round((totalNew / totalOriginal) * 100)
        : 100;

    return {
      totalOriginal,
      totalNew,
      savedBytes,
      reductionPct,
      compressedPct,
    };
  }, [stats]);

  // Active processing stats
  const processingStats = useMemo(() => {
    const transcodingCount = activeFiles.length;
    const pendingCount = stats.pending + stats.queued;

    // Average speed from active files
    const speeds = activeFiles
      .map((f) => f.speed)
      .filter((s): s is string => s != null)
      .map((s) => parseFloat(s));
    const avgSpeed =
      speeds.length > 0
        ? (speeds.reduce((a, b) => a + b, 0) / speeds.length).toFixed(1)
        : null;

    // Total ETA from active files (use max as rough estimate)
    const etas = activeFiles
      .map((f) => f.eta_secs)
      .filter((e): e is number => e != null);
    const totalEta = etas.length > 0 ? Math.max(...etas) : null;

    return { transcodingCount, pendingCount, avgSpeed, totalEta };
  }, [activeFiles, stats]);

  return (
    <div className="flex flex-col">
      {/* Compression Summary Card */}
      <div className="px-6 pt-6 pb-3">
      <Card>
        <CardHeader>
          <CardTitle>Compression Summary</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="flex flex-wrap items-baseline gap-x-8 gap-y-3">
            <div className="flex flex-col">
              <span className="text-xs text-muted-foreground flex items-center gap-1.5">
                <HardDrive className="h-3.5 w-3.5" />
                Original
              </span>
              <span className="font-heading text-2xl tabular-nums tracking-tight text-foreground">
                {formatBytes(compressionSummary.totalOriginal)}
              </span>
            </div>
            <div className="text-muted-foreground/40 text-lg select-none">&rarr;</div>
            <div className="flex flex-col">
              <span className="text-xs text-muted-foreground flex items-center gap-1.5">
                <TrendingDown className="h-3.5 w-3.5 text-gold" />
                Estimated
              </span>
              <span className="font-heading text-2xl tabular-nums tracking-tight text-gold">
                {formatBytes(compressionSummary.totalNew)}
              </span>
            </div>
            <div className="text-muted-foreground/40 text-lg select-none">&rarr;</div>
            <div className="flex flex-col">
              <span className="text-xs text-muted-foreground">Saved</span>
              <span className="font-heading text-2xl tabular-nums tracking-tight text-emerald-400">
                {formatBytes(compressionSummary.savedBytes)}
              </span>
              <span className="text-[10px] text-emerald-400/70 tabular-nums">
                {compressionSummary.reductionPct}% reduction
              </span>
            </div>
          </div>

          {/* Visual compression bar */}
          <div className="space-y-1.5">
            <div className="relative h-3 w-full rounded-full bg-secondary/60 overflow-hidden">
              {/* Full bar = original size */}
              <div
                className="absolute inset-y-0 left-0 rounded-full bg-gold/80 transition-all duration-500"
                style={{ width: `${compressionSummary.compressedPct}%` }}
              />
              {/* Shimmer overlay */}
              <div
                className="absolute inset-y-0 left-0 rounded-full bg-gradient-to-r from-transparent via-white/10 to-transparent animate-pulse"
                style={{
                  width: `${compressionSummary.compressedPct}%`,
                  animationDuration: "3s",
                }}
              />
            </div>
            <p className="text-[10px] text-muted-foreground/60 tabular-nums">
              {formatBytes(compressionSummary.totalOriginal)} &rarr;{" "}
              {formatBytes(compressionSummary.totalNew)} (
              {formatBytes(compressionSummary.savedBytes)} saved,{" "}
              {compressionSummary.reductionPct}% reduction)
            </p>
          </div>
        </CardContent>
      </Card>

      </div>

      {/* Active Processing Status */}
      <div className="px-6 pb-3">
      <Card>
        <CardContent>
          <div className="flex items-center gap-2.5 text-sm">
            {processingStats.transcodingCount > 0 ? (
              <>
                <Activity className="h-4 w-4 text-emerald-400 animate-pulse" />
                <span className="text-foreground font-medium">
                  Processing{" "}
                  <span className="font-heading text-base tabular-nums">
                    {processingStats.transcodingCount}
                  </span>{" "}
                  of{" "}
                  <span className="font-heading text-base tabular-nums">
                    {processingStats.pendingCount + processingStats.transcodingCount}
                  </span>{" "}
                  files
                </span>
                {processingStats.avgSpeed && (
                  <>
                    <span className="text-border">&middot;</span>
                    <span className="tabular-nums text-muted-foreground">
                      {processingStats.avgSpeed}x avg speed
                    </span>
                  </>
                )}
                {processingStats.totalEta != null && (
                  <>
                    <span className="text-border">&middot;</span>
                    <Clock className="h-3.5 w-3.5 text-muted-foreground" />
                    <span className="tabular-nums text-muted-foreground">
                      {formatEtaHours(processingStats.totalEta)}
                    </span>
                  </>
                )}
              </>
            ) : (
              <>
                <Activity className="h-4 w-4 text-muted-foreground/40" />
                <span className="text-muted-foreground">No active transcoding jobs</span>
              </>
            )}
          </div>
        </CardContent>
      </Card>
      </div>

      {/* File List — no extra padding, let it fill remaining space */}
      <FileList />
    </div>
  );
}
