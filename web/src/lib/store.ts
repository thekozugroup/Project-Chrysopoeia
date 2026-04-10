import { create } from "zustand";
import { toast } from "sonner";
import type {
  MediaFile,
  LibraryStats,
  HardwareInfo,
  LibraryPath,
  LibraryTranscodeConfig,
  GlobalSettings,
  LogEntry,
} from "./types";
import {
  MOCK_FILES,
  MOCK_STATS,
  MOCK_HARDWARE,
  MOCK_GLOBAL,
  MOCK_LIBRARY_PATHS,
  MOCK_LOG_ENTRIES,
} from "./mock-data";

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

interface AppStore {
  files: MediaFile[];
  stats: LibraryStats;
  hardware: HardwareInfo;

  // Global settings (defaults for new libraries)
  globalSettings: GlobalSettings;
  updateGlobalSettings: (patch: Partial<GlobalSettings>) => void;

  // Library paths with per-library transcode config
  library_paths: LibraryPath[];
  addLibraryPath: (path: string) => void;
  removeLibraryPath: (id: string) => void;
  toggleLibraryPath: (id: string) => void;
  updateLibraryConfig: (libraryId: string, patch: Partial<LibraryTranscodeConfig>) => void;

  // Selected library for sidebar filtering
  selectedLibraryId: string | null;
  setSelectedLibrary: (id: string | null) => void;

  // Settings page/modal
  settingsOpen: boolean;
  setSettingsOpen: (v: boolean) => void;

  // Main area tab
  activeTab: "overview" | "queue" | "settings";
  setActiveTab: (tab: "overview" | "queue" | "settings") => void;

  // Adding new library (inline in sidebar)
  isAddingLibrary: boolean;
  setIsAddingLibrary: (v: boolean) => void;

  // Theme
  theme: "dark" | "light";
  toggleTheme: () => void;

  isLoading: boolean;
  setIsLoading: (v: boolean) => void;
  isProcessing: boolean;
  startProcessing: () => void;
  stopProcessing: () => void;
  isScanning: boolean;
  startScan: () => void;
  scanComplete: (newFiles?: number) => void;
  wsConnected: boolean;
  setWsConnected: (v: boolean) => void;

  // WS-driven actions
  updateFileProgress: (
    fileId: string,
    progress: number,
    speed: string | null,
    eta: number | null,
  ) => void;
  updateFileStatus: (
    fileId: string,
    status: MediaFile["status"],
    outputSize?: number | null,
    error?: string | null,
  ) => void;
  updateStats: (stats: LibraryStats) => void;

  // Activity log
  logEntries: LogEntry[];
  addLogEntry: (entry: LogEntry) => void;
  clearLog: () => void;
}

