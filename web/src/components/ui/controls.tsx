"use client";

/**
 * Form controls. Native elements wherever the platform already does the job
 * (text inputs, selects, radios), so keyboard, screen reader and phone
 * behaviour come for free; only the look is ours.
 */

import { ChevronDown } from "lucide-react";
import { createContext, useContext, useId, type ComponentProps, type ReactNode } from "react";
import { cn } from "@/lib/utils";

const fieldBase =
  "w-full rounded-md border border-line-strong/70 bg-surface text-fg shadow-card transition-[border-color,box-shadow] duration-150 hover:border-line-strong focus-visible:border-accent-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent-ink/40 disabled:cursor-not-allowed disabled:opacity-60 aria-invalid:border-danger aria-invalid:ring-2 aria-invalid:ring-danger/25";

/** Single-line text input. */
export function Input({ className, ...props }: ComponentProps<"input">) {
  const field = useField();
  return <input {...field} className={cn(fieldBase, "h-9 px-3 text-sm", className)} {...props} />;
}

/** Multi-line text input. */
export function Textarea({ className, ...props }: ComponentProps<"textarea">) {
  const field = useField();
  return (
    <textarea
      {...field}
      className={cn(fieldBase, "min-h-24 px-3 py-2 text-sm leading-relaxed", className)}
      {...props}
    />
  );
}

/** Native select with our styling. */
export function Select({ className, children, ...props }: ComponentProps<"select">) {
  const field = useField();
  return (
    <div className={cn("relative", className)}>
      <select
        {...field}
        className={cn(fieldBase, "h-9 cursor-pointer appearance-none pr-9 pl-3 text-sm")}
        {...props}
      >
        {children}
      </select>
      <ChevronDown
        className="pointer-events-none absolute top-1/2 right-3 size-4 -translate-y-1/2 text-muted"
        aria-hidden
      />
    </div>
  );
}

interface FieldContextValue {
  id: string;
  descriptionId: string;
  errorId: string;
  hasDescription: boolean;
  hasError: boolean;
}

const FieldContext = createContext<FieldContextValue | null>(null);

/** Ids for the control inside a `Field`, for `aria-describedby`. */
export function useField(): {
  id?: string;
  "aria-describedby"?: string;
  "aria-invalid"?: boolean;
} {
  const ctx = useContext(FieldContext);
  if (!ctx) return {};
  const describedBy = [ctx.hasDescription ? ctx.descriptionId : null, ctx.hasError ? ctx.errorId : null]
    .filter(Boolean)
    .join(" ");
  return {
    id: ctx.id,
    "aria-describedby": describedBy || undefined,
    "aria-invalid": ctx.hasError || undefined,
  };
}

interface FieldProps {
  label: ReactNode;
  description?: ReactNode;
  error?: string | null;
  children: ReactNode;
  className?: string;
  /** Render the label visually hidden (the control is labelled elsewhere). */
  hideLabel?: boolean;
}

/** Label, control, help text and error message, wired for assistive tech. */
export function Field({ label, description, error, children, className, hideLabel }: FieldProps) {
  const id = useId();
  const value: FieldContextValue = {
    id: `${id}-control`,
    descriptionId: `${id}-description`,
    errorId: `${id}-error`,
    hasDescription: Boolean(description),
    hasError: Boolean(error),
  };
  return (
    <FieldContext.Provider value={value}>
      <div className={cn("flex flex-col gap-1.5", className)}>
        <label htmlFor={value.id} className={cn("text-sm font-medium text-fg", hideLabel && "sr-only")}>
          {label}
        </label>
        {children}
        {description ? (
          <p id={value.descriptionId} className="text-[0.8125rem] leading-snug text-muted">
            {description}
          </p>
        ) : null}
        {error ? (
          <p id={value.errorId} role="alert" className="text-[0.8125rem] leading-snug font-medium text-danger">
            {error}
          </p>
        ) : null}
      </div>
    </FieldContext.Provider>
  );
}

interface SwitchProps {
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  label: ReactNode;
  description?: ReactNode;
  disabled?: boolean;
  className?: string;
}

/** An on/off setting: label and description on the left, toggle on the right. */
export function SwitchRow({ checked, onCheckedChange, label, description, disabled, className }: SwitchProps) {
  const id = useId();
  return (
    <div className={cn("flex items-start justify-between gap-6", className)}>
      <div className="min-w-0">
        <label id={`${id}-label`} htmlFor={`${id}-switch`} className="text-sm font-medium text-fg">
          {label}
        </label>
        {description ? (
          <p id={`${id}-description`} className="mt-0.5 text-[0.8125rem] leading-snug text-muted">
            {description}
          </p>
        ) : null}
      </div>
      <Switch
        id={`${id}-switch`}
        checked={checked}
        onCheckedChange={onCheckedChange}
        disabled={disabled}
        aria-describedby={description ? `${id}-description` : undefined}
      />
    </div>
  );
}

/** The toggle itself. Use `SwitchRow` for a labelled setting. */
export function Switch({
  checked,
  onCheckedChange,
  className,
  ...props
}: Omit<ComponentProps<"button">, "onChange"> & {
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      onClick={() => onCheckedChange(!checked)}
      className={cn(
        "relative mt-0.5 inline-flex h-6 w-10 shrink-0 cursor-pointer items-center rounded-full border transition-colors duration-150 disabled:cursor-not-allowed disabled:opacity-50",
        checked ? "border-transparent bg-accent" : "border-line-strong bg-raised",
        className,
      )}
      {...props}
    >
      <span
        aria-hidden
        className={cn(
          "absolute top-1/2 left-[2px] size-[1.125rem] -translate-y-1/2 rounded-full shadow-card transition-[translate,background-color] duration-200 ease-[var(--ease-out-expo)]",
          checked ? "translate-x-4 bg-on-accent" : "translate-x-0 bg-muted",
        )}
      />
    </button>
  );
}

