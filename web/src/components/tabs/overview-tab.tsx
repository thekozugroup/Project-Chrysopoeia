"use client";

import { useMemo } from "react";
import { useAppStore } from "@/lib/store";

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

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

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
    <div className="space-y-2">
      {/* Bar */}
      <div className="flex h-7 w-full overflow-hidden rounded-md bg-muted/50">
        {data.map((d) => {
          const width = pct(d.count, total);
          if (width === 0) return null;
          return (
            <div
              key={d.label}
              className={`${colorMap[d.label] ?? "bg-muted-foreground"} relative flex items-center justify-center transition-all duration-500`}
              style={{ width: `${width}%`, minWidth: width > 0 ? "2px" : 0 }}
              title={`${d.label}: ${d.count} (${width.toFixed(1)}%)`}
            >
              {width > 8 && (
                <span className="truncate px-1.5 text-[11px] font-semibold text-white drop-shadow-sm">
                  {d.label}
                </span>
              )}
            </div>
          );
        })}
      </div>

      {/* Legend */}
      <div className="flex flex-wrap gap-x-4 gap-y-1">
        {data.map((d) => (
          <div key={d.label} className="flex items-center gap-1.5 text-xs">
            <span
              className={`inline-block h-2.5 w-2.5 rounded-sm ${colorMap[d.label] ?? "bg-muted-foreground"}`}
            />
            <span className={`font-medium ${textColorMap[d.label] ?? "text-muted-foreground"}`}>
              {d.label}
            </span>
            <span className="text-muted-foreground tabular-nums">
              {d.count}
            </span>
            <span className="text-muted-foreground/60 tabular-nums">
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

  return (
    <div className="space-y-6 animate-fade-up">
      {/* Header */}
      <div className="flex items-baseline gap-2">
        <h2 className="text-lg font-semibold text-foreground">Overview</h2>
        {selectedLibName && (
          <span className="text-sm text-muted-foreground">
            / {selectedLibName}
          </span>
        )}
      </div>

      {/* Stats row */}
      <div className="flex flex-wrap items-baseline gap-x-8 gap-y-2 text-sm">
        <div>
          <span className="text-muted-foreground">Files </span>
          <span className="font-semibold tabular-nums text-foreground">
            {displayStats.totalFiles.toLocaleString()}
          </span>
        </div>
        <div>
          <span className="text-muted-foreground">Total size </span>
          <span className="font-semibold tabular-nums text-foreground">
            {formatBytes(displayStats.totalSize)}
          </span>
        </div>
        <div>
          <span className="text-muted-foreground">Space saved </span>
          <span className="font-semibold tabular-nums text-emerald-500">
            {formatBytes(displayStats.savedBytes)}
          </span>
        </div>
        <div>
          <span className="text-muted-foreground">Complete </span>
          <span className="font-semibold tabular-nums text-foreground">
            {displayStats.completionPct.toFixed(1)}%
          </span>
        </div>
      </div>

      {/* Completion progress bar */}
      <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted/50">
        <div
          className="h-full rounded-full bg-emerald-500 transition-all duration-700"
          style={{ width: `${Math.min(displayStats.completionPct, 100)}%` }}
        />
      </div>

      {/* Distribution sections */}
      <div className="grid gap-6 lg:grid-cols-2">
        {/* Video codecs */}
        <section className="space-y-2">
          <h3 className="text-xs font-medium uppercase tracking-wider text-muted-foreground">
            Video Codecs
          </h3>
          <DistributionBar
            data={videoCodecs}
            colorMap={VIDEO_COLORS}
            textColorMap={VIDEO_TEXT_COLORS}
          />
        </section>

        {/* Audio codecs */}
        <section className="space-y-2">
          <h3 className="text-xs font-medium uppercase tracking-wider text-muted-foreground">
            Audio Codecs
          </h3>
          <DistributionBar
            data={audioCodecs}
            colorMap={AUDIO_COLORS}
            textColorMap={AUDIO_TEXT_COLORS}
          />
        </section>

        {/* Resolution */}
        <section className="space-y-2">
          <h3 className="text-xs font-medium uppercase tracking-wider text-muted-foreground">
            Resolution
          </h3>
          <DistributionBar
            data={resolutions}
            colorMap={RESOLUTION_COLORS}
            textColorMap={RESOLUTION_TEXT_COLORS}
          />
        </section>

        {/* Status breakdown */}
        <section className="space-y-2">
          <h3 className="text-xs font-medium uppercase tracking-wider text-muted-foreground">
            Status
          </h3>
          <div className="space-y-1.5">
            {statusBreakdown.map((s) => (
              <div
                key={s.label}
                className="flex items-center justify-between text-sm"
              >
                <div className="flex items-center gap-2">
                  <span
                    className={`inline-block h-2 w-2 rounded-full ${STATUS_COLORS[s.label] ?? "bg-muted-foreground"}`}
                  />
                  <span className="capitalize text-foreground">{s.label}</span>
                </div>
                <span className="tabular-nums text-muted-foreground">
                  {s.count.toLocaleString()}
                </span>
              </div>
            ))}
          </div>
        </section>
      </div>
    </div>
  );
}
