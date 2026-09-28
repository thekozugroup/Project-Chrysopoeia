import { cva, type VariantProps } from "class-variance-authority";
import { LoaderCircle } from "lucide-react";
import type { ComponentProps } from "react";
import { cn } from "@/lib/utils";

/** Shared button styling, also used for links that look like buttons. */
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
        sm: "h-8 px-3 text-[0.8125rem] [&_svg:not([class*='size-'])]:size-4",
        md: "h-9 px-3.5 text-sm [&_svg:not([class*='size-'])]:size-4",
        lg: "h-11 px-5 text-[0.9375rem] [&_svg:not([class*='size-'])]:size-[1.125rem]",
        icon: "size-9 p-0 [&_svg:not([class*='size-'])]:size-[1.125rem]",
        "icon-sm": "size-8 p-0 [&_svg:not([class*='size-'])]:size-4",
      },
    },
    compoundVariants: [{ variant: "link", className: "h-auto px-0" }],
    defaultVariants: { variant: "secondary", size: "md" },
  },
);

export type ButtonProps = ComponentProps<"button"> &
  VariantProps<typeof buttonVariants> & {
    /** Show a spinner and block clicks while an action runs. */
    loading?: boolean;
  };

/** A button. Defaults to `type="button"` so it never submits a form by accident. */
export function Button({
  className,
  variant,
  size,
  loading = false,
  disabled,
  children,
  type = "button",
  ...props
}: ButtonProps) {
  return (
    <button
      type={type}
      className={cn(buttonVariants({ variant, size }), className)}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      {...props}
    >
      {loading ? <LoaderCircle className="spin" aria-hidden /> : null}
      {children}
    </button>
  );
}
