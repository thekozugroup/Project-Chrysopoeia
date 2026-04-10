"use client";

import { useEffect } from "react";
import { Sun, Moon } from "lucide-react";
import { useAppStore } from "@/lib/store";

export function ThemeToggle() {
  const theme = useAppStore((s) => s.theme);
  const toggleTheme = useAppStore((s) => s.toggleTheme);

  useEffect(() => {
    const root = document.documentElement;
    if (theme === "dark") {
      root.classList.add("dark");
      root.classList.remove("light");
    } else {
      root.classList.add("light");
      root.classList.remove("dark");
    }
  }, [theme]);

  return (
    <button
      type="button"
      onClick={toggleTheme}
      aria-label={`Switch to ${theme === "dark" ? "light" : "dark"} mode`}
      className="fixed top-3 right-3 z-50 flex h-7 w-7 items-center justify-center rounded-full border border-border/40 bg-card/80 text-muted-foreground/60 backdrop-blur-sm transition-all hover:text-foreground hover:border-border hover:bg-card focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-gold/40"
    >
      {theme === "dark" ? (
        <Sun className="h-3.5 w-3.5" />
      ) : (
        <Moon className="h-3.5 w-3.5" />
      )}
    </button>
  );
}
