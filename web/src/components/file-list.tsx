"use client";

import { useState, useCallback, useMemo } from "react";
import {
  Clock,
  Check,
  CheckCircle2,
  AlertTriangle,
  Loader2,
  SkipForward,
  FolderOpen,
  Play,
  Ban,
  X,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { useAppStore } from "@/lib/store";
import type { MediaFile } from "@/lib/types";

function formatBytes(bytes: number): string {
  if (bytes >= 1e12) return `${(bytes / 1e12).toFixed(1)} TB`;
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  return `${(bytes / 1e6).toFixed(0)} MB`;
}

function formatBytesCompact(bytes: number): string {
  if (bytes >= 1e12) return `${(bytes / 1e12).toFixed(1)}T`;
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)}G`;
  if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(0)}M`;
  return `${(bytes / 1e3).toFixed(0)}K`;
}

function formatDuration(secs: number): string {
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

function formatEta(secs: number | null): string {
  if (secs == null) return "";
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (h > 0) return `${h}h ${m}m left`;
  return `${m}m left`;
}

function parseFilename(filename: string): { title: string; meta: string } {
  // Try to extract a clean title from common naming conventions
  const noExt = filename.replace(/\.[^.]+$/, "");
  const parts = noExt.split(".");
  // Find year pattern
  const yearIdx = parts.findIndex((p) => /^(19|20)\d{2}$/.test(p));
  if (yearIdx > 0) {
    const title = parts.slice(0, yearIdx).join(" ");
    const meta = parts.slice(yearIdx).join(" ");
    return { title, meta };
  }
  // Find season pattern S01E01
  const seasonIdx = parts.findIndex((p) => /^S\d{2}E\d{2}$/i.test(p));
  if (seasonIdx > 0) {
    const showName = parts.slice(0, seasonIdx).join(" ");
    const episode = parts[seasonIdx];
    const rest = parts.slice(seasonIdx + 1).join(" ");
    return { title: `${showName} ${episode}`, meta: rest };
  }
  return { title: noExt.replace(/\./g, " "), meta: "" };
}

const STATUS_CONFIG: Record<
  MediaFile["status"],
  { icon: typeof Clock; color: string; label: string }
> = {
  pending: { icon: Clock, color: "text-muted-foreground", label: "Pending" },
  queued: { icon: Clock, color: "text-amber-400", label: "Queued" },
  transcoding: { icon: Loader2, color: "text-emerald-400", label: "Transcoding" },
  complete: { icon: Check, color: "text-gold", label: "Complete" },
  skipped: { icon: SkipForward, color: "text-muted-foreground", label: "Skipped" },
  error: { icon: AlertTriangle, color: "text-destructive", label: "Error" },
};

// Open (patent-free) codecs
const OPEN_CODECS = new Set(["av1", "vp9", "vp8", "theora", "opus", "vorbis", "flac"]);

function FileRow({ file, index, selected, onToggle }: { file: MediaFile; index: number; selected: boolean; onToggle: (id: string) => void }) {
  const staggerClass = index <= 5 ? `stagger-${Math.min(index, 5)}` : "stagger-5";
  const libraryPaths = useAppStore((s) => s.library_paths);
  const globalSettings = useAppStore((s) => s.globalSettings);
  const lib = libraryPaths.find((lp) => lp.path === file.library_path);
  const transcode = lib?.transcode ?? {
    output_video: globalSettings.default_video,
    output_audio: globalSettings.default_audio,
    output_container: globalSettings.default_container,
    crf: globalSettings.default_crf,
    skip_open_formats: globalSettings.default_skip_open,
  };
  const statusCfg = STATUS_CONFIG[file.status];
  const StatusIcon = statusCfg.icon;
  const { title, meta } = parseFilename(file.filename);
  const isOpen = file.video_codec ? OPEN_CODECS.has(file.video_codec) : false;

  const sizeReduction =
    file.output_size_bytes != null
      ? Math.round((1 - file.output_size_bytes / file.size_bytes) * 100)
      : null;

  const rowStatusClass =
    file.status === "transcoding"
      ? "bg-emerald-400/[0.03] border-l-2 border-l-gold"
      : file.status === "queued"
        ? "border-l-2 border-l-amber-400/60"
        : file.status === "error"
          ? "bg-destructive/[0.04] border-l-2 border-l-destructive/50"
          : "hover:border-l-2 hover:border-l-gold/40 hover:pl-[10px] md:hover:pl-[22px]";

  return (
    <div
      role="row"
      aria-label={`${title} - ${statusCfg.label}`}
      className={`animate-fade-up ${staggerClass} group border-b border-border/40 px-3 md:px-6 py-3 transition-all duration-200 hover:bg-secondary/20 ${rowStatusClass} ${selected ? "bg-gold/[0.04]" : ""}`}
    >
      <div className="flex items-center gap-2 md:gap-4">
        {/* Checkbox */}
        <div className="shrink-0 w-4">
          <button
            type="button"
            role="checkbox"
            aria-checked={selected}
            onClick={(e) => { e.stopPropagation(); onToggle(file.id); }}
            className={`h-3.5 w-3.5 rounded-sm border transition-colors flex items-center justify-center ${
              selected
                ? "bg-gold border-gold text-gold-foreground"
                : "border-muted-foreground/30 hover:border-muted-foreground/60"
            }`}
          >
            {selected && <Check className="h-2.5 w-2.5" />}
          </button>
        </div>

        {/* Status indicator — circular progress for transcoding, icon for others */}
        <div role="cell" className="shrink-0 w-5">
          {file.status === "transcoding" ? (
            <svg className="h-5 w-5 -rotate-90" viewBox="0 0 20 20" aria-label={`Transcoding ${file.progress}%`}>
              <circle cx="10" cy="10" r="8" fill="none" stroke="currentColor" strokeWidth="2" className="text-muted-foreground/15" />
              <circle
                cx="10" cy="10" r="8" fill="none" stroke="currentColor" strokeWidth="2"
                className="text-emerald-400 transition-all duration-500"
                strokeLinecap="round"
                strokeDasharray={`${2 * Math.PI * 8}`}
                strokeDashoffset={`${2 * Math.PI * 8 * (1 - file.progress / 100)}`}
              />
            </svg>
          ) : (
            <StatusIcon
              aria-label={statusCfg.label}
              className={`h-4 w-4 ${statusCfg.color}`}
            />
          )}
        </div>

        {/* File info */}
        <div role="cell" className="min-w-0 flex-1">
          <div className="flex items-baseline gap-2">
            <p className="text-sm font-medium truncate">{title}</p>
            {meta && (
              <p className="text-[10px] text-muted-foreground truncate">
                {meta}
              </p>
            )}
          </div>

          {/* Codec badges + conversion arrow */}
          <div className="mt-1 flex items-center gap-1.5 text-[10px]">
            {file.video_codec && (
              <Badge
                variant="secondary"
                className={`px-1.5 py-0 font-mono uppercase text-[9px] ${
                  OPEN_CODECS.has(file.video_codec)
                    ? "text-emerald-400/80 bg-emerald-400/10"
                    : "text-amber-400/80 bg-amber-400/10"
                }`}
              >
                {file.video_codec}
              </Badge>
            )}
            {file.audio_codec && (
              <Badge
                variant="secondary"
                className={`px-1.5 py-0 font-mono uppercase text-[9px] ${
                  OPEN_CODECS.has(file.audio_codec)
                    ? "text-emerald-400/80 bg-emerald-400/10"
                    : "text-amber-400/80 bg-amber-400/10"
                }`}
              >
                {file.audio_codec}
              </Badge>
            )}
            {!isOpen && file.status !== "skipped" && (
              <>
                <span className="text-[10px] text-gold/50 font-medium mx-0.5">&rarr;</span>
                <Badge
                  variant="secondary"
                  className="px-1.5 py-0 font-mono uppercase text-[9px] text-gold bg-gold/10"
                >
                  {transcode.output_video}
                </Badge>
                <Badge
                  variant="secondary"
                  className="px-1.5 py-0 font-mono uppercase text-[9px] text-gold bg-gold/10"
                >
                  {transcode.output_audio}
                </Badge>
              </>
            )}
            {file.status === "skipped" && (
              <span className="text-[10px] text-muted-foreground/50 italic">
                already open format
              </span>
            )}
          </div>
        </div>

        {/* Resolution -- hidden below md */}
        <div role="cell" className="hidden md:block shrink-0 w-20 text-right">
          <p className="text-[11px] tabular-nums text-muted-foreground">
            {file.resolution ?? "--"}
          </p>
        </div>

        {/* Duration */}
        <div role="cell" className="hidden md:block shrink-0 w-14 text-right">
          <p className="text-[11px] tabular-nums text-muted-foreground">
            {formatDuration(file.duration_secs)}
          </p>
        </div>

        {/* Size */}
        <div role="cell" className="shrink-0 w-20 text-right">
          {file.status === "complete" && file.output_size_bytes != null ? (
            <div>
              <p className="text-[11px] tabular-nums text-muted-foreground/50 line-through decoration-muted-foreground/30">
                {formatBytesCompact(file.size_bytes)}
              </p>
              <p className="text-[11px] tabular-nums text-gold font-medium">
                {formatBytesCompact(file.output_size_bytes)}
                {sizeReduction != null && sizeReduction > 0 && (
                  <span className="text-[9px] text-gold/70 ml-0.5">-{sizeReduction}%</span>
                )}
              </p>
            </div>
          ) : file.status === "transcoding" && file.progress > 0 ? (
            <div>
              <p className="text-[11px] tabular-nums text-muted-foreground">
                {formatBytesCompact(file.size_bytes)}
              </p>
              <p className="text-[9px] tabular-nums text-emerald-400/70">
                ~{formatBytesCompact(file.size_bytes * 0.65)}
              </p>
            </div>
          ) : (
            <p className="text-[11px] tabular-nums text-muted-foreground">
              {formatBytesCompact(file.size_bytes)}
            </p>
          )}
        </div>

        {/* Progress/status info */}
        <div role="cell" className="shrink-0 w-24 md:w-32 text-right">
          {file.status === "transcoding" && (
            <div className="text-[10px] tabular-nums text-muted-foreground">
              <span className="text-emerald-400 font-medium">{file.progress}%</span>
              {file.speed && (
                <span className="hidden md:inline ml-1.5">{file.speed}</span>
              )}
              {file.eta_secs != null && (
                <span className="hidden md:inline ml-1.5 text-muted-foreground/60">{formatEta(file.eta_secs)}</span>
              )}
            </div>
          )}
          {file.status === "complete" && sizeReduction != null && (
            <span className="text-[10px] tabular-nums text-gold">
              {formatBytes(file.size_bytes - (file.output_size_bytes ?? 0))} saved
            </span>
          )}
          {file.status === "error" && (
            <Tooltip>
              <TooltipTrigger>
                <span className="text-[10px] text-destructive">Error</span>
              </TooltipTrigger>
              <TooltipContent className="max-w-64">
                <p className="text-xs">{file.error_message}</p>
              </TooltipContent>
            </Tooltip>
          )}
        </div>
      </div>
    </div>
  );
}

export function FileList() {
  const allFiles = useAppStore((s) => s.files);
  const selectedLibraryId = useAppStore((s) => s.selectedLibraryId);
  const libraryPaths = useAppStore((s) => s.library_paths);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());

  // Filter by selected library
  const files = useMemo(() => {
    if (!selectedLibraryId) return allFiles;
    const lib = libraryPaths.find((lp) => lp.id === selectedLibraryId);
    if (!lib) return allFiles;
    return allFiles.filter((f) => f.library_path === lib.path);
  }, [allFiles, selectedLibraryId, libraryPaths]);

  const toggleSelect = useCallback((id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const clearSelection = useCallback(() => setSelectedIds(new Set()), []);

  // Sort: transcoding first, then queued, pending, error, complete, skipped
  const ORDER: Record<MediaFile["status"], number> = {
    transcoding: 0,
    queued: 1,
    error: 2,
    pending: 3,
    complete: 4,
    skipped: 5,
  };

  const sorted = [...files].sort(
    (a, b) => ORDER[a.status] - ORDER[b.status]
  );

  const selectedCount = selectedIds.size;

  return (
    <div className="relative min-w-0" role="table" aria-label="Media files">
      {/* Column headers */}
      <div role="row" className="sticky top-0 z-10 flex items-center gap-2 md:gap-4 border-b border-border bg-background/95 backdrop-blur-md px-3 md:px-6 py-2 shadow-[0_1px_2px_0_rgb(0_0_0/0.03)] will-change-transform">
        <div className="w-4" />
        <div role="columnheader" aria-label="Status" className="w-5" />
        <div role="columnheader" className="flex-1 text-[10px] text-muted-foreground/60 uppercase tracking-wider">
          File
        </div>
        <div role="columnheader" className="hidden md:block w-20 text-right text-[10px] text-muted-foreground/60 uppercase tracking-wider">
          Res
        </div>
        <div role="columnheader" className="hidden md:block w-14 text-right text-[10px] text-muted-foreground/60 uppercase tracking-wider">
          Dur
        </div>
        <div role="columnheader" className="w-20 text-right text-[10px] text-muted-foreground/60 uppercase tracking-wider">
          Size
        </div>
        <div role="columnheader" className="w-24 md:w-32 text-right text-[10px] text-muted-foreground/60 uppercase tracking-wider">
          Status
        </div>
      </div>

      {/* Virtualization point: for large file lists (1000+), wrap sorted.map in a virtual scroller like @tanstack/react-virtual */}
      {sorted.map((file, index) => (
        <FileRow
          key={file.id}
          file={file}
          index={index}
          selected={selectedIds.has(file.id)}
          onToggle={toggleSelect}
        />
      ))}

      {files.length === 0 && (
        <div className="flex flex-col items-center justify-center py-24 text-center gap-3">
          <FolderOpen className="h-10 w-10 text-muted-foreground/30" />
          <div>
            <p className="text-sm text-muted-foreground">No files found</p>
            <p className="text-xs text-muted-foreground/60 mt-1">
              Add library paths in the sidebar to get started
            </p>
          </div>
        </div>
      )}

      {files.length > 0 && sorted.every((f) => f.status === "complete" || f.status === "skipped") && (
        <div className="flex flex-col items-center justify-center py-12 text-center gap-3">
          <div className="relative">
            <CheckCircle2 className="h-8 w-8 text-gold" />
            <div className="absolute -inset-2 rounded-full bg-gold/10 animate-ping" style={{ animationDuration: "2s" }} />
          </div>
          <div>
            <p className="text-sm font-medium text-gold">Library fully converted</p>
            <p className="text-xs text-muted-foreground/60 mt-1">
              All files have been processed or skipped
            </p>
          </div>
        </div>
      )}

      {/* Floating bulk actions bar */}
      <div
        className={`fixed bottom-6 left-1/2 -translate-x-1/2 z-50 transition-all duration-300 ${
          selectedCount > 0
            ? "translate-y-0 opacity-100"
            : "translate-y-8 opacity-0 pointer-events-none"
        }`}
      >
        <div className="flex items-center gap-3 rounded-xl border border-border bg-card/95 backdrop-blur-md px-5 py-3 shadow-lg shadow-black/20">
          <span className="text-sm font-medium tabular-nums">
            {selectedCount} selected
          </span>
          <div className="h-4 w-px bg-border" />
          <Button size="sm" className="gap-1.5 bg-gold text-gold-foreground hover:bg-gold/80 text-xs h-7">
            <Play className="h-3 w-3" />
            Transcode
          </Button>
          <Button size="sm" variant="secondary" className="gap-1.5 text-xs h-7">
            <SkipForward className="h-3 w-3" />
            Skip
          </Button>
          <Button size="sm" variant="secondary" className="gap-1.5 text-xs text-destructive h-7">
            <Ban className="h-3 w-3" />
            Cancel
          </Button>
          <div className="h-4 w-px bg-border" />
          <button
            type="button"
            onClick={clearSelection}
            className="text-muted-foreground hover:text-foreground transition-colors"
            aria-label="Clear selection"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>
    </div>
  );
}
