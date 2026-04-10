import { create } from "zustand";
import { toast } from "sonner";
import type {
  MediaFile,
  LibraryStats,
  HardwareInfo,
  LibraryPath,
  LibraryTranscodeConfig,
  GlobalSettings,
  FormatOption,
  LogEntry,
} from "./types";

// ---------------------------------------------------------------------------
// Mock data
// ---------------------------------------------------------------------------

const MOCK_FORMATS: FormatOption[] = [
  {
    codec: "av1",
    label: "AV1",
    description: "Best compression, patent-free. Slow encoding without hardware.",
    open: true,
    hw: { encode: true, decode: true, api: "nvenc" },
  },
  {
    codec: "vp9",
    label: "VP9",
    description: "Good compression, patent-free. Widely supported.",
    open: true,
    hw: { encode: false, decode: true, api: null },
  },
  {
    codec: "hevc",
    label: "HEVC/H.265",
    description: "Great compression, patent-encumbered.",
    open: false,
    hw: { encode: true, decode: true, api: "nvenc" },
  },
  {
    codec: "h264",
    label: "H.264",
    description: "Universal compatibility, patent-encumbered. Larger files.",
    open: false,
    hw: { encode: true, decode: true, api: "nvenc" },
  },
];

const MOCK_HARDWARE: HardwareInfo = {
  gpu_name: "NVIDIA RTX 4090",
  gpu_vendor: "nvidia",
  formats: MOCK_FORMATS,
  cpu_cores: 16,
  ram_gb: 64,
};

const MOCK_STATS: LibraryStats = {
  total_files: 2847,
  pending: 891,
  queued: 12,
  transcoding: 2,
  complete: 1923,
  skipped: 14,
  errored: 5,
  total_size_bytes: 4_821_000_000_000,
  saved_bytes: 1_247_000_000_000,
};

const MOCK_GLOBAL: GlobalSettings = {
  default_video: "av1",
  default_audio: "opus",
  default_container: "mkv",
  default_crf: 28,
  default_skip_open: true,
  concurrent_jobs: 2,
  auto_scan: true,
  auto_transcode: true,
};

const MOCK_LIBRARY_PATHS: LibraryPath[] = [
  {
    id: "lib-movies",
    path: "/mnt/media/movies",
    file_count: 1842,
    total_size_bytes: 3_200_000_000_000,
    enabled: true,
    transcode: {
      output_video: "av1",
      output_audio: "opus",
      output_container: "mkv",
      crf: 28,
      skip_open_formats: true,
    },
  },
  {
    id: "lib-tv",
    path: "/mnt/media/tv",
    file_count: 892,
    total_size_bytes: 1_400_000_000_000,
    enabled: true,
    transcode: {
      output_video: "hevc",
      output_audio: "aac",
      output_container: "mkv",
      crf: 24,
      skip_open_formats: false,
    },
  },
  {
    id: "lib-anime",
    path: "/mnt/media/anime",
    file_count: 113,
    total_size_bytes: 221_000_000_000,
    enabled: true,
    transcode: {
      output_video: "vp9",
      output_audio: "opus",
      output_container: "webm",
      crf: 30,
      skip_open_formats: true,
    },
  },
];

