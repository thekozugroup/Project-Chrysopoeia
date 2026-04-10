"use client";

import { useEffect } from "react";
import { useAppStore } from "./store";

const INPUT_TAGS = new Set(["INPUT", "TEXTAREA", "SELECT"]);

export function useKeyboardShortcuts(onShowHelp: () => void) {
  const startProcessing = useAppStore((s) => s.startProcessing);
  const stopProcessing = useAppStore((s) => s.stopProcessing);
  const isProcessing = useAppStore((s) => s.isProcessing);
  const startScan = useAppStore((s) => s.startScan);
  const isScanning = useAppStore((s) => s.isScanning);

  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      const target = document.activeElement;
      const tagName = target?.tagName ?? "";
      const isEditable =
        INPUT_TAGS.has(tagName) ||
        (target as HTMLElement)?.isContentEditable === true;

      // Escape always works -- close dialogs/panels
      if (e.key === "Escape") {
        return; // let the dialog/sheet handle it natively
      }

      // Block shortcuts when typing in inputs
      if (isEditable) return;

      // ? -- show keyboard shortcuts help
      if (e.key === "?") {
        e.preventDefault();
        onShowHelp();
        return;
      }

      // Space -- toggle processing
      if (e.key === " ") {
        e.preventDefault();
        if (isProcessing) {
          stopProcessing();
        } else {
          startProcessing();
        }
        return;
      }

      // r/R -- trigger scan
      if (e.key === "r" || e.key === "R") {
        if (!isScanning) {
          e.preventDefault();
          startScan();
        }
        return;
      }
    }

    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [isProcessing, isScanning, startProcessing, stopProcessing, startScan, onShowHelp]);
}
