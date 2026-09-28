"use client";

import { cva, type VariantProps } from "class-variance-authority";
import { LoaderCircle } from "lucide-react";
import type { ComponentProps } from "react";
import { SERVER_DOWN_TITLE, useServerDown } from "@/lib/store";
import { cn } from "@/lib/utils";

/**
 * Shared button styling, also used for links that look like buttons. On
 * touch screens every size is at least 44px tall, so it can be hit reliably.
 */
export const buttonVariants = cva(
  "inline-flex shrink-0 items-center justify-center gap-2 rounded-md border font-medium whitespace-nowrap select-none transition-[background-color,border-color,color,box-shadow,opacity] duration-150 disabled:pointer-events-none disabled:opacity-50 aria-disabled:pointer-events-none aria-disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:shrink-0",
  {
    variants: {
      variant: {
        primary:
          "border-transparent bg-accent text-on-accent shadow-card hover:bg-accent-hover active:brightness-95",
        secondary:
          "border-line-strong/60 bg-surface text-fg shadow-card hover:border-line-strong hover:bg-raised",
        ghost: "border-transparent bg-transparent text-fg hover:bg-raised",
        quiet: "border-transparent bg-transparent text-muted hover:bg-raised hover:text-fg",
        danger:
          "border-transparent bg-danger-soft text-danger hover:bg-danger hover:text-surface",
        link: "h-auto border-transparent bg-transparent p-0 text-accent-ink underline-offset-4 hover:underline",
      },
      size: {
        sm: "h-8 px-3 text-[0.8125rem] pointer-coarse:h-11 [&_svg:not([class*='size-'])]:size-4",
        md: "h-9 px-3.5 text-sm pointer-coarse:h-11 [&_svg:not([class*='size-'])]:size-4",
        lg: "h-11 px-5 text-[0.9375rem] [&_svg:not([class*='size-'])]:size-[1.125rem]",
        icon: "size-9 p-0 pointer-coarse:size-11 [&_svg:not([class*='size-'])]:size-[1.125rem]",
        "icon-sm": "size-8 p-0 pointer-coarse:size-11 [&_svg:not([class*='size-'])]:size-4",
      },
    },
    compoundVariants: [{ variant: "link", className: "h-auto px-0 pointer-coarse:h-auto" }],
    defaultVariants: { variant: "secondary", size: "md" },
  },
);

export type ButtonProps = ComponentProps<"button"> &
  VariantProps<typeof buttonVariants> & {
    /** Show a spinner and block clicks while an action runs. */
    loading?: boolean;
    /**
     * The action changes something on the server, so it is disabled (and
     * says why) while the server can't be reached.
     */
    needsServer?: boolean;
  };

/** A button. Defaults to `type="button"` so it never submits a form by accident. */
export function Button({
  className,
  variant,
  size,
  loading = false,
  needsServer = false,
  disabled,
  children,
  title,
  type = "button",
  ...props
}: ButtonProps) {
  const serverDown = useServerDown();
  const offline = needsServer && serverDown;
  return (
    <button
      type={type}
      className={cn(buttonVariants({ variant, size }), className)}
      disabled={disabled || loading || offline}
      aria-busy={loading || undefined}
      title={offline ? SERVER_DOWN_TITLE : title}
      {...props}
    >
      {loading ? <LoaderCircle className="spin" aria-hidden /> : null}
      {children}
    </button>
  );
}
