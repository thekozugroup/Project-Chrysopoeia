"use client";

import { useEffect } from "react";
import { useAppStore } from "./store";
import * as api from "./api";

/**
 * Attempts to fetch real data from the backend API on mount.
 * If the API is unreachable, the app stays in demo mode using mock data.
 */
export function useDataInit() {
  const setIsLoading = useAppStore((s) => s.setIsLoading);

  useEffect(() => {
    let mounted = true;

    async function init() {
      try {
        const [files, stats, libraries, hardware, config] =
          await Promise.allSettled([
            api.getFiles(),
            api.getStats(),
            api.getLibraries(),
            api.getHardware(),
            api.getConfig(),
          ]);

        if (!mounted) return;

        // Only update store fields where the API returned data.
        // Otherwise keep mock/demo data as initial state.
        if (files.status === "fulfilled") {
          useAppStore.setState({ files: files.value });
        }
        if (stats.status === "fulfilled") {
          useAppStore.setState({ stats: stats.value });
        }
        if (libraries.status === "fulfilled") {
          useAppStore.setState({ library_paths: libraries.value });
        }
        if (hardware.status === "fulfilled") {
          useAppStore.setState({ hardware: hardware.value });
        }
        if (config.status === "fulfilled") {
          useAppStore.setState({ globalSettings: config.value });
        }
      } catch {
        // API entirely unreachable — stay in demo mode with mock data
      } finally {
        if (mounted) setIsLoading(false);
      }
    }

    init();
    return () => {
      mounted = false;
    };
  }, [setIsLoading]);
}
