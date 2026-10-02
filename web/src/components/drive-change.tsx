"use client";

/**
 * "Use the drive that's there now": what the user is told before
 * Chrysopoeia takes another drive, mounted where a folder's drive or share
 * was, as the usual one. Shared by the library page and Settings.
 */

import { HardDrive } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/ui/overlays";

export const USE_DRIVE_LABEL = "Use the drive that's there now";

/** The button that opens the confirmation. */
export function UseDriveButton({ onClick }: { onClick: () => void }) {
  return (
    <Button size="sm" variant="secondary" onClick={onClick} needsServer>
      <HardDrive aria-hidden />
      {USE_DRIVE_LABEL}
    </Button>
  );
}

/** The confirmation, which says what taking the drive at `mount` means. */
export function UseDriveConfirm({
  open,
  onOpenChange,
  mount,
  loading,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  mount: string;
  loading: boolean;
  onConfirm: () => void;
}) {
  return (
    <ConfirmDialog
      open={open}
      onOpenChange={onOpenChange}
      title="Use the drive that's there now?"
      confirmLabel="Use this drive"
      loading={loading}
      onConfirm={onConfirm}
    >
      <p>
        Chrysopoeia will take the drive mounted at <span className="font-mono text-[0.8125rem] text-fg">{mount}</span>{" "}
        as the usual one from now on: it reads files from it and saves new files to it.
      </p>
      <p className="font-medium text-fg">
        Only do this if you replaced the drive or share on purpose. If the usual one just isn&apos;t connected yet,
        reconnect it instead: files saved now would end up on the drive that&apos;s there now.
      </p>
    </ConfirmDialog>
  );
}
