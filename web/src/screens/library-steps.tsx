"use client";

/**
 * The two steps shared by first-run setup and "Add library": choose a folder,
 * then choose a goal. Creating the library maps API error codes back to the
 * step they belong to.
 */

import { useMutation, useQueryClient } from "@tanstack/react-query";
import { FolderOpen } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { FolderPicker } from "@/components/folder-picker";
import { GoalPicker } from "@/components/goal-picker";
import { Button } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/controls";
import { ApiError, api, errorMessage } from "@/lib/api";
import { recommendedGoal } from "@/lib/hardware";
import { defaultsCustomized, profileForNewLibrary } from "@/lib/profile";
import { keys, useHardwareInfo, usePresets, useSettings } from "@/lib/queries";
import type { Goal, Library } from "@/lib/types";
import { titleFromFolder } from "@/lib/utils";

/**
 * Error codes from `POST /libraries` that are about the folder rather than
 * the goal, so the user is sent back to the folder step to pick another.
 */
export const FOLDER_ERRORS = new Set([
  "path_required",
  "path_not_absolute",
  "path_not_found",
  "path_not_supported",
  "not_a_directory",
  "not_readable",
  "library_exists",
  "library_overlaps",
  "contains_output_folder",
  "outside_roots",
  "http_403",
]);

/**
 * A step's heading, focused when the step appears so keyboard and screen
 * reader users land on the new step instead of the top of the page.
 */
export function StepHeading({
  children,
  step,
  className,
  level = 2,
  focusOnMount = true,
}: {
  children: ReactNode;
  /** e.g. "Step 2 of 3", read before the heading. */
  step?: string;
  className?: string;
  level?: 1 | 2;
  focusOnMount?: boolean;
}) {
  const ref = useRef<HTMLHeadingElement>(null);
  useEffect(() => {
    if (focusOnMount) ref.current?.focus({ preventScroll: false });
  }, [focusOnMount]);
  const Tag = level === 1 ? "h1" : "h2";
  return (
    <Tag ref={ref} tabIndex={-1} className={className ?? "font-display text-[2rem] leading-tight text-fg outline-none"}>
      {step ? <span className="sr-only">{step}: </span> : null}
      {children}
    </Tag>
  );
}

export function FolderStep({
  initialPath,
  error,
  onPicked,
  onNavigate,
  step,
  level = 2,
}: {
  initialPath?: string;
  error?: string | null;
  onPicked: (path: string) => void;
  onNavigate?: () => void;
  step?: string;
  /** 1 when the step is the page's own heading (Add library). */
  level?: 1 | 2;
}) {
  return (
    <div>
      <StepHeading step={step} level={level}>
        Where are your videos?
      </StepHeading>
      <p className="mt-2 max-w-xl text-sm leading-relaxed text-muted">
        Choose the folder that holds your movies or shows; folders inside it are included. In Docker this is the
        path inside the container, such as <code className="font-mono text-[0.8125rem] text-fg">/media</code>.
      </p>
      <FolderPicker
        className="mt-6"
        initialPath={initialPath}
        onSelect={onPicked}
        error={error}
        onNavigate={onNavigate}
      />
    </div>
  );
}

interface GoalStepProps {
  path: string;
  onChangeFolder: () => void;
  onCreated: (library: Library) => void;
  onFolderError: (message: string) => void;
  submitLabel: string;
  /** Extra work after the library exists (e.g. marking setup done). */
  afterCreate?: () => Promise<void>;
  secondary?: ReactNode;
  step?: string;
  /** 1 when the step is the page's own heading (Add library). */
  level?: 1 | 2;
}

