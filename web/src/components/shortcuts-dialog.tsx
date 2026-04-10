"use client";

import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";

const SHORTCUTS = [
  { keys: ["Space"], description: "Start / Pause processing" },
  { keys: ["R"], description: "Scan library" },
  { keys: ["?"], description: "Show this dialog" },
  { keys: ["Esc"], description: "Close dialog" },
] as const;

function Kbd({ children }: { children: React.ReactNode }) {
  return (
    <kbd className="inline-flex h-6 min-w-6 items-center justify-center rounded-[5px] border border-border/60 bg-secondary/80 px-1.5 font-mono text-[11px] font-medium text-foreground/80 shadow-[0_1px_0_1px_oklch(0_0_0_/_0.15)]">
      {children}
    </kbd>
  );
}

export function ShortcutsDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-xs">
        <DialogHeader>
          <DialogTitle>Keyboard Shortcuts</DialogTitle>
          <DialogDescription>
            Navigate Chrysopeia faster with these shortcuts.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-2.5">
          {SHORTCUTS.map((shortcut) => (
            <div
              key={shortcut.description}
              className="flex items-center justify-between gap-4"
            >
              <span className="text-sm text-muted-foreground">
                {shortcut.description}
              </span>
              <div className="flex items-center gap-1">
                {shortcut.keys.map((key) => (
                  <Kbd key={key}>{key}</Kbd>
                ))}
              </div>
            </div>
          ))}
        </div>
      </DialogContent>
    </Dialog>
  );
}
