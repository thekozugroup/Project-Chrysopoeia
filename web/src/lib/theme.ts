"use client";

/**
 * Light/dark theme. The choice is a per-browser convenience kept in
 * localStorage; "system" follows the OS. The inline script in the root layout
 * applies it before first paint so there is no flash.
 */

import { useEffect, useSyncExternalStore } from "react";
import { THEME_STORAGE_KEY } from "./theme-script";

export type ThemeChoice = "system" | "light" | "dark";

const listeners = new Set<() => void>();

function readChoice(): ThemeChoice {
  try {
    const value = window.localStorage.getItem(THEME_STORAGE_KEY);
    if (value === "light" || value === "dark") return value;
  } catch {
    // Storage blocked: fall back to the OS setting.
  }
  return "system";
}

function systemPrefersDark(): boolean {
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/** Apply a theme choice to <html>. */
export function applyTheme(choice: ThemeChoice): void {
  const dark = choice === "dark" || (choice === "system" && systemPrefersDark());
  const root = document.documentElement;
  root.classList.toggle("dark", dark);
  root.style.colorScheme = dark ? "dark" : "light";
}

/** Remember and apply a theme choice. */
export function setTheme(choice: ThemeChoice): void {
  try {
    if (choice === "system") window.localStorage.removeItem(THEME_STORAGE_KEY);
    else window.localStorage.setItem(THEME_STORAGE_KEY, choice);
  } catch {
    // Not persisted; still applied for this visit.
  }
  applyTheme(choice);
  current = choice;
  listeners.forEach((l) => l());
}

let current: ThemeChoice | null = null;

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function snapshot(): ThemeChoice {
  if (current === null) current = readChoice();
  return current;
}

/** The current theme choice, and keep "system" in sync with the OS. */
export function useTheme(): ThemeChoice {
  const choice = useSyncExternalStore(subscribe, snapshot, () => "system" as ThemeChoice);
  useEffect(() => {
    if (choice !== "system") return;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => applyTheme("system");
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, [choice]);
  return choice;
}
