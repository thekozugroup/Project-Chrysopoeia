"use client";

import { useState, useMemo, useRef, useEffect } from "react";
import {
  ChevronUp,
  ChevronDown,
  Trash2,
  Search,
  X,
  AlertCircle,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { useAppStore } from "@/lib/store";
import type { LogEntry } from "@/lib/types";

type LogFilter = "all" | "errors" | "warnings";

function formatTime(timestamp: string): string {
  const d = new Date(timestamp);
  return d.toLocaleTimeString("en-US", {
    hour12: false,
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

const LEVEL_DOT: Record<LogEntry["level"], string> = {
  success: "bg-emerald-400",
  info: "bg-blue-400",
  warn: "bg-yellow-400",
  error: "bg-red-400",
};

export function ActivityLog({
  isOpen,
  onToggle,
}: {
  isOpen: boolean;
  onToggle: () => void;
}) {
  const logEntries = useAppStore((s) => s.logEntries);
  const clearLog = useAppStore((s) => s.clearLog);

  const [filter, setFilter] = useState<LogFilter>("all");
  const [search, setSearch] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);
  const [prevCount, setPrevCount] = useState(logEntries.length);

  // Track new entries for animation
  const [animatingIds, setAnimatingIds] = useState<Set<string>>(new Set());
  useEffect(() => {
    if (logEntries.length > prevCount) {
      const newIds = new Set(
        logEntries.slice(0, logEntries.length - prevCount).map((e) => e.id)
      );
      setAnimatingIds(newIds);
      const timer = setTimeout(() => setAnimatingIds(new Set()), 400);
      return () => clearTimeout(timer);
    }
    setPrevCount(logEntries.length);
  }, [logEntries.length, prevCount, logEntries]);

  const filtered = useMemo(() => {
    let entries = logEntries;
    if (filter === "errors") {
      entries = entries.filter((e) => e.level === "error");
    } else if (filter === "warnings") {
      entries = entries.filter(
        (e) => e.level === "warn" || e.level === "error"
      );
    }
    if (search.trim()) {
      const q = search.toLowerCase();
      entries = entries.filter((e) => e.message.toLowerCase().includes(q));
    }
    return entries;
  }, [logEntries, filter, search]);

  const filterPills: { key: LogFilter; label: string }[] = [
    { key: "all", label: "All" },
    { key: "errors", label: "Errors" },
    { key: "warnings", label: "Warnings" },
  ];

  return (
    <div
      className={`shrink-0 border-t border-border bg-card/80 backdrop-blur-sm transition-all duration-200 ${
        isOpen ? "h-[220px]" : "h-9"
      }`}
    >
      {/* Header bar */}
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-border/50 px-3">
        <button
          onClick={onToggle}
          aria-label={isOpen ? "Collapse activity log" : "Expand activity log"}
          aria-expanded={isOpen}
          className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground hover:text-foreground transition-colors"
        >
          {isOpen ? (
            <ChevronDown className="h-3 w-3" />
          ) : (
            <ChevronUp className="h-3 w-3" />
          )}
          <span>Activity</span>
        </button>

        <span className="text-[10px] tabular-nums text-muted-foreground/60">
          {logEntries.length} entries
        </span>

        {isOpen && (
          <>
            {/* Filter pills */}
            <div className="ml-2 flex items-center gap-1">
              {filterPills.map((p) => (
                <button
                  key={p.key}
                  onClick={() => setFilter(p.key)}
                  aria-label={`Filter: ${p.label}`}
                  aria-pressed={filter === p.key}
                  className={`rounded-full px-2 py-0.5 text-[10px] font-medium transition-colors ${
                    filter === p.key
                      ? "bg-gold/15 text-gold ring-1 ring-inset ring-gold/25"
                      : "text-muted-foreground/70 hover:text-muted-foreground hover:bg-secondary/60"
                  }`}
                >
                  {p.label}
                </button>
              ))}
            </div>

            {/* Search */}
            <div className="relative ml-auto flex items-center">
              <Search className="absolute left-1.5 h-3 w-3 text-muted-foreground/50" />
              <input
                type="text"
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder="Filter logs..."
                aria-label="Filter activity log entries"
                className="h-5 w-32 rounded-sm border border-border/50 bg-card pl-5 pr-5 text-[10px] text-foreground placeholder:text-muted-foreground/40 focus:outline-none focus:ring-1 focus:ring-gold/30"
              />
              {search && (
                <button
                  onClick={() => setSearch("")}
                  className="absolute right-1 text-muted-foreground/50 hover:text-muted-foreground"
                >
                  <X className="h-3 w-3" />
                </button>
              )}
            </div>

            <Button
              variant="ghost"
              size="icon"
              className="h-5 w-5 text-muted-foreground/60 hover:text-muted-foreground"
              onClick={clearLog}
              aria-label="Clear activity log"
              title="Clear log"
            >
              <Trash2 className="h-3 w-3" />
            </Button>
          </>
        )}
      </div>

      {/* Log entries */}
      {isOpen && (
        <ScrollArea className="h-[calc(220px-36px)]" ref={scrollRef}>
          {filtered.length === 0 ? (
            <div className="flex items-center justify-center gap-2 py-8 text-xs text-muted-foreground/50">
              <AlertCircle className="h-3.5 w-3.5" />
              {logEntries.length === 0
                ? "No activity yet"
                : "No matching entries"}
            </div>
          ) : (
            <div className="divide-y divide-border/30">
              {filtered.map((entry) => (
                <div
                  key={entry.id}
                  className={`flex items-start gap-2 px-3 py-1.5 text-xs transition-all ${
                    entry.level === "error"
                      ? "bg-red-500/[0.04]"
                      : ""
                  } ${
                    animatingIds.has(entry.id)
                      ? "animate-[slide-in-left_0.3s_ease-out]"
                      : ""
                  }`}
                >
                  {/* Level dot */}
                  <span
                    className={`mt-1.5 h-1.5 w-1.5 shrink-0 rounded-full ${LEVEL_DOT[entry.level]}`}
                  />

                  {/* Timestamp */}
                  <span className="shrink-0 tabular-nums text-[10px] text-muted-foreground/60 mt-0.5">
                    {formatTime(entry.timestamp)}
                  </span>

                  {/* Message */}
                  <span
                    className={`leading-relaxed ${
                      entry.level === "error"
                        ? "text-red-400/90"
                        : entry.level === "warn"
                          ? "text-yellow-400/80"
                          : "text-muted-foreground"
                    }`}
                  >
                    {entry.level === "success" && entry.fileName ? (
                      <>
                        Completed{" "}
                        <span className="text-gold font-medium">
                          {entry.fileName}
                        </span>
                        {entry.message.includes("\u2014")
                          ? ` \u2014${entry.message.split("\u2014")[1]}`
                          : ""}
                      </>
                    ) : (
                      entry.message
                    )}
                  </span>
                </div>
              ))}
            </div>
          )}
        </ScrollArea>
      )}
    </div>
  );
}
