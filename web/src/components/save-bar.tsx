"use client";

/**
 * Floating "unsaved changes" bar shared by every settings form. While there
 * are unsaved changes it also guards navigation: closing the tab warns, and
 * leaving the screen inside the app asks to save, discard or keep editing.
 */

import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { UnsavedChangesDialog } from "@/components/ui/overlays";
import { useNavigationGuard, usePendingNavigation, type Route } from "@/lib/router";
import { cn } from "@/lib/utils";

interface SaveBarProps {
  dirty: boolean;
  saving: boolean;
  onSave: () => void;
  onDiscard: () => void;
  /** Plain-language reason saving is blocked or failed. */
  error?: string | null;
  /**
   * `alert` (default) announces the error at once. Use `status` when it only
   * points at an error already announced next to its field, so screen
   * readers don't read the same problem twice.
   */
  errorRole?: "alert" | "status";
  disabled?: boolean;
  /** Whether going to `target` would unmount this form (and lose the edits). */
  blocks: (target: Route) => boolean;
  /** Save from the "leave this page?" dialog. Resolves `true` when saved. */
  saveAndLeave: () => Promise<boolean>;
}

export function SaveBar({
  dirty,
  saving,
  onSave,
  onDiscard,
  error,
  errorRole = "alert",
  disabled,
  blocks,
  saveAndLeave,
}: SaveBarProps) {
  // Warn before closing the tab or reloading with unsaved changes.
  useEffect(() => {
    if (!dirty) return;
    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      e.preventDefault();
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    return () => window.removeEventListener("beforeunload", onBeforeUnload);
  }, [dirty]);

  // Hash navigation never fires beforeunload: ask inside the app instead.
  useNavigationGuard(dirty, { blocks, save: saveAndLeave, discard: onDiscard });

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
        <p
          className={cn("min-w-0 flex-1 text-sm", error ? "font-medium text-danger" : "text-fg")}
          role={error ? errorRole : "status"}
        >
          {error ?? "You have unsaved changes."}
        </p>
        <div className="flex shrink-0 gap-2">
          {/* Discard also clears invalid text that never became a change. */}
          <Button variant="quiet" onClick={onDiscard} disabled={saving}>
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

/**
 * The "Save your changes?" dialog for navigation held by a save bar. Render
 * once, inside the app frame.
 */
export function NavigationPrompt() {
  const { pending, save, discard, stay } = usePendingNavigation();
  const [saving, setSaving] = useState(false);
  return (
    <UnsavedChangesDialog
      open={pending !== null}
      saving={saving}
      onStay={stay}
      onDiscard={discard}
      onSave={() => {
        setSaving(true);
        void save().finally(() => setSaving(false));
      }}
    />
  );
}
