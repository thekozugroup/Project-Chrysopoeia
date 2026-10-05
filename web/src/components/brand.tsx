import { cn } from "@/lib/utils";

/** The alchemical sign for gold (a circle with a point at its centre). */
export function BrandMark({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 32 32" aria-hidden className={cn("size-7 shrink-0", className)} fill="none">
      <circle cx="16" cy="16" r="12.25" stroke="currentColor" strokeWidth="2.5" />
      <circle cx="16" cy="16" r="3.25" fill="currentColor" />
    </svg>
  );
}

/** Mark plus wordmark. */
export function Brand({ className }: { className?: string }) {
  return (
    <span className={cn("inline-flex items-center gap-2.5 text-fg", className)}>
      <BrandMark className="text-accent-ink" />
      <span className="font-display text-[1.375rem] leading-none">Szalinski</span>
    </span>
  );
}
