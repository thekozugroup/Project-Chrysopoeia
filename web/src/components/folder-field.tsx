"use client";

/** A folder setting: shows the chosen path and opens the folder picker. */

import { Folder } from "lucide-react";
import { useState } from "react";
import { FolderPicker } from "@/components/folder-picker";
import { Button } from "@/components/ui/button";
import { Modal } from "@/components/ui/overlays";
import { cn } from "@/lib/utils";

interface FolderFieldProps {
  value: string | null;
  onChange: (path: string) => void;
  placeholder: string;
  dialogTitle: string;
  dialogDescription?: string;
  error?: string | null;
  labelId?: string;
}

export function FolderField({
  value,
  onChange,
  placeholder,
  dialogTitle,
  dialogDescription,
  error,
  labelId,
}: FolderFieldProps) {
  const [open, setOpen] = useState(false);
  return (
    <div>
      <div
        className={cn(
          "flex items-center gap-3 rounded-md border bg-surface py-1.5 pr-1.5 pl-3",
          error ? "border-danger ring-2 ring-danger/25" : "border-line-strong",
        )}
      >
        <Folder className="size-4 shrink-0 text-accent-ink" aria-hidden />
        <span
          className={cn("min-w-0 flex-1 truncate font-mono text-[0.8125rem]", value ? "text-fg" : "text-muted")}
          aria-labelledby={labelId}
        >
          {value || placeholder}
        </span>
        <Button size="sm" onClick={() => setOpen(true)} aria-describedby={labelId}>
          {value ? "Change…" : "Choose…"}
        </Button>
      </div>
      {error ? (
        <p role="alert" className="mt-1.5 text-[0.8125rem] font-medium text-danger">
          {error}
        </p>
      ) : null}
      <Modal open={open} onOpenChange={setOpen} title={dialogTitle} description={dialogDescription}>
        {open ? (
          <FolderPicker
            initialPath={value ?? undefined}
            onSelect={(path) => {
              onChange(path);
              setOpen(false);
            }}
          />
        ) : null}
      </Modal>
    </div>
  );
}
