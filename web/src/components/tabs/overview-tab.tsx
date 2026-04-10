"use client";

import { useMemo } from "react";
import { useAppStore } from "@/lib/store";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB", "PB"];
  const i = Math.floor(Math.log(bytes) / Math.log(1024));
  const val = bytes / Math.pow(1024, i);
  return `${val >= 100 ? val.toFixed(0) : val.toFixed(1)} ${units[i]}`;
}

function pct(count: number, total: number): number {
  return total === 0 ? 0 : (count / total) * 100;
}

// ---------------------------------------------------------------------------
// Codec / resolution color maps
// ---------------------------------------------------------------------------

const VIDEO_COLORS: Record<string, string> = {
  h264: "bg-amber-500",
  h265: "bg-orange-500",
  hevc: "bg-orange-500",
  av1: "bg-emerald-500",
  vp9: "bg-blue-500",
  vp8: "bg-sky-400",
  prores: "bg-violet-500",
  mpeg2: "bg-rose-400",
  theora: "bg-teal-400",
  dnxhd: "bg-pink-400",
};

const VIDEO_TEXT_COLORS: Record<string, string> = {
  h264: "text-amber-500",
  h265: "text-orange-500",
  hevc: "text-orange-500",
  av1: "text-emerald-500",
  vp9: "text-blue-500",
  vp8: "text-sky-400",
  prores: "text-violet-500",
  mpeg2: "text-rose-400",
  theora: "text-teal-400",
  dnxhd: "text-pink-400",
};

const AUDIO_COLORS: Record<string, string> = {
  aac: "bg-amber-500",
  ac3: "bg-orange-500",
  eac3: "bg-red-400",
  flac: "bg-emerald-500",
  opus: "bg-blue-500",
  vorbis: "bg-teal-400",
  dts: "bg-violet-500",
  mp3: "bg-rose-400",
  pcm: "bg-sky-400",
};

const AUDIO_TEXT_COLORS: Record<string, string> = {
  aac: "text-amber-500",
  ac3: "text-orange-500",
  eac3: "text-red-400",
  flac: "text-emerald-500",
  opus: "text-blue-500",
  vorbis: "text-teal-400",
  dts: "text-violet-500",
  mp3: "text-rose-400",
  pcm: "text-sky-400",
};

const RESOLUTION_COLORS: Record<string, string> = {
  "4K": "bg-violet-500",
  "1080p": "bg-blue-500",
  "720p": "bg-amber-500",
  Other: "bg-muted-foreground/60",
};

const RESOLUTION_TEXT_COLORS: Record<string, string> = {
  "4K": "text-violet-500",
  "1080p": "text-blue-500",
  "720p": "text-amber-500",
  Other: "text-muted-foreground",
};

const STATUS_COLORS: Record<string, string> = {
  pending: "bg-muted-foreground",
  queued: "bg-amber-500",
  transcoding: "bg-blue-500",
  complete: "bg-emerald-500",
  skipped: "bg-muted-foreground/60",
  error: "bg-destructive",
};

const STATUS_TEXT_COLORS: Record<string, string> = {
  pending: "text-muted-foreground",
  queued: "text-amber-500",
  transcoding: "text-blue-500",
  complete: "text-emerald-500",
  skipped: "text-muted-foreground/60",
  error: "text-destructive",
};

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function CircularProgress({ value, size = 140 }: { value: number; size?: number }) {
  const clampedValue = Math.min(Math.max(value, 0), 100);
  const displayValue = clampedValue.toFixed(1);

  return (
    <div
      className="relative flex items-center justify-center rounded-full"
      style={{
        width: size,
        height: size,
        background: `conic-gradient(
          var(--color-emerald-500) ${clampedValue * 3.6}deg,
          var(--color-muted) ${clampedValue * 3.6}deg 360deg
        )`,
      }}
    >
      {/* Inner circle cutout */}
      <div
        className="absolute flex flex-col items-center justify-center rounded-full bg-card"
        style={{ width: size - 20, height: size - 20 }}
      >
        <span className="font-heading text-3xl tabular-nums text-foreground">
          {displayValue}
          <span className="text-lg text-muted-foreground">%</span>
        </span>
        <span className="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
          Complete
        </span>
      </div>
    </div>
  );
}

function StatItem({
  label,
  value,
  valueClass,
}: {
  label: string;
  value: string;
  valueClass?: string;
}) {
  return (
    <div className="flex flex-col items-center gap-0.5">
      <span
        className={`font-heading text-2xl tabular-nums ${valueClass ?? "text-foreground"}`}
      >
        {value}
      </span>
      <span className="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
        {label}
      </span>
    </div>
  );
}

