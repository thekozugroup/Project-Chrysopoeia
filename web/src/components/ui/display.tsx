"use client";

/** Small display building blocks shared by every screen. */

import { Check, ChevronRight, Copy, CircleAlert, Info, TriangleAlert, CircleCheck } from "lucide-react";
import { useEffect, useRef, useState, type ComponentProps, type ReactNode } from "react";
import { toast } from "sonner";
import { cn, copyText } from "@/lib/utils";
import { Button } from "./button";

/** Placeholder block while content loads. */
export function Skeleton({ className, ...props }: ComponentProps<"div">) {
  return <div aria-hidden className={cn("skeleton h-4", className)} {...props} />;
}

interface MeterProps {
  /** 0..100 */
  value: number;
  label: string;
  className?: string;
  /** Adds the travelling highlight that says "working". */
  live?: boolean;
  tone?: "accent" | "success" | "danger" | "muted";
  size?: "xs" | "sm" | "md";
}

/** A progress bar with a proper progressbar role. */
export function Meter({ value, label, className, live, tone = "accent", size = "sm" }: MeterProps) {
  const clamped = Math.max(0, Math.min(100, Number.isFinite(value) ? value : 0));
  return (
    <div
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(clamped)}
      className={cn(
        "w-full overflow-hidden rounded-full bg-raised",
        size === "xs" ? "h-1" : size === "sm" ? "h-1.5" : "h-2.5",
        className,
      )}
    >
      <div
        className={cn(
          "progress-fill h-full w-full rounded-full",
          tone === "accent" && "bg-meter",
          tone === "success" && "bg-success",
          tone === "danger" && "bg-danger",
          tone === "muted" && "bg-line-strong",
          live && clamped > 0 && clamped < 100 && "progress-live",
        )}
        style={{ clipPath: `inset(0 ${100 - clamped}% 0 0 round 999px)` }}
      />
    </div>
  );
}

export type Tone = "neutral" | "accent" | "success" | "warning" | "danger" | "info";

const toneClasses: Record<Tone, string> = {
  neutral: "bg-raised text-muted",
  accent: "bg-accent-soft text-accent-ink",
  success: "bg-success-soft text-success",
  warning: "bg-warning-soft text-warning",
  danger: "bg-danger-soft text-danger",
  info: "bg-info-soft text-info",
};

/** A compact label with an icon. Colour always travels with a word. */
export function Badge({
  tone = "neutral",
  icon,
  children,
  className,
}: {
  tone?: Tone;
  icon?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <span
      className={cn(
        "inline-flex h-6 shrink-0 items-center gap-1.5 rounded-full px-2.5 text-xs font-medium whitespace-nowrap [&_svg]:size-3.5 [&_svg]:shrink-0",
        toneClasses[tone],
        className,
      )}
    >
      {icon}
      {children}
    </span>
  );
}

const calloutIcons: Record<"info" | "warning" | "danger" | "success", ReactNode> = {
  info: <Info aria-hidden />,
  warning: <TriangleAlert aria-hidden />,
  danger: <CircleAlert aria-hidden />,
  success: <CircleCheck aria-hidden />,
};

/** A boxed message: setup hints, failures, notes. */
export function Callout({
  tone = "info",
  title,
  children,
  action,
  className,
}: {
  tone?: "info" | "warning" | "danger" | "success";
  title: ReactNode;
  children?: ReactNode;
  action?: ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "flex gap-3 rounded-lg border p-4",
        tone === "info" && "border-info/25 bg-info-soft",
        tone === "warning" && "border-warning/30 bg-warning-soft",
        tone === "danger" && "border-danger/30 bg-danger-soft",
        tone === "success" && "border-success/25 bg-success-soft",
        className,
      )}
    >
      <span
        className={cn(
          "mt-px [&_svg]:size-[1.125rem]",
          tone === "info" && "text-info",
          tone === "warning" && "text-warning",
          tone === "danger" && "text-danger",
          tone === "success" && "text-success",
        )}
      >
        {calloutIcons[tone]}
      </span>
      <div className="min-w-0 flex-1">
        <p className="text-sm font-semibold text-fg">{title}</p>
        {children ? <div className="mt-1 text-[0.8125rem] leading-relaxed text-fg/85">{children}</div> : null}
        {action ? <div className="mt-3">{action}</div> : null}
      </div>
    </div>
  );
}

/** Copy a string, with a toast and a check mark as feedback. */
export function CopyButton({
  text,
  label = "Copy",
  className,
  size = "sm",
}: {
  text: string;
  label?: string;
  className?: string;
  size?: "sm" | "icon-sm";
}) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => {
    if (timer.current) clearTimeout(timer.current);
  }, []);
  const onCopy = async () => {
    const ok = await copyText(text);
    if (ok) {
      setCopied(true);
      if (timer.current) clearTimeout(timer.current);
      timer.current = setTimeout(() => setCopied(false), 1800);
    } else {
      toast.error("Couldn't copy. Select the text and copy it by hand.");
    }
  };
  return (
    <Button
      variant="secondary"
      size={size}
      onClick={onCopy}
      className={className}
      aria-label={size === "icon-sm" ? label : undefined}
    >
      {copied ? <Check aria-hidden /> : <Copy aria-hidden />}
      {size === "icon-sm" ? null : copied ? "Copied" : label}
      <span className="sr-only" aria-live="polite">
        {copied ? "Copied to clipboard" : ""}
      </span>
    </Button>
  );
}

