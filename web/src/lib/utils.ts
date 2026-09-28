import { clsx, type ClassValue } from "clsx";
import { useEffect, useState } from "react";
import { twMerge } from "tailwind-merge";

/** Join class names and let later Tailwind utilities win over earlier ones. */
export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}

/** Copy text to the clipboard, falling back to a hidden textarea on plain HTTP. */
export async function copyText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // Fall through to the legacy path.
  }
  try {
    const area = document.createElement("textarea");
    area.value = text;
    area.setAttribute("readonly", "");
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    const ok = document.execCommand("copy");
    document.body.removeChild(area);
    return ok;
  } catch {
    return false;
  }
}

/** Last path segment of a folder path, for default library names. */
export function baseName(path: string): string {
  const parts = path.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/** A folder name as a friendly default title: `movies` → `Movies`. */
export function titleFromFolder(path: string): string {
  const name = baseName(path).replace(/[-_]+/g, " ").trim();
  return name ? name.charAt(0).toUpperCase() + name.slice(1) : path;
}

/**
 * The value, or the last non-null one while it is `null`. Lets a sheet keep
 * its content on screen while it animates closed after its id was cleared.
 */
export function useRetained<T>(value: T | null): T | null {
  const [kept, setKept] = useState<T | null>(value);
  if (value !== null && value !== kept) setKept(value);
  return value ?? kept;
}

/** Whether `value` has been true for at least `ms` (false again at once). */
export function useSustained(value: boolean, ms: number): boolean {
  const [sustained, setSustained] = useState(false);
  useEffect(() => {
    if (!value) return;
    const timer = setTimeout(() => setSustained(true), ms);
    return () => {
      clearTimeout(timer);
      setSustained(false);
    };
  }, [value, ms]);
  return value && sustained;
}