export interface SegmentOption<T extends string> {
  value: T;
  label: ReactNode;
  /** Accessible name when `label` is an icon. */
  ariaLabel?: string;
}

interface SegmentedProps<T extends string> {
  value: T;
  onChange: (value: T) => void;
  options: readonly SegmentOption<T>[];
  /** Group label for assistive tech (shown by the surrounding `legend`/heading). */
  label: string;
  className?: string;
  size?: "sm" | "md";
  disabled?: boolean;
  /**
   * Stack the options vertically on phones, for rows with more choices than
   * fit side by side at 390px. Each option may then show `description`.
   */
  stackOnPhones?: boolean;
}

/**
 * A row of mutually exclusive choices built on native radios, so arrow keys
 * move the selection and screen readers announce "1 of 5".
 */
export function Segmented<T extends string>({
  value,
  onChange,
  options,
  label,
  className,
  size = "md",
  disabled,
  stackOnPhones,
}: SegmentedProps<T>) {
  const name = useId();
  return (
    <div
      role="radiogroup"
      aria-label={label}
      className={cn(
        "w-full rounded-md border border-line-strong/60 bg-sunken p-0.5",
        stackOnPhones ? "flex flex-col gap-0.5 sm:inline-flex sm:flex-row sm:gap-0" : "inline-flex",
        disabled && "opacity-60",
        className,
      )}
    >
      {options.map((option) => {
        const selected = option.value === value;
        return (
          <label
            key={option.value}
            className={cn(
              "relative flex min-w-0 flex-1 cursor-pointer items-center rounded-[5px] px-2 font-medium transition-[background-color,color,box-shadow] duration-150 has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-offset-1 has-[:focus-visible]:outline-accent-ink",
              stackOnPhones ? "justify-start px-3 sm:justify-center sm:px-2 sm:text-center" : "justify-center text-center",
              size === "sm" ? "h-7 text-[0.8125rem]" : stackOnPhones ? "h-10 text-sm sm:h-8" : "h-8 text-sm",
              // The selected option carries a 1.5px gold ring (≥3:1 against the
              // track in both themes), like a selected choice card.
              selected
                ? "bg-surface text-fg shadow-card ring-[1.5px] ring-accent-ink ring-inset dark:bg-raised"
                : "text-muted hover:text-fg",
              disabled && "cursor-not-allowed",
            )}
          >
            <input
              type="radio"
              name={name}
              value={option.value}
              checked={selected}
              disabled={disabled}
              onChange={() => onChange(option.value)}
              aria-label={option.ariaLabel}
              className="sr-only"
            />
            <span className="truncate">{option.label}</span>
          </label>
        );
      })}
    </div>
  );
}

interface ChoiceCardProps {
  name: string;
  value: string;
  checked: boolean;
  onChange: () => void;
  title: ReactNode;
  description?: ReactNode;
  children?: ReactNode;
  badge?: ReactNode;
  className?: string;
  disabled?: boolean;
}

/**
 * A large radio option: title, one-line explanation, optional detail. The
 * radio is named by its title alone and described by the rest, so screen
 * readers don't read the whole card on every arrow key.
 */
export function ChoiceCard({
  name,
  value,
  checked,
  onChange,
  title,
  description,
  children,
  badge,
  className,
  disabled,
}: ChoiceCardProps) {
  const id = useId();
  const describedBy = [
    badge ? `${id}-badge` : null,
    description ? `${id}-description` : null,
    children ? `${id}-detail` : null,
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <label
      className={cn(
        "group relative flex cursor-pointer flex-col gap-1 rounded-lg border bg-surface p-4 text-left transition-[border-color,background-color,box-shadow] duration-150 has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-offset-2 has-[:focus-visible]:outline-accent-ink",
        checked
          ? "border-accent-ink bg-accent-soft/40 shadow-card ring-1 ring-accent-ink"
          : "border-line hover:border-line-strong",
        disabled && "cursor-not-allowed opacity-60",
        className,
      )}
    >
      <input
        type="radio"
        name={name}
        value={value}
        checked={checked}
        onChange={onChange}
        disabled={disabled}
        aria-labelledby={`${id}-title`}
        aria-describedby={describedBy || undefined}
        className="sr-only"
      />
      <span className="flex items-start justify-between gap-3">
        <span className="flex items-center gap-2.5 text-[0.9375rem] font-semibold text-fg">
          <span
            aria-hidden
            className={cn(
              "grid size-4 shrink-0 place-items-center rounded-full border transition-colors",
              checked ? "border-accent-ink bg-accent-ink" : "border-line-strong bg-surface",
            )}
          >
            <span className={cn("size-1.5 rounded-full", checked ? "bg-surface" : "bg-transparent")} />
          </span>
          <span id={`${id}-title`}>{title}</span>
        </span>
        {badge ? <span id={`${id}-badge`}>{badge}</span> : null}
      </span>
      {description ? (
        <span id={`${id}-description`} className="pl-[1.625rem] text-[0.8125rem] leading-snug text-muted">
          {description}
        </span>
      ) : null}
      {children ? (
        <span id={`${id}-detail`} className="pl-[1.625rem]">
          {children}
        </span>
      ) : null}
    </label>
  );
}