export function GoalStep({
  path,
  onChangeFolder,
  onCreated,
  onFolderError,
  submitLabel,
  afterCreate,
  secondary,
  step,
  level = 2,
}: GoalStepProps) {
  const client = useQueryClient();
  const presets = usePresets();
  const hardware = useHardwareInfo();
  const settings = useSettings();
  const [chosen, setChosen] = useState<Exclude<Goal, "custom"> | "defaults" | null>(null);
  const [name, setName] = useState(titleFromFolder(path));
  const [error, setError] = useState<string | null>(null);
  const defaults = settings.data?.default_profile;
  // Defaults changed in Settings › Advanced win; untouched ones let the
  // hardware suggest a goal.
  const customized = defaults ? defaultsCustomized(presets.data, defaults) : false;
  const choice: Exclude<Goal, "custom"> | "defaults" =
    chosen ?? (customized ? "defaults" : recommendedGoal(hardware.hw));
  const goal: Goal = choice === "defaults" ? (defaults?.goal ?? "save_space") : choice;

  const create = useMutation({
    mutationFn: async () => {
      const library = await api.createLibrary({
        path,
        name: name.trim() || undefined,
        // Send the whole profile, so the defaults' tracks, resolution and
        // thresholds carry over; `goal` alone would use the stock preset.
        ...(defaults ? { profile: profileForNewLibrary(presets.data, defaults, choice) } : { goal }),
      });
      if (afterCreate) await afterCreate();
      return library;
    },
    onSuccess: (library) => {
      client.setQueryData<Library[]>(keys.libraries, (old) =>
        old ? [...old.filter((l) => l.id !== library.id), library] : [library],
      );
      void client.invalidateQueries({ queryKey: keys.libraries });
      void client.invalidateQueries({ queryKey: keys.overview });
      onCreated(library);
    },
    onError: (err) => {
      if (err instanceof ApiError && FOLDER_ERRORS.has(err.code)) {
        onFolderError(err.message);
      } else {
        setError(errorMessage(err));
      }
    },
  });

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        setError(null);
        create.mutate();
      }}
    >
      <StepHeading step={step} level={level}>
        What should happen to these videos?
      </StepHeading>
      <p className="mt-2 max-w-xl text-sm leading-relaxed text-muted">
        Pick what matters most. You can fine-tune quality, formats and tracks later in the library&apos;s settings.
      </p>

      <div className="mt-5 flex flex-wrap items-center gap-x-3 gap-y-1 rounded-lg border border-line bg-surface px-4 py-3">
        <FolderOpen className="size-[1.125rem] shrink-0 text-accent-ink" aria-hidden />
        <span className="min-w-0 flex-1 truncate font-mono text-[0.8125rem] text-fg">{path}</span>
        <Button variant="link" onClick={onChangeFolder}>
          Change folder
        </Button>
      </div>

      <GoalPicker
        className="mt-6"
        label="Goal"
        value={goal}
        onChange={setChosen}
        presets={presets.data}
        hardware={hardware.hw}
        hardwarePending={hardware.pending}
        preference={settings.data?.hardware}
        defaults={
          defaults && customized
            ? { profile: defaults, selected: choice === "defaults", onSelect: () => setChosen("defaults") }
            : undefined
        }
      />
      {hardware.pending ? (
        <p className="mt-3 text-[0.8125rem] text-muted" role="status">
          Checking your hardware to see how fast each goal converts…
        </p>
      ) : null}

      <Field
        className="mt-7 max-w-sm"
        label="Library name"
        description="Shown in the sidebar. The folder name is used if you leave it empty."
      >
        <Input value={name} onChange={(e) => setName(e.target.value)} maxLength={80} />
      </Field>

      {error ? (
        <p role="alert" className="mt-5 rounded-md bg-danger-soft px-3 py-2.5 text-sm font-medium text-danger">
          {error}
        </p>
      ) : null}

      {/* Primary first in the DOM (and tab order); shown on the right from sm up. */}
      <div className="mt-8 flex flex-col gap-3 sm:flex-row-reverse sm:items-center sm:justify-between">
        <Button type="submit" variant="primary" size="lg" loading={create.isPending}>
          {submitLabel}
        </Button>
        {secondary}
      </div>
      <p className="mt-3 text-[0.8125rem] text-muted sm:text-right">
        {settings.data?.auto_queue === false
          ? "The folder is scanned first. Then choose which files to convert from the library."
          : "The folder is scanned first, then files convert in the background. You can pause any time."}
      </p>
    </form>
  );
}
