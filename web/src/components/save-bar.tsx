"use client";

/** Floating "unsaved changes" bar shared by every settings form. */

import { useEffect } from "react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

interface SaveBarProps {
  dirty: boolean;
  saving: boolean;
  onSave: () => void;
  onDiscard: () => void;
  /** Plain-language reason saving is blocked or failed. */
  error?: string | null;
  disabled?: boolean;
}

export function SaveBar({ dirty, saving, onSave, onDiscard, error, disabled }: SaveBarProps) {
  // Warn before leaving the page with unsaved changes.
  useEffect(() => {
    if (!dirty) return;
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      e.preventDefault();
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    return () => window.removeEventListener("beforeunload", onBeforeUnload);
  }, [dirty]);

  if (!dirty && !error) return null;
  return (
    <div className="pointer-events-none sticky bottom-20 z-20 mt-8 flex justify-center md:bottom-5">
      <div
        role="region"
        aria-label="Unsaved changes"
        className={cn(
          "pointer-events-auto flex w-full max-w-2xl flex-col gap-3 rounded-xl border bg-surface px-4 py-3 shadow-pop sm:flex-row sm:items-center",
          error ? "border-danger/40" : "border-line",
        )}
      >
        <p className={cn("min-w-0 flex-1 text-sm", error ? "font-medium text-danger" : "text-fg")} role={error ? "alert" : "status"}>
          {error ?? "You have unsaved changes."}
        </p>
        <div className="flex shrink-0 gap-2">
          <Button variant="quiet" onClick={onDiscard} disabled={saving || !dirty}>
            Discard
          </Button>
          <Button variant="primary" onClick={onSave} loading={saving} disabled={disabled || !dirty}>
            Save changes
          </Button>
        </div>
      </div>
    </div>
  );
}
