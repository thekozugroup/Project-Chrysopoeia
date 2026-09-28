"use client";

/**
 * Overlays built on Base UI (focus trapping, Escape, portals, scroll lock):
 * a detail sheet that slides from the right on desktop and up from the
 * bottom on phones, a confirmation dialog, an action menu and tooltips.
 */

import { AlertDialog } from "@base-ui/react/alert-dialog";
import { Dialog } from "@base-ui/react/dialog";
import { Menu } from "@base-ui/react/menu";
import { Tooltip as BaseTooltip } from "@base-ui/react/tooltip";
import { X } from "lucide-react";
import type { ReactElement, ReactNode } from "react";
import { cn } from "@/lib/utils";
import { Button, buttonVariants } from "./button";

interface SheetProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: ReactNode;
  description?: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  /** Extra content in the header row, right of the title. */
  headerExtra?: ReactNode;
}

/** Detail panel for a job or file. */
export function Sheet({ open, onOpenChange, title, description, children, footer, headerExtra }: SheetProps) {
  return (
    <Dialog.Root open={open} onOpenChange={(next) => onOpenChange(next)}>
      <Dialog.Portal>
        <Dialog.Backdrop className="fixed inset-0 z-40 bg-overlay transition-opacity duration-200 data-[ending-style]:opacity-0 data-[starting-style]:opacity-0" />
        <Dialog.Popup
          className={cn(
            "fixed z-50 flex flex-col bg-surface text-fg shadow-pop outline-none",
            "inset-x-0 bottom-0 max-h-[92dvh] rounded-t-xl border-t border-line",
            "md:inset-y-2 md:right-2 md:left-auto md:max-h-none md:w-[min(34rem,calc(100vw-1rem))] md:rounded-xl md:border",
            "transition-[transform,opacity] duration-250 ease-[var(--ease-out-expo)]",
            "data-[starting-style]:translate-y-8 data-[starting-style]:opacity-0 data-[ending-style]:translate-y-8 data-[ending-style]:opacity-0",
            "md:data-[starting-style]:translate-x-8 md:data-[starting-style]:translate-y-0 md:data-[ending-style]:translate-x-8 md:data-[ending-style]:translate-y-0",
          )}
        >
          <div className="flex items-start gap-3 border-b border-line px-5 pt-4 pb-3.5">
            <div className="min-w-0 flex-1">
              <Dialog.Title className="text-base leading-snug font-semibold break-words text-fg">{title}</Dialog.Title>
              {description ? (
                <Dialog.Description className="mt-1 text-[0.8125rem] text-muted">{description}</Dialog.Description>
              ) : null}
            </div>
            {headerExtra}
            <Dialog.Close
              className={cn(buttonVariants({ variant: "quiet", size: "icon-sm" }), "-mr-1.5")}
              aria-label="Close"
            >
              <X />
            </Dialog.Close>
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-5 py-5">{children}</div>
          {footer ? (
            <div className="flex flex-wrap items-center justify-end gap-2 border-t border-line px-5 py-3 pb-[max(0.75rem,env(safe-area-inset-bottom))]">
              {footer}
            </div>
          ) : null}
        </Dialog.Popup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

interface ModalProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: ReactNode;
  description?: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  className?: string;
}

/** A centred dialog for focused tasks such as picking a folder. */
export function Modal({ open, onOpenChange, title, description, children, footer, className }: ModalProps) {
  return (
    <Dialog.Root open={open} onOpenChange={(next) => onOpenChange(next)}>
      <Dialog.Portal>
        <Dialog.Backdrop className="fixed inset-0 z-40 bg-overlay transition-opacity duration-200 data-[ending-style]:opacity-0 data-[starting-style]:opacity-0" />
        <Dialog.Popup
          className={cn(
            "fixed top-1/2 left-1/2 z-50 flex max-h-[min(44rem,calc(100dvh-2rem))] w-[min(40rem,calc(100vw-1.5rem))] -translate-x-1/2 -translate-y-1/2 flex-col rounded-xl border border-line bg-surface text-fg shadow-pop outline-none",
            "transition-[transform,opacity] duration-200 ease-[var(--ease-out-expo)] data-[ending-style]:scale-[0.98] data-[ending-style]:opacity-0 data-[starting-style]:scale-[0.98] data-[starting-style]:opacity-0",
            className,
          )}
        >
          <div className="flex items-start gap-3 px-5 pt-4 pb-3">
            <div className="min-w-0 flex-1">
              <Dialog.Title className="text-base font-semibold text-fg">{title}</Dialog.Title>
              {description ? (
                <Dialog.Description className="mt-1 text-[0.8125rem] text-muted">{description}</Dialog.Description>
              ) : null}
            </div>
            <Dialog.Close
              className={cn(buttonVariants({ variant: "quiet", size: "icon-sm" }), "-mr-1.5")}
              aria-label="Close"
            >
              <X />
            </Dialog.Close>
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto px-5 pb-4">{children}</div>
          {footer ? (
            <div className="flex flex-wrap items-center justify-end gap-2 border-t border-line px-5 py-3">{footer}</div>
          ) : null}
        </Dialog.Popup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

interface ConfirmProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: ReactNode;
  children: ReactNode;
  confirmLabel: string;
  onConfirm: () => void;
  /** Destructive actions get the red confirm button. */
  destructive?: boolean;
  loading?: boolean;
}

/** "Are you sure?" for actions that can't be undone. */
export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  children,
  confirmLabel,
  onConfirm,
  destructive,
  loading,
}: ConfirmProps) {
  return (
    <AlertDialog.Root open={open} onOpenChange={(next) => onOpenChange(next)}>
      <AlertDialog.Portal>
        <AlertDialog.Backdrop className="fixed inset-0 z-40 bg-overlay transition-opacity duration-200 data-[ending-style]:opacity-0 data-[starting-style]:opacity-0" />
        <AlertDialog.Popup className="fixed top-1/2 left-1/2 z-50 w-[min(28rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 rounded-xl border border-line bg-surface p-5 text-fg shadow-pop outline-none transition-[transform,opacity] duration-200 ease-[var(--ease-out-expo)] data-[ending-style]:scale-[0.98] data-[ending-style]:opacity-0 data-[starting-style]:scale-[0.98] data-[starting-style]:opacity-0">
          <AlertDialog.Title className="text-base font-semibold text-fg">{title}</AlertDialog.Title>
          <AlertDialog.Description render={<div />} className="mt-2 space-y-2 text-sm leading-relaxed text-muted">
            {children}
          </AlertDialog.Description>
          <div className="mt-5 flex flex-col-reverse gap-2 sm:flex-row sm:justify-end">
            <AlertDialog.Close className={buttonVariants({ variant: "secondary" })}>Cancel</AlertDialog.Close>
            <Button variant={destructive ? "danger" : "primary"} onClick={onConfirm} loading={loading}>
              {confirmLabel}
            </Button>
          </div>
        </AlertDialog.Popup>
      </AlertDialog.Portal>
    </AlertDialog.Root>
  );
}

