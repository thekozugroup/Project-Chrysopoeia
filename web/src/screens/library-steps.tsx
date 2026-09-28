"use client";

/**
 * The two steps shared by first-run setup and "Add library": choose a folder,
 * then choose a goal. Creating the library maps API error codes back to the
 * step they belong to.
 */

import { useMutation, useQueryClient } from "@tanstack/react-query";
import { FolderOpen } from "lucide-react";
import { useState } from "react";
import { FolderPicker } from "@/components/folder-picker";
import { GoalPicker } from "@/components/goal-picker";
import { Button } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/controls";
import { ApiError, api, errorMessage } from "@/lib/api";
import { recommendedGoal } from "@/lib/hardware";
import { keys, useHardware, usePresets, useSettings } from "@/lib/queries";
import type { Goal, Library } from "@/lib/types";
import { titleFromFolder } from "@/lib/utils";

/** Error codes that are about the folder rather than the goal. */
const FOLDER_ERRORS = new Set(["path_not_found", "not_a_directory", "not_readable", "library_exists", "library_overlaps", "forbidden", "http_403"]);

export function FolderStep({
  initialPath,
  error,
  onPicked,
  onNavigate,
}: {
  initialPath?: string;
  error?: string | null;
  onPicked: (path: string) => void;
  onNavigate?: () => void;
}) {
  return (
    <div>
      <h2 className="font-display text-[2rem] leading-tight text-fg">Where are your videos?</h2>
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
  secondary?: React.ReactNode;
}

export function GoalStep({
  path,
  onChangeFolder,
  onCreated,
  onFolderError,
  submitLabel,
  afterCreate,
  secondary,
}: GoalStepProps) {
  const client = useQueryClient();
  const presets = usePresets();
  const hardware = useHardware();
  const settings = useSettings();
  const [chosen, setChosen] = useState<Goal | null>(null);
  const [name, setName] = useState(titleFromFolder(path));
  const [error, setError] = useState<string | null>(null);
  const goal: Goal = chosen ?? recommendedGoal(hardware.data);

  const create = useMutation({
    mutationFn: async () => {
      const library = await api.createLibrary({ path, name: name.trim() || undefined, goal });
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
      <h2 className="font-display text-[2rem] leading-tight text-fg">What should happen to these videos?</h2>
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
        hardware={hardware.data}
        hardwarePending={hardware.isPending || Boolean(hardware.error)}
        preference={settings.data?.hardware}
      />
      {hardware.isPending || hardware.error ? (
        <p className="mt-3 text-[0.8125rem] text-muted" role="status">
          Checking your hardware to estimate speed…
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

      <div className="mt-8 flex flex-col-reverse gap-3 sm:flex-row sm:items-center">
        {secondary}
        <div className="flex flex-col gap-2 sm:ml-auto sm:items-end">
          <Button type="submit" variant="primary" size="lg" loading={create.isPending}>
            {submitLabel}
          </Button>
        </div>
      </div>
      <p className="mt-3 text-[0.8125rem] text-muted sm:text-right">
        {settings.data?.auto_queue === false
          ? "The folder is scanned first. Then choose which files to convert from the library."
          : "The folder is scanned first, then files convert in the background. You can pause any time."}
      </p>
    </form>
  );
}