export const useAppStore = create<AppStore>((set) => ({
  files: MOCK_FILES,
  stats: MOCK_STATS,
  hardware: MOCK_HARDWARE,

  // Global settings
  globalSettings: MOCK_GLOBAL,
  updateGlobalSettings: (patch) =>
    set((state) => ({ globalSettings: { ...state.globalSettings, ...patch } })),

  // Library paths
  library_paths: MOCK_LIBRARY_PATHS,

  addLibraryPath: (path) =>
    set((state) => ({
      library_paths: [
        ...state.library_paths,
        {
          id: `lib-${Date.now()}`,
          path,
          file_count: 0,
          total_size_bytes: 0,
          enabled: true,
          transcode: {
            output_video: state.globalSettings.default_video,
            output_audio: state.globalSettings.default_audio,
            output_container: state.globalSettings.default_container,
            crf: state.globalSettings.default_crf,
            skip_open_formats: state.globalSettings.default_skip_open,
          },
        },
      ],
    })),

  removeLibraryPath: (id) =>
    set((state) => ({
      library_paths: state.library_paths.filter((p) => p.id !== id),
    })),

  toggleLibraryPath: (id) =>
    set((state) => ({
      library_paths: state.library_paths.map((p) =>
        p.id === id ? { ...p, enabled: !p.enabled } : p,
      ),
    })),

  updateLibraryConfig: (libraryId, patch) =>
    set((state) => ({
      library_paths: state.library_paths.map((p) =>
        p.id === libraryId
          ? { ...p, transcode: { ...p.transcode, ...patch } }
          : p,
      ),
    })),

  // Selected library
  selectedLibraryId: null,
  setSelectedLibrary: (id) => set({ selectedLibraryId: id }),

  // Settings
  settingsOpen: false,
  setSettingsOpen: (v) => set({ settingsOpen: v }),

  // Tab
  activeTab: "overview",
  setActiveTab: (tab) => set({ activeTab: tab }),

  // Adding library
  isAddingLibrary: false,
  setIsAddingLibrary: (v) => set({ isAddingLibrary: v }),

  // Theme
  theme: "dark",
  toggleTheme: () =>
    set((state) => ({ theme: state.theme === "dark" ? "light" : "dark" })),

  isLoading: true,
  setIsLoading: (v) => set({ isLoading: v }),

  isProcessing: true,
  startProcessing: () => {
    set((state) => ({
      isProcessing: true,
      logEntries: [
        {
          id: `log-${Date.now()}-start`,
          timestamp: new Date().toISOString(),
          level: "info" as const,
          message: "Processing started",
        },
        ...state.logEntries,
      ].slice(0, 500),
    }));
    toast("Processing started");
  },
  stopProcessing: () => {
    set((state) => ({
      isProcessing: false,
      logEntries: [
        {
          id: `log-${Date.now()}-stop`,
          timestamp: new Date().toISOString(),
          level: "info" as const,
          message: "Processing paused",
        },
        ...state.logEntries,
      ].slice(0, 500),
    }));
    toast("Processing paused");
  },

  isScanning: false,
  startScan: () => set({ isScanning: true }),
  scanComplete: (newFiles?: number) => {
    set((state) => ({
      isScanning: false,
      logEntries: [
        {
          id: `log-${Date.now()}-scan`,
          timestamp: new Date().toISOString(),
          level: "info" as const,
          message: `Library scan complete. Found ${newFiles ?? 0} new files.`,
        },
        ...state.logEntries,
      ].slice(0, 500),
    }));
    toast.info("Scan complete", {
      description: `Found ${newFiles ?? 0} new files`,
    });
  },

  wsConnected: true,
  setWsConnected: (v) => set({ wsConnected: v }),

  // Activity log
  logEntries: MOCK_LOG_ENTRIES,
  addLogEntry: (entry) =>
    set((state) => ({
      logEntries: [entry, ...state.logEntries].slice(0, 500),
    })),
  clearLog: () => set({ logEntries: [] }),

  // WS-driven actions
  updateFileProgress: (fileId, progress, speed, eta) =>
    set((state) => ({
      files: state.files.map((f) =>
        f.id === fileId
          ? { ...f, progress, speed, eta_secs: eta }
          : f,
      ),
    })),

  updateFileStatus: (fileId, status, outputSize, error) => {
    const file = useAppStore.getState().files.find((f) => f.id === fileId);
    const name = file?.filename ?? fileId;

    let logEntry: LogEntry | null = null;
    if (status === "transcoding") {
      logEntry = {
        id: `log-${Date.now()}-${fileId}`,
        timestamp: new Date().toISOString(),
        level: "info",
        message: `Started transcoding ${name}`,
        fileId,
        fileName: name,
      };
    } else if (status === "complete" && file) {
      const original = file.size_bytes;
      const output = outputSize ?? 0;
      const reduction =
        original > 0 ? Math.round((1 - output / original) * 100) : 0;
      logEntry = {
        id: `log-${Date.now()}-${fileId}`,
        timestamp: new Date().toISOString(),
        level: "success",
        message: `Completed ${name} \u2014 saved ${reduction}%`,
        fileId,
        fileName: name,
      };
      toast.success(`Completed: ${name}`, {
        description: `Saved ${reduction}%`,
      });
    } else if (status === "error") {
      logEntry = {
        id: `log-${Date.now()}-${fileId}`,
        timestamp: new Date().toISOString(),
        level: "error",
        message: `Failed ${name}: ${error ?? "Unknown error"}`,
        fileId,
        fileName: name,
      };
      toast.error(`Failed: ${name}`, {
        description: error ?? "Unknown error",
      });
    }

    set((state) => ({
      files: state.files.map((f) =>
        f.id === fileId
          ? {
              ...f,
              status,
              output_size_bytes: outputSize ?? f.output_size_bytes,
              error_message: error ?? f.error_message,
              progress: status === "complete" ? 100 : f.progress,
            }
          : f,
      ),
      logEntries: logEntry
        ? [logEntry, ...state.logEntries].slice(0, 500)
        : state.logEntries,
    }));
  },

  updateStats: (stats) => set({ stats }),
}));