const MOCK_FILES: MediaFile[] = [
  {
    id: "f-001",
    path: "/mnt/media/movies/Blade.Runner.2049.2017.2160p.UHD.BluRay.x264.mkv",
    filename: "Blade.Runner.2049.2017.2160p.UHD.BluRay.x264.mkv",
    library_path: "/mnt/media/movies",
    format: "mkv",
    video_codec: "h264",
    audio_codec: "ac3",
    resolution: "3840x2160",
    duration_secs: 9924,
    size_bytes: 58_200_000_000,
    bitrate_kbps: 46900,
    status: "transcoding",
    progress: 67,
    speed: "1.4x",
    eta_secs: 1842,
    output_size_bytes: 33_756_000_000,
    error_message: null,
    scanned_at: "2026-04-09T10:00:00Z",
  },
  {
    id: "f-002",
    path: "/mnt/media/movies/Dune.Part.Two.2024.2160p.WEB-DL.DDP5.1.Atmos.H.265.mkv",
    filename: "Dune.Part.Two.2024.2160p.WEB-DL.DDP5.1.Atmos.H.265.mkv",
    library_path: "/mnt/media/movies",
    format: "mkv",
    video_codec: "h265",
    audio_codec: "eac3",
    resolution: "3840x2160",
    duration_secs: 9960,
    size_bytes: 22_100_000_000,
    bitrate_kbps: 17700,
    status: "transcoding",
    progress: 23,
    speed: "0.8x",
    eta_secs: 9600,
    output_size_bytes: null,
    error_message: null,
    scanned_at: "2026-04-08T14:22:00Z",
  },
  {
    id: "f-003",
    path: "/mnt/media/tv/The.Expanse.S04E10.Cibola.Burn.1080p.BluRay.x264.mkv",
    filename: "The.Expanse.S04E10.Cibola.Burn.1080p.BluRay.x264.mkv",
    library_path: "/mnt/media/tv",
    format: "mkv",
    video_codec: "h264",
    audio_codec: "aac",
    resolution: "1920x1080",
    duration_secs: 2700,
    size_bytes: 4_800_000_000,
    bitrate_kbps: 14200,
    status: "queued",
    progress: 0,
    speed: null,
    eta_secs: null,
    output_size_bytes: null,
    error_message: null,
    scanned_at: "2026-04-09T10:00:00Z",
  },
  {
    id: "f-004",
    path: "/mnt/media/anime/Vinland.Saga.S02E24.1080p.BluRay.x264.mkv",
    filename: "Vinland.Saga.S02E24.1080p.BluRay.x264.mkv",
    library_path: "/mnt/media/anime",
    format: "mkv",
    video_codec: "h264",
    audio_codec: "flac",
    resolution: "1920x1080",
    duration_secs: 1440,
    size_bytes: 2_400_000_000,
    bitrate_kbps: 13300,
    status: "queued",
    progress: 0,
    speed: null,
    eta_secs: null,
    output_size_bytes: null,
    error_message: null,
    scanned_at: "2026-04-09T10:00:00Z",
  },
  {
    id: "f-005",
    path: "/mnt/media/movies/Interstellar.2014.1080p.BluRay.x264.DTS-HD.MA.mkv",
    filename: "Interstellar.2014.1080p.BluRay.x264.DTS-HD.MA.mkv",
    library_path: "/mnt/media/movies",
    format: "mkv",
    video_codec: "h264",
    audio_codec: "ac3",
    resolution: "1920x1080",
    duration_secs: 10140,
    size_bytes: 38_600_000_000,
    bitrate_kbps: 30400,
    status: "complete",
    progress: 100,
    speed: null,
    eta_secs: null,
    output_size_bytes: 18_528_000_000,
    error_message: null,
    scanned_at: "2026-04-07T08:15:00Z",
  },
  {
    id: "f-006",
    path: "/mnt/media/movies/Everything.Everywhere.All.at.Once.2022.1080p.BluRay.AV1.Opus.mkv",
    filename: "Everything.Everywhere.All.at.Once.2022.1080p.BluRay.AV1.Opus.mkv",
    library_path: "/mnt/media/movies",
    format: "mkv",
    video_codec: "av1",
    audio_codec: "opus",
    resolution: "1920x1080",
    duration_secs: 8340,
    size_bytes: 6_200_000_000,
    bitrate_kbps: 5900,
    status: "skipped",
    progress: 0,
    speed: null,
    eta_secs: null,
    output_size_bytes: null,
    error_message: null,
    scanned_at: "2026-04-09T10:00:00Z",
  },
  {
    id: "f-007",
    path: "/mnt/media/tv/Severance.S01E09.1080p.ATVP.WEB-DL.DDP5.1.H.264.mkv",
    filename: "Severance.S01E09.1080p.ATVP.WEB-DL.DDP5.1.H.264.mkv",
    library_path: "/mnt/media/tv",
    format: "mkv",
    video_codec: "h264",
    audio_codec: "eac3",
    resolution: "1920x1080",
    duration_secs: 2580,
    size_bytes: 3_100_000_000,
    bitrate_kbps: 9600,
    status: "error",
    progress: 34,
    speed: null,
    eta_secs: null,
    output_size_bytes: null,
    error_message: "FFmpeg process exited with code 1: Insufficient disk space",
    scanned_at: "2026-04-09T10:00:00Z",
  },
];

const MOCK_LOG_ENTRIES: LogEntry[] = [
  {
    id: "log-001",
    timestamp: "2026-04-09T10:02:14Z",
    level: "info",
    message: "Processing started",
  },
  {
    id: "log-002",
    timestamp: "2026-04-09T10:02:15Z",
    level: "info",
    message: "Started transcoding Blade.Runner.2049.2017.2160p.UHD.BluRay.x264.mkv",
    fileId: "f-001",
    fileName: "Blade.Runner.2049.2017.2160p.UHD.BluRay.x264.mkv",
  },
  {
    id: "log-003",
    timestamp: "2026-04-09T10:02:16Z",
    level: "info",
    message: "Started transcoding Dune.Part.Two.2024.2160p.WEB-DL.DDP5.1.Atmos.H.265.mkv",
    fileId: "f-002",
    fileName: "Dune.Part.Two.2024.2160p.WEB-DL.DDP5.1.Atmos.H.265.mkv",
  },
  {
    id: "log-004",
    timestamp: "2026-04-09T09:58:30Z",
    level: "success",
    message: "Completed Interstellar.2014.1080p.BluRay.x264.DTS-HD.MA.mkv \u2014 saved 52%",
    fileId: "f-005",
    fileName: "Interstellar.2014.1080p.BluRay.x264.DTS-HD.MA.mkv",
  },
  {
    id: "log-005",
    timestamp: "2026-04-09T09:45:12Z",
    level: "error",
    message: "Failed Severance.S01E09.1080p.ATVP.WEB-DL.DDP5.1.H.264.mkv: FFmpeg process exited with code 1: Insufficient disk space",
    fileId: "f-007",
    fileName: "Severance.S01E09.1080p.ATVP.WEB-DL.DDP5.1.H.264.mkv",
  },
  {
    id: "log-006",
    timestamp: "2026-04-09T09:40:00Z",
    level: "info",
    message: "Library scan complete. Found 24 new files.",
  },
  {
    id: "log-007",
    timestamp: "2026-04-09T09:39:55Z",
    level: "warn",
    message: "Skipped Everything.Everywhere.All.at.Once.2022.1080p.BluRay.AV1.Opus.mkv \u2014 already open format",
    fileId: "f-006",
    fileName: "Everything.Everywhere.All.at.Once.2022.1080p.BluRay.AV1.Opus.mkv",
  },
  {
    id: "log-008",
    timestamp: "2026-04-09T09:35:00Z",
    level: "info",
    message: "Processing started",
  },
];

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
