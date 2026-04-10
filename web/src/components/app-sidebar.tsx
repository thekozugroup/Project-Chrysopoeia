"use client";

import { useState, useMemo } from "react";
import {
  Folder,
  FolderOpen,
  Plus,
  X,
  Cpu,
  Zap,
  ZapOff,
} from "lucide-react";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
} from "@/components/ui/sidebar";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { useAppStore } from "@/lib/store";

function formatBytes(bytes: number): string {
  if (bytes >= 1e12) return `${(bytes / 1e12).toFixed(1)} TB`;
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  return `${(bytes / 1e6).toFixed(0)} MB`;
}

export function AppSidebar() {
  const libraryPaths = useAppStore((s) => s.library_paths);
  const addLibraryPath = useAppStore((s) => s.addLibraryPath);
  const removeLibraryPath = useAppStore((s) => s.removeLibraryPath);
  const selectedLibraryId = useAppStore((s) => s.selectedLibraryId);
  const setSelectedLibrary = useAppStore((s) => s.setSelectedLibrary);
  const hardware = useAppStore((s) => s.hardware);
  const files = useAppStore((s) => s.files);
  const [newPath, setNewPath] = useState("");

  // Compute per-library stats
  const libStats = useMemo(() => {
    const out: Record<string, { pct: number; origBytes: number; newBytes: number }> = {};
    for (const lp of libraryPaths) {
      const pathFiles = files.filter((f) => f.library_path === lp.path);
      if (pathFiles.length === 0) {
        out[lp.id] = { pct: 0, origBytes: lp.total_size_bytes, newBytes: lp.total_size_bytes };
      } else {
        const done = pathFiles.filter(
          (f) => f.status === "complete" || f.status === "skipped",
        ).length;
        const origBytes = pathFiles.reduce((a, f) => a + f.size_bytes, 0);
        const newBytes = pathFiles.reduce((a, f) => {
          if (f.status === "complete" && f.output_size_bytes != null) return a + f.output_size_bytes;
          return a + f.size_bytes;
        }, 0);
        out[lp.id] = {
          pct: pathFiles.length > 0 ? Math.round((done / pathFiles.length) * 100) : 0,
          origBytes,
          newBytes,
        };
      }
    }
    return out;
  }, [files, libraryPaths]);

  function handleAddPath() {
    const p = newPath.trim();
    if (p && !libraryPaths.some((lp) => lp.path === p)) {
      addLibraryPath(p);
      setNewPath("");
    }
  }

  return (
    <Sidebar className="border-r border-sidebar-border">
      <SidebarHeader className="p-4">
        <div className="flex items-center gap-2.5">
          <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-gold/10">
            <svg
              viewBox="0 0 24 24"
              className="h-4.5 w-4.5 text-gold"
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
          <div>
            <h1 className="font-heading text-lg font-normal tracking-tight">
              Chrysopeia
            </h1>
            <p className="text-[10px] text-muted-foreground">
              Media Transmuter
            </p>
          </div>
        </div>
      </SidebarHeader>

      <SidebarContent>
        <SidebarGroup>
          <SidebarGroupLabel className="text-[10px] uppercase tracking-widest text-muted-foreground/60">
            Libraries
          </SidebarGroupLabel>
          <SidebarGroupContent>
            <div className="px-2">
              {libraryPaths.map((lp) => {
                const isSelected = selectedLibraryId === lp.id;
                const st = libStats[lp.id] ?? { pct: 0, origBytes: 0, newBytes: 0 };
                const compressionPct = st.origBytes > 0 ? Math.round((st.newBytes / st.origBytes) * 100) : 100;

                return (
                  <button
                    key={lp.id}
                    type="button"
                    onClick={() => setSelectedLibrary(isSelected ? null : lp.id)}
                    className={`group flex w-full flex-col rounded-lg px-3 py-2.5 text-left transition-colors mb-1 ${
                      isSelected
                        ? "bg-secondary/70"
                        : "hover:bg-secondary/30"
                    } ${!lp.enabled ? "opacity-40" : ""}`}
                  >
                    {/* Path name */}
                    <div className="flex items-center justify-between w-full">
                      <span
                        className={`text-sm truncate ${
                          isSelected ? "text-foreground font-medium" : "text-foreground/80"
                        } ${!lp.enabled ? "line-through" : ""}`}
                      >
                        {lp.path.split("/").pop() || lp.path}
                      </span>
                      <button
                        type="button"
                        onClick={(e) => {
                          e.stopPropagation();
                          removeLibraryPath(lp.id);
                        }}
                        className="shrink-0 text-muted-foreground/30 opacity-0 transition-opacity hover:text-destructive group-hover:opacity-100 ml-2"
                        aria-label={`Remove ${lp.path}`}
                      >
                        <X className="h-3 w-3" />
                      </button>
                    </div>

                    {/* Full path */}
                    <span className="text-[10px] text-muted-foreground/50 font-mono truncate mt-0.5">
                      {lp.path}
                    </span>

                    {/* Size bar: original size as full bar, compressed overlay on top */}
                    <div className="mt-2 w-full">
                      <div className="relative h-1.5 w-full rounded-full bg-muted-foreground/10 overflow-hidden">
                        {/* Compressed size overlay */}
                        <div
                          className="absolute inset-y-0 left-0 rounded-full bg-gold/50 transition-all duration-500"
                          style={{ width: `${compressionPct}%` }}
                        />
                      </div>
                      <div className="flex items-center justify-between mt-1">
                        <span className="text-[9px] text-muted-foreground/50 tabular-nums">
                          {formatBytes(st.origBytes)}
                        </span>
                        {st.origBytes !== st.newBytes && (
                          <span className="text-[9px] text-gold/70 tabular-nums">
                            {formatBytes(st.newBytes)}
                          </span>
                        )}
                      </div>
                    </div>

                    {/* Format badges */}
                    <div className="mt-1.5 flex items-center gap-1 text-[9px] text-muted-foreground/50">
                      <span className="uppercase">{lp.transcode.output_video}</span>
                      <span>/</span>
                      <span className="uppercase">{lp.transcode.output_audio}</span>
                      <span>/</span>
                      <span>.{lp.transcode.output_container}</span>
                    </div>
                  </button>
                );
              })}

              {/* Add library input */}
              <div className="flex gap-1.5 pt-2">
                <div className="relative flex-1">
                  <Folder className="absolute left-2 top-1/2 h-3 w-3 -translate-y-1/2 text-muted-foreground/50" />
                  <Input
                    value={newPath}
                    onChange={(e) => setNewPath(e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && handleAddPath()}
                    placeholder="/path/to/media"
                    className="h-7 pl-7 font-mono text-[11px]"
                  />
                </div>
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={handleAddPath}
                  className="h-7 w-7 shrink-0 p-0"
                >
                  <Plus className="h-3 w-3" />
                </Button>
              </div>
            </div>
          </SidebarGroupContent>
        </SidebarGroup>
      </SidebarContent>

      <SidebarFooter className="p-3">
        <Tooltip>
          <TooltipTrigger className="flex w-full items-center gap-2 rounded-md bg-secondary/30 px-3 py-2 text-left">
            <Cpu className="h-3.5 w-3.5 text-gold-muted" />
            <span className="flex-1 truncate text-xs text-muted-foreground">
              {hardware.gpu_name ?? "CPU Only"}
            </span>
            {hardware.gpu_name ? (
              <Zap className="h-3 w-3 text-emerald-400" />
            ) : (
              <ZapOff className="h-3 w-3 text-muted-foreground" />
            )}
          </TooltipTrigger>
          <TooltipContent side="top" className="max-w-64">
            <p className="text-xs font-medium">{hardware.gpu_name}</p>
            <p className="text-[10px] text-muted-foreground">
              {hardware.cpu_cores} cores &middot; {hardware.ram_gb} GB RAM
            </p>
          </TooltipContent>
        </Tooltip>
        <p className="mt-1.5 text-center text-[10px] text-muted-foreground/40">
          v0.1.0 &middot; oximedia
        </p>
      </SidebarFooter>
    </Sidebar>
  );
}
