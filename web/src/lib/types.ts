// Types matching Rust backend models

export type VideoCodec = "h264" | "h265" | "hevc" | "av1" | "vp9" | "vp8" | "theora" | "prores" | "dnxhd" | "mpeg2";
export type AudioCodec = "aac" | "opus" | "flac" | "vorbis" | "ac3" | "eac3" | "dts" | "mp3" | "pcm";
export type ContainerFormat = "mkv" | "mp4" | "mov" | "avi" | "webm" | "ts" | "flv" | "ogg";

export type OutputFormat = "av1" | "vp9" | "hevc" | "h264";
export type OutputAudioFormat = "opus" | "flac" | "aac" | "copy";
export type OutputContainer = "mkv" | "webm" | "mp4";

export interface HwSupport {
  encode: boolean;
  decode: boolean;
  api: string | null;
}

export interface FormatOption {
  codec: OutputFormat;
  label: string;
  description: string;
  open: boolean;
  hw: HwSupport;
}

export interface AudioFormatOption {
  codec: OutputAudioFormat;
  label: string;
  open: boolean;
}

// Per-library transcode settings
export interface LibraryTranscodeConfig {
  output_video: OutputFormat;
  output_audio: OutputAudioFormat;
  output_container: OutputContainer;
  crf: number;
  skip_open_formats: boolean;
}

export interface MediaFile {
  id: string;
  path: string;
  filename: string;
  library_path: string; // which library this file belongs to
  format: ContainerFormat;
  video_codec: VideoCodec | null;
  audio_codec: AudioCodec | null;
  resolution: string | null;
  duration_secs: number;
  size_bytes: number;
  bitrate_kbps: number;
  status: "pending" | "queued" | "transcoding" | "complete" | "skipped" | "error";
  progress: number;
  speed: string | null;
  eta_secs: number | null;
  output_size_bytes: number | null;
  error_message: string | null;
  scanned_at: string;
}

export interface LibraryPath {
  id: string;
  path: string;
  file_count: number;
  total_size_bytes: number;
  enabled: boolean;
  transcode: LibraryTranscodeConfig;
}

export interface LibraryStats {
  total_files: number;
  pending: number;
  queued: number;
  transcoding: number;
  complete: number;
  skipped: number;
  errored: number;
  total_size_bytes: number;
  saved_bytes: number;
}

export interface HardwareInfo {
  gpu_name: string | null;
  gpu_vendor: "nvidia" | "amd" | "intel" | "apple" | null;
  formats: FormatOption[];
  cpu_cores: number;
  ram_gb: number;
}

export interface LogEntry {
  id: string;
  timestamp: string;
  level: "info" | "warn" | "error" | "success";
  message: string;
  fileId?: string;
  fileName?: string;
}

export interface AppConfig {
  library_paths: {
    path: string;
    file_count: number;
    total_size_bytes: number;
    enabled: boolean;
  }[];
  output_video: OutputFormat;
  output_audio: OutputAudioFormat;
  output_container: OutputContainer;
  crf: number;
  concurrent_jobs: number;
  auto_scan: boolean;
  auto_transcode: boolean;
  skip_open_formats: boolean;
}

export interface GlobalSettings {
  default_video: OutputFormat;
  default_audio: OutputAudioFormat;
  default_container: OutputContainer;
  default_crf: number;
  default_skip_open: boolean;
  concurrent_jobs: number;
  auto_scan: boolean;
  auto_transcode: boolean;
}
