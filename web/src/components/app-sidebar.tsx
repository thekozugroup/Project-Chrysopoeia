"use client";

import { useState } from "react";
import {
  Folder,
  Plus,
  X,
  Cpu,
  MonitorSmartphone,
  MemoryStick,
  Check,
} from "lucide-react";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarHeader,
} from "@/components/ui/sidebar";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
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
  const isAddingLibrary = useAppStore((s) => s.isAddingLibrary);
  const setIsAddingLibrary = useAppStore((s) => s.setIsAddingLibrary);
  const [newPath, setNewPath] = useState("");

  function handleAddPath() {
    const p = newPath.trim();
    if (p && !libraryPaths.some((lp) => lp.path === p)) {
      addLibraryPath(p);
      setNewPath("");
      setIsAddingLibrary(false);
    }
  }

  function handleCancelAdd() {
    setNewPath("");
    setIsAddingLibrary(false);
  }

  // Compute per-library compression stats
  function getLibStats(lp: (typeof libraryPaths)[0]) {
    const pathFiles = files.filter((f) => f.library_path === lp.path);
    if (pathFiles.length === 0) {
      return { origBytes: lp.total_size_bytes, newBytes: lp.total_size_bytes };
    }
    const origBytes = pathFiles.reduce((a, f) => a + f.size_bytes, 0);
    const newBytes = pathFiles.reduce((a, f) => {
      if (f.status === "complete" && f.output_size_bytes != null) return a + f.output_size_bytes;
      return a + f.size_bytes;
    }, 0);
    return { origBytes, newBytes };
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
          <h1 className="font-heading text-xl font-normal tracking-tight">
            Chrysopoeia
          </h1>
        </div>
      </SidebarHeader>

      <SidebarContent>
        <SidebarGroup>
          {/* Libraries header with + button */}
          <div className="flex items-center justify-between px-4 py-1">
            <span className="text-[10px] uppercase tracking-widest text-muted-foreground/60 font-medium">
              Libraries
            </span>
            <Button
              type="button"
              size="sm"
              variant="ghost"
              onClick={() => setIsAddingLibrary(true)}
              className="h-5 w-5 p-0 text-muted-foreground/50 hover:text-foreground"
              aria-label="Add library"
            >
              <Plus className="h-3.5 w-3.5" />
            </Button>
          </div>

          <SidebarGroupContent>
            <div className="px-2">
              {/* Inline add-library card */}
              {isAddingLibrary && (
                <div className="rounded-lg border border-gold/30 bg-secondary/50 px-3 py-2.5 mb-1 animate-fade-up">
                  <Input
                    autoFocus
                    value={newPath}
                    onChange={(e) => setNewPath(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") handleAddPath();
                      if (e.key === "Escape") handleCancelAdd();
                    }}
                    placeholder="/path/to/media"
                    className="h-7 font-mono text-sm border-0 bg-transparent px-0 focus-visible:ring-0 placeholder:text-muted-foreground/40"
                  />
                  <div className="flex items-center justify-end gap-1.5 mt-2">
                    <Button
                      type="button"
                      size="sm"
                      variant="ghost"
                      onClick={handleCancelAdd}
                      className="h-6 px-2 text-[10px] text-muted-foreground"
                    >
                      Cancel
                    </Button>
                    <Button
                      type="button"
                      size="sm"
                      onClick={handleAddPath}
                      disabled={!newPath.trim()}
                      className="h-6 px-2 text-[10px] bg-gold text-gold-foreground hover:bg-gold/90 disabled:opacity-30"
                    >
                      <Check className="h-3 w-3 mr-1" />
                      Add
                    </Button>
                  </div>
                </div>
              )}

              {/* Library cards */}
              {libraryPaths.map((lp) => {
                const isSelected = selectedLibraryId === lp.id;
                const st = getLibStats(lp);
                const compressionPct = st.origBytes > 0 ? Math.round((st.newBytes / st.origBytes) * 100) : 100;

                return (
                  <button
                    key={lp.id}
                    type="button"
                    aria-label={`Library: ${lp.path.split("/").pop() || lp.path}`}
                    aria-pressed={isSelected}
                    onClick={() => setSelectedLibrary(isSelected ? null : lp.id)}
                    className={`group flex w-full flex-col rounded-lg px-3 py-2 text-left transition-colors mb-0.5 ${
                      isSelected
                        ? "bg-secondary/70"
                        : "hover:bg-secondary/30"
                    } ${!lp.enabled ? "opacity-40" : ""}`}
                  >
                    {/* Name + remove */}
                    <div className="flex items-center justify-between w-full">
                      <span
                        className={`text-sm ${
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

                    {/* Size bar */}
                    <div className="mt-1.5 w-full">
                      <div className="relative h-1.5 w-full rounded-full bg-muted-foreground/10 overflow-hidden">
                        <div
                          className="absolute inset-y-0 left-0 rounded-full bg-gold/50 transition-all duration-500"
                          style={{ width: `${compressionPct}%` }}
                        />
                      </div>
                      <div className="flex items-center justify-between mt-0.5">
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

                    {/* Format line */}
                    <span className="mt-0.5 text-[8px] uppercase tracking-wide text-muted-foreground/35 font-mono">
                      {lp.transcode.output_video} / {lp.transcode.output_audio} / .{lp.transcode.output_container}
                    </span>
                  </button>
                );
              })}
            </div>
          </SidebarGroupContent>
        </SidebarGroup>
      </SidebarContent>

      <SidebarFooter className="px-4 py-3 space-y-1.5">
        {hardware.gpu_name && (
          <div className="flex items-center gap-2 text-[11px] text-muted-foreground/70">
            <MonitorSmartphone className="h-3 w-3 shrink-0 text-muted-foreground/40" />
            <span className="truncate">{hardware.gpu_name}</span>
          </div>
        )}
        <div className="flex items-center gap-2 text-[11px] text-muted-foreground/70">
          <Cpu className="h-3 w-3 shrink-0 text-muted-foreground/40" />
          <span>{hardware.cpu_cores} cores</span>
        </div>
        <div className="flex items-center gap-2 text-[11px] text-muted-foreground/70">
          <MemoryStick className="h-3 w-3 shrink-0 text-muted-foreground/40" />
          <span>{hardware.ram_gb} GB</span>
        </div>
      </SidebarFooter>
    </Sidebar>
  );
}