function DistributionBar({
  data,
  colorMap,
  textColorMap,
}: {
  data: { label: string; count: number }[];
  colorMap: Record<string, string>;
  textColorMap: Record<string, string>;
}) {
  const total = data.reduce((sum, d) => sum + d.count, 0);
  if (total === 0) {
    return (
      <p className="text-sm text-muted-foreground italic">No data available</p>
    );
  }

  return (
    <div className="space-y-3">
      {/* Bar */}
      <div className="flex h-8 w-full overflow-hidden rounded-lg bg-muted/40">
        {data.map((d, i) => {
          const width = pct(d.count, total);
          if (width === 0) return null;
          const isFirst = i === 0 || data.slice(0, i).every((prev) => pct(prev.count, total) === 0);
          const isLast = i === data.length - 1 || data.slice(i + 1).every((next) => pct(next.count, total) === 0);
          return (
            <div
              key={d.label}
              className={`${colorMap[d.label] ?? "bg-muted-foreground"} relative flex items-center justify-center transition-all duration-500 ${isFirst ? "rounded-l-lg" : ""} ${isLast ? "rounded-r-lg" : ""}`}
              style={{ width: `${width}%`, minWidth: width > 0 ? "3px" : 0 }}
              title={`${d.label}: ${d.count} (${width.toFixed(1)}%)`}
            >
              {width > 10 && (
                <span className="truncate px-2 text-[11px] font-semibold text-white drop-shadow-sm">
                  {d.label}
                </span>
              )}
            </div>
          );
        })}
      </div>

      {/* Legend */}
      <div className="flex flex-wrap gap-x-5 gap-y-1.5">
        {data.map((d) => (
          <div key={d.label} className="flex items-center gap-2 text-xs">
            <span
              className={`inline-block h-2.5 w-2.5 shrink-0 rounded-full ${colorMap[d.label] ?? "bg-muted-foreground"}`}
            />
            <span
              className={`font-medium ${textColorMap[d.label] ?? "text-muted-foreground"}`}
            >
              {d.label}
            </span>
            <span className="tabular-nums text-muted-foreground">
              {d.count}
            </span>
            <span className="tabular-nums text-muted-foreground/60">
              ({pct(d.count, total).toFixed(1)}%)
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export function OverviewTab() {
  const files = useAppStore((s) => s.files);
  const stats = useAppStore((s) => s.stats);
  const selectedLibraryId = useAppStore((s) => s.selectedLibraryId);
  const libraryPaths = useAppStore((s) => s.library_paths);

  // Resolve selected library path string
  const selectedLibraryPath = useMemo(() => {
    if (!selectedLibraryId) return null;
    return libraryPaths.find((lp) => lp.id === selectedLibraryId)?.path ?? null;
  }, [selectedLibraryId, libraryPaths]);

  // Filtered files
  const filteredFiles = useMemo(() => {
    if (!selectedLibraryPath) return files;
    return files.filter((f) => f.library_path === selectedLibraryPath);
  }, [files, selectedLibraryPath]);

  // Computed stats (from filtered files, not the global stats object)
  const computed = useMemo(() => {
    const totalFiles = filteredFiles.length;
    const totalSize = filteredFiles.reduce((s, f) => s + f.size_bytes, 0);
    const savedBytes = filteredFiles.reduce((s, f) => {
      if (f.status === "complete" && f.output_size_bytes !== null) {
        return s + (f.size_bytes - f.output_size_bytes);
      }
      return s;
    }, 0);
    const completeCount = filteredFiles.filter(
      (f) => f.status === "complete" || f.status === "skipped",
    ).length;
    const completionPct = totalFiles === 0 ? 0 : (completeCount / totalFiles) * 100;

    return { totalFiles, totalSize, savedBytes, completionPct };
  }, [filteredFiles]);

  // Use global stats when viewing all libraries (more accurate mock data)
  const displayStats = useMemo(() => {
    if (!selectedLibraryId) {
      return {
        totalFiles: stats.total_files,
        totalSize: stats.total_size_bytes,
        savedBytes: stats.saved_bytes,
        completionPct:
          stats.total_files === 0
            ? 0
            : ((stats.complete + stats.skipped) / stats.total_files) * 100,
      };
    }
    return computed;
  }, [selectedLibraryId, stats, computed]);

  // Video codec distribution
  const videoCodecs = useMemo(() => {
    const counts = new Map<string, number>();
    for (const f of filteredFiles) {
      const codec = f.video_codec ?? "unknown";
      counts.set(codec, (counts.get(codec) ?? 0) + 1);
    }
    return Array.from(counts.entries())
      .map(([label, count]) => ({ label, count }))
      .sort((a, b) => b.count - a.count);
  }, [filteredFiles]);

  // Audio codec distribution
  const audioCodecs = useMemo(() => {
    const counts = new Map<string, number>();
    for (const f of filteredFiles) {
      const codec = f.audio_codec ?? "unknown";
      counts.set(codec, (counts.get(codec) ?? 0) + 1);
    }
    return Array.from(counts.entries())
      .map(([label, count]) => ({ label, count }))
      .sort((a, b) => b.count - a.count);
  }, [filteredFiles]);

  // Resolution distribution
  const resolutions = useMemo(() => {
    const groups = new Map<string, number>();
    for (const f of filteredFiles) {
      let group = "Other";
      if (f.resolution) {
        const w = parseInt(f.resolution.split("x")[0], 10);
        if (w >= 3840) group = "4K";
        else if (w >= 1920) group = "1080p";
        else if (w >= 1280) group = "720p";
      }
      groups.set(group, (groups.get(group) ?? 0) + 1);
    }
    // Fixed order
    const order = ["4K", "1080p", "720p", "Other"];
    return order
      .filter((k) => groups.has(k))
      .map((label) => ({ label, count: groups.get(label)! }));
  }, [filteredFiles]);

  // Status breakdown (use global stats if no library filter)
  const statusBreakdown = useMemo(() => {
    if (!selectedLibraryId) {
      return [
        { label: "pending", count: stats.pending },
        { label: "queued", count: stats.queued },
        { label: "transcoding", count: stats.transcoding },
        { label: "complete", count: stats.complete },
        { label: "skipped", count: stats.skipped },
        { label: "error", count: stats.errored },
      ];
    }
    const counts = new Map<string, number>();
    for (const f of filteredFiles) {
      counts.set(f.status, (counts.get(f.status) ?? 0) + 1);
    }
    return [
      "pending",
      "queued",
      "transcoding",
      "complete",
      "skipped",
      "error",
    ].map((label) => ({ label, count: counts.get(label) ?? 0 }));
  }, [selectedLibraryId, stats, filteredFiles]);

  const selectedLibName = selectedLibraryPath
    ? selectedLibraryPath.split("/").pop()
    : null;

  const statusTotal = statusBreakdown.reduce((sum, s) => sum + s.count, 0);

  return (
    <div className="space-y-6 px-6 py-6 animate-fade-up overflow-hidden">
      {/* Header */}
      <div className="flex items-baseline gap-2">
        <h2 className="font-heading text-2xl tracking-tight text-foreground">
          Overview
        </h2>
        {selectedLibName && (
          <span className="text-sm text-muted-foreground">
            / {selectedLibName}
          </span>
        )}
      </div>

      {/* Top summary: Completion ring + stats */}
      <Card>
        <CardContent className="flex flex-col items-center gap-6 sm:flex-row sm:items-center sm:gap-10 pt-2">
          <CircularProgress value={displayStats.completionPct} />
          <div className="flex flex-1 flex-wrap items-center justify-center gap-8 sm:justify-start">
            <StatItem
              label="Files"
              value={displayStats.totalFiles.toLocaleString()}
            />
            <StatItem
              label="Total Size"
              value={formatBytes(displayStats.totalSize)}
            />
            <StatItem
              label="Space Saved"
              value={formatBytes(displayStats.savedBytes)}
              valueClass="text-emerald-500"
            />
          </div>
        </CardContent>
      </Card>

      {/* Distribution sections */}
      <div className="grid gap-6 lg:grid-cols-2">
        {/* Video codecs */}
        <Card>
          <CardHeader>
            <CardTitle>Video Codecs</CardTitle>
          </CardHeader>
          <CardContent>
            <DistributionBar
              data={videoCodecs}
              colorMap={VIDEO_COLORS}
              textColorMap={VIDEO_TEXT_COLORS}
            />
          </CardContent>
        </Card>

        {/* Audio codecs */}
        <Card>
          <CardHeader>
            <CardTitle>Audio Codecs</CardTitle>
          </CardHeader>
          <CardContent>
            <DistributionBar
              data={audioCodecs}
              colorMap={AUDIO_COLORS}
              textColorMap={AUDIO_TEXT_COLORS}
            />
          </CardContent>
        </Card>

        {/* Resolution */}
        <Card>
          <CardHeader>
            <CardTitle>Resolution</CardTitle>
          </CardHeader>
          <CardContent>
            <DistributionBar
              data={resolutions}
              colorMap={RESOLUTION_COLORS}
              textColorMap={RESOLUTION_TEXT_COLORS}
            />
          </CardContent>
        </Card>

        {/* Status breakdown */}
        <Card>
          <CardHeader>
            <CardTitle>Status</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="space-y-2.5">
              {statusBreakdown.map((s) => (
                <div
                  key={s.label}
                  className="flex items-center gap-3 text-sm"
                >
                  <span
                    className={`inline-block h-2.5 w-2.5 shrink-0 rounded-full ${STATUS_COLORS[s.label] ?? "bg-muted-foreground"}`}
                  />
                  <span className="capitalize text-foreground flex-1">
                    {s.label}
                  </span>
                  <span
                    className={`shrink-0 tabular-nums font-medium ${STATUS_TEXT_COLORS[s.label] ?? "text-muted-foreground"}`}
                  >
                    {s.count.toLocaleString()}
                  </span>
                  <span className="shrink-0 w-12 text-right tabular-nums text-muted-foreground/60 text-xs">
                    {statusTotal > 0 ? `${pct(s.count, statusTotal).toFixed(0)}%` : "0%"}
                  </span>
                </div>
              ))}
            </div>
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
