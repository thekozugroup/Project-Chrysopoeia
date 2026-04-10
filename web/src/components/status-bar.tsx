"use client";

import { useAppStore } from "@/lib/store";

function formatBytes(bytes: number): string {
  if (bytes >= 1e12) return `${(bytes / 1e12).toFixed(1)} TB`;
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  return `${(bytes / 1e6).toFixed(0)} MB`;
}

function formatEtaCompact(files: { eta_secs: number | null }[]): string {
  const maxEta = files.reduce(
    (max, f) => (f.eta_secs != null && f.eta_secs > max ? f.eta_secs : max),
    0,
  );
  if (maxEta === 0) return "";
  const h = Math.floor(maxEta / 3600);
  const m = Math.floor((maxEta % 3600) / 60);
  if (h > 0) return `~${h}h ${m}m remaining`;
  return `~${m}m remaining`;
}

function aggregateSpeed(
  files: { speed: string | null; status: string }[],
): string {
  const speeds = files
    .filter((f) => f.status === "transcoding" && f.speed)
    .map((f) => parseFloat(f.speed!));
  if (speeds.length === 0) return "";
  const avg = speeds.reduce((a, b) => a + b, 0) / speeds.length;
  return `${avg.toFixed(1)}x`;
}

export function StatusBar() {
  const stats = useAppStore((s) => s.stats);
  const files = useAppStore((s) => s.files);
  const wsConnected = useAppStore((s) => s.wsConnected);

  const activeFiles = files.filter((f) => f.status === "transcoding");
  const speed = aggregateSpeed(files);
  const eta = formatEtaCompact(activeFiles);
  const processing = stats.transcoding + stats.queued;

  return (
    <footer className="flex h-6 shrink-0 items-center border-t border-border bg-sidebar px-4 md:px-6 text-[10px] font-mono text-muted-foreground/60 select-none gap-4 overflow-hidden">
      {/* Connection status */}
      <span className="flex items-center gap-1.5 shrink-0">
        <span
          className={`inline-block h-1.5 w-1.5 rounded-full ${
            wsConnected
              ? "bg-emerald-400 status-pulse"
              : "bg-destructive"
          }`}
        />
        {wsConnected ? "Connected" : "Disconnected"}
      </span>

      <span className="h-3 w-px bg-border shrink-0" />

      {/* Processing summary */}
      {processing > 0 ? (
        <span className="truncate tabular-nums">
          Processing {stats.transcoding} of{" "}
          {stats.total_files.toLocaleString()} files
          {speed && ` \u00b7 ${speed}`}
          {eta && ` \u00b7 ${eta}`}
        </span>
      ) : (
        <span className="truncate">Idle</span>
      )}

      {/* Spacer */}
      <span className="flex-1" />

      {/* Saved bytes */}
      {stats.saved_bytes > 0 && (
        <span className="shrink-0 tabular-nums text-gold/50">
          {formatBytes(stats.saved_bytes)} saved
        </span>
      )}
    </footer>
  );
}