/** Monospace block for commands and logs, with a copy button. */
export function CodeBlock({
  code,
  label,
  className,
  maxHeight = "16rem",
  wrap = true,
}: {
  code: string;
  label: string;
  className?: string;
  maxHeight?: string;
  wrap?: boolean;
}) {
  return (
    <div className={cn("overflow-hidden rounded-lg border border-line bg-sunken", className)}>
      <div className="flex items-center justify-between gap-2 border-b border-line py-1.5 pr-1.5 pl-3">
        <span className="text-xs font-medium text-muted">{label}</span>
        <CopyButton text={code} label="Copy" />
      </div>
      <pre
        tabIndex={0}
        aria-label={label}
        className={cn(
          "overflow-auto px-3 py-2.5 font-mono text-xs leading-relaxed text-fg",
          wrap ? "break-all whitespace-pre-wrap" : "whitespace-pre",
        )}
        style={{ maxHeight }}
      >
        {code}
      </pre>
    </div>
  );
}

/** Teaches what belongs in an empty area and the next step. */
export function EmptyState({
  icon,
  title,
  children,
  action,
  className,
}: {
  icon?: ReactNode;
  title: ReactNode;
  children?: ReactNode;
  action?: ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "flex flex-col items-center rounded-lg border border-dashed border-line-strong/50 px-6 py-10 text-center",
        className,
      )}
    >
      {icon ? (
        <div className="mb-3 grid size-11 place-items-center rounded-full bg-raised text-muted [&_svg]:size-5">
          {icon}
        </div>
      ) : null}
      <p className="text-[0.9375rem] font-semibold text-fg">{title}</p>
      {children ? <div className="mt-1.5 max-w-md text-sm leading-relaxed text-muted">{children}</div> : null}
      {action ? <div className="mt-4 flex flex-wrap justify-center gap-2">{action}</div> : null}
    </div>
  );
}

/** Heading for a block within a page. */
export function SectionHeading({
  title,
  description,
  action,
  id,
  className,
  quietAction = true,
}: {
  title: ReactNode;
  description?: ReactNode;
  action?: ReactNode;
  id?: string;
  className?: string;
  /**
   * The action starts with a borderless button, whose text is pulled left
   * to line up with the heading on phones. Pass `false` for a bordered one,
   * whose edge must line up instead.
   */
  quietAction?: boolean;
}) {
  // The action sits beside the heading at every width ("Libraries · Add
  // library"); only a row of several controls that can't fit wraps below.
  return (
    <div
      className={cn(
        "mb-3 flex flex-wrap justify-between gap-x-4 gap-y-2",
        description ? "items-end" : "items-center",
        className,
      )}
    >
      <div className="min-w-0">
        <h2 id={id} className="text-[1.0625rem] leading-snug font-semibold text-fg">
          {title}
        </h2>
        {description ? <p className="mt-0.5 text-[0.8125rem] text-muted">{description}</p> : null}
      </div>
      {action ? (
        // Pulled left so a wrapped borderless button's text lines up with
        // the heading; beside it, the margin only widens the gap.
        <div className={cn("flex max-w-full shrink-0 flex-wrap items-center gap-2", quietAction && "-ml-2 sm:ml-0")}>
          {action}
        </div>
      ) : null}
    </div>
  );
}

/**
 * The one place technical data lives: codecs, encoder names, similarity
 * scores, ffmpeg commands and logs, behind a closed "Technical details".
 */
export function Disclosure({
  title = "Technical details",
  children,
  className,
}: {
  title?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <details className={cn("group rounded-lg border border-line", className)}>
      <summary className="flex cursor-pointer list-none items-center gap-2 rounded-lg px-4 py-3 text-sm font-medium text-fg select-none hover:text-accent-ink pointer-coarse:min-h-11 [&::-webkit-details-marker]:hidden">
        <ChevronRight
          className="size-4 shrink-0 text-muted transition-transform duration-200 group-open:rotate-90"
          aria-hidden
        />
        {title}
      </summary>
      <div className="flex flex-col gap-4 border-t border-line px-4 py-4">{children}</div>
    </details>
  );
}

/** A label/value pair for detail lists. */
export function Detail({ label, children, mono }: { label: ReactNode; children: ReactNode; mono?: boolean }) {
  return (
    <div className="flex items-baseline justify-between gap-4 py-2 text-sm">
      <dt className="shrink-0 text-muted">{label}</dt>
      <dd className={cn("min-w-0 text-right break-words text-fg", mono && "font-mono text-[0.8125rem]")}>
        {children}
      </dd>
    </div>
  );
}