export interface MenuAction {
  label: string;
  icon?: ReactNode;
  onSelect: () => void;
  destructive?: boolean;
  disabled?: boolean;
}

interface ActionMenuProps {
  /** The trigger button element; receives menu props via `render`. */
  trigger: ReactElement;
  actions: (MenuAction | "separator")[];
  align?: "start" | "end";
}

/** A small menu of secondary actions ("⋯" buttons). */
export function ActionMenu({ trigger, actions, align = "end" }: ActionMenuProps) {
  return (
    <Menu.Root>
      <Menu.Trigger render={trigger} />
      <Menu.Portal>
        <Menu.Positioner side="bottom" align={align} sideOffset={6} className="z-50 outline-none">
          <Menu.Popup className="min-w-52 origin-[var(--transform-origin)] rounded-lg border border-line bg-surface p-1 text-sm text-fg shadow-pop outline-none transition-[transform,opacity] duration-150 data-[ending-style]:scale-95 data-[ending-style]:opacity-0 data-[starting-style]:scale-95 data-[starting-style]:opacity-0">
            {actions.map((action, i) =>
              action === "separator" ? (
                <Menu.Separator key={`sep-${i}`} className="mx-1 my-1 h-px bg-line" />
              ) : (
                <Menu.Item
                  key={action.label}
                  disabled={action.disabled}
                  onClick={action.onSelect}
                  className={cn(
                    "flex h-9 cursor-default items-center gap-2.5 rounded-md px-2.5 outline-none select-none data-[disabled]:opacity-50 data-[highlighted]:bg-raised [&_svg]:size-4 [&_svg]:shrink-0",
                    action.destructive ? "text-danger" : "text-fg [&_svg]:text-muted",
                  )}
                >
                  {action.icon}
                  {action.label}
                </Menu.Item>
              ),
            )}
          </Menu.Popup>
        </Menu.Positioner>
      </Menu.Portal>
    </Menu.Root>
  );
}

/** Tooltip provider; wrap the app once. */
export const TooltipProvider = BaseTooltip.Provider;

/** A short hint on hover or focus. Never the only place information lives. */
export function Tooltip({ content, children }: { content: ReactNode; children: ReactElement }) {
  return (
    <BaseTooltip.Root>
      <BaseTooltip.Trigger render={children} />
      <BaseTooltip.Portal>
        <BaseTooltip.Positioner sideOffset={6} className="z-50">
          <BaseTooltip.Popup className="max-w-72 rounded-md border border-line bg-surface px-2.5 py-1.5 text-[0.8125rem] leading-snug text-fg shadow-pop transition-opacity duration-150 data-[ending-style]:opacity-0 data-[starting-style]:opacity-0">
            {content}
          </BaseTooltip.Popup>
        </BaseTooltip.Positioner>
      </BaseTooltip.Portal>
    </BaseTooltip.Root>
  );
}
