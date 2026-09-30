/**
 * TypeScript mirror of `crates/chrysopoeia-core`. The serde JSON shape of the
 * Rust types is the API contract, so this file follows it exactly:
 * snake_case fields, lowercase/snake_case enum strings, `Option<T>` as
 * `T | null`, `Uuid` and `DateTime<Utc>` as strings, integers as numbers.
 *
 * Types that only exist in the REST layer (list envelopes, presets, the
 * folder browser) are at the bottom and follow docs/ARCHITECTURE.md.
 */

/** RFC 3339 UTC timestamp. */
export type Timestamp = string;
/** Hyphenated lowercase UUID. */
export type Uuid = string;

// ---------------------------------------------------------------------------
// codec.rs
// ---------------------------------------------------------------------------

export type VideoCodec = "av1" | "hevc" | "h264" | "vp9";
export type AudioCodec = "copy" | "opus" | "aac" | "flac" | "ac3" | "eac3" | "mp3" | "vorbis";
export type Container = "mkv" | "mp4" | "webm";

export const VIDEO_CODECS: readonly VideoCodec[] = ["av1", "hevc", "h264", "vp9"];
export const AUDIO_CODECS: readonly AudioCodec[] = [
  "copy",
  "opus",
  "aac",
  "flac",
  "ac3",
  "eac3",
  "mp3",
  "vorbis",
];
export const CONTAINERS: readonly Container[] = ["mkv", "mp4", "webm"];

// ---------------------------------------------------------------------------
// encoder.rs
// ---------------------------------------------------------------------------

export type HwApi =
  | "software"
  | "nvenc"
  | "qsv"
  | "vaapi"
  | "videotoolbox"
  | "amf"
  | "rkmpp"
  | "v4l2m2m";

export interface EncoderInfo {
  name: string;
  codec: VideoCodec;
  api: HwApi;
}

// ---------------------------------------------------------------------------
// hardware.rs
// ---------------------------------------------------------------------------

export type HwPreference =
  | "auto"
  | "cpu"
  | "nvenc"
  | "qsv"
  | "vaapi"
  | "videotoolbox"
  | "amf"
  | "rkmpp"
  | "v4l2m2m";

export type GpuVendor = "nvidia" | "intel" | "amd" | "apple" | "other";

export interface CpuInfo {
  model: string;
  logical_cores: number;
  physical_cores: number | null;
  cgroup_limit: number | null;
}

export interface MemoryInfo {
  total_bytes: number;
  available_bytes: number;
  cgroup_limit_bytes: number | null;
}

export interface GpuDevice {
  vendor: GpuVendor;
  name: string;
  render_node: string | null;
  driver: string | null;
}

export interface EncoderStatus {
  name: string;
  codec: VideoCodec;
  api: HwApi;
  available: boolean;
  verified: boolean;
  device: string | null;
  error: string | null;
}

export interface EncoderCandidate {
  name: string;
  codec: VideoCodec;
  api: HwApi;
  device: string | null;
  hw_decode: boolean;
}

export interface FfmpegInfo {
  ffmpeg_path: string;
  ffprobe_path: string;
  found: boolean;
  ffprobe_found: boolean;
  version: string | null;
}

export interface JobRecommendation {
  cpu_jobs: number;
  gpu_jobs: number;
  total: number;
  reason: string;
}

export type SetupHintLevel = "info" | "warning" | "error";

export interface SetupHint {
  level: SetupHintLevel;
  title: string;
  detail: string;
  fix: string | null;
}

export interface HardwareInfo {
  cpu: CpuInfo;
  memory: MemoryInfo;
  gpus: GpuDevice[];
  encoders: EncoderStatus[];
  audio_encoders: string[];
  filters: string[];
  ffmpeg: FfmpegInfo;
  recommended_jobs: JobRecommendation;
  hints: SetupHint[];
  in_container: boolean;
  /**
   * True only for the stand-in the server returns while its first detection
   * runs; the other fields are placeholders until `hardware.updated` arrives.
   */
  detecting: boolean;
  detected_at: Timestamp;
}

// ---------------------------------------------------------------------------
// media.rs
// ---------------------------------------------------------------------------

export type StreamKind = "video" | "audio" | "subtitle" | "attachment" | "data";
export type HdrFormat = "hdr10" | "hdr10_plus" | "hlg" | "dolby_vision";

export interface StreamInfo {
  index: number;
  kind: StreamKind | null;
  codec: string;
  profile: string | null;
  language: string | null;
  title: string | null;
  is_default: boolean;
  is_forced: boolean;
  is_attached_pic: boolean;
  bit_rate: number | null;
  width: number | null;
  height: number | null;
  pix_fmt: string | null;
  bit_depth: number | null;
  frame_rate: number | null;
  color_primaries: string | null;
  color_transfer: string | null;
  color_space: string | null;
  color_range: string | null;
  hdr: HdrFormat | null;
  /**
   * Dolby Vision without a standard base layer (profile 5, or compatibility
   * id 0): it can't be converted without ruining its colours. Only sent
   * when true.
   */
  dolby_vision_without_base_layer?: boolean;
  interlaced: boolean;
  /** HDR10 static metadata: the mastering display. Only sent when known. */
  mastering_display?: MasteringDisplay;
  /** HDR10 static metadata: content light levels. Only sent when known. */
  content_light?: ContentLight;
  channels: number | null;
  channel_layout: string | null;
  sample_rate: number | null;
}

/** The colour volume of the display an HDR video was mastered on (SMPTE ST 2086). */
export interface MasteringDisplay {
  /** CIE 1931 xy chromaticity of each primary and the white point. */
  red: [number, number];
  green: [number, number];
  blue: [number, number];
  white_point: [number, number];
  /** Peak luminance in cd/m² (nits). */
  max_luminance: number;
  /** Black level in cd/m². */
  min_luminance: number;
}

/** Content light levels of an HDR10 video (CTA-861.3), in cd/m². */
export interface ContentLight {
  /** Brightest pixel of the whole video (MaxCLL). */
  max_cll: number;
  /** Brightest frame on average (MaxFALL). */
  max_fall: number;
}

export interface ProbeInfo {
  container: string;
  format_long_name: string | null;
  duration_secs: number | null;
  bit_rate: number | null;
  size_bytes: number;
  start_time: number | null;
  chapters: number;
  streams: StreamInfo[];
}

// ---------------------------------------------------------------------------
// profile.rs
// ---------------------------------------------------------------------------

export type Goal = "save_space" | "balanced" | "compatible" | "archive" | "custom";
export type QualityLevel = "smallest" | "small" | "balanced" | "high" | "best";
export type SpeedPreset = "fast" | "balanced" | "thorough";
export type SubtitlePolicy = "keep" | "drop";

export const QUALITY_LEVELS: readonly QualityLevel[] = [
  "smallest",
  "small",
  "balanced",
  "high",
  "best",
];
export const SPEED_PRESETS: readonly SpeedPreset[] = ["fast", "balanced", "thorough"];

export interface TranscodeProfile {
  goal: Goal;
  video_codec: VideoCodec;
  audio_codec: AudioCodec;
  container: Container;
  quality: QualityLevel;
  speed: SpeedPreset;
  quality_override: number | null;
  max_height: number | null;
  subtitles: SubtitlePolicy;
  audio_languages: string[];
  subtitle_languages: string[];
  skip_efficient: boolean;
  min_savings_pct: number | null;
}

// ---------------------------------------------------------------------------
// settings.rs
// ---------------------------------------------------------------------------

export type OutputMode = "replace" | "folder";
export type ValidationLevel = "off" | "quick" | "standard" | "thorough";

export interface ActiveHours {
  start: number;
  end: number;
}

export interface Settings {
  auto_queue: boolean;
  watch_folders: boolean;
  rescan_interval_hours: number;
  max_jobs: number | null;
  hardware: HwPreference;
  cpu_fallback: boolean;
  validation: ValidationLevel;
  output_mode: OutputMode;
  output_folder: string | null;
  temp_dir: string | null;
  keep_file_dates: boolean;
  low_priority: boolean;
  active_hours: ActiveHours | null;
  ignore_patterns: string[];
  min_file_size_mb: number;
  default_profile: TranscodeProfile;
  onboarded: boolean;
}

// ---------------------------------------------------------------------------
// library.rs
// ---------------------------------------------------------------------------

export type FileStatus = "pending" | "queued" | "processing" | "done" | "skipped" | "failed";

export const FILE_STATUSES: readonly FileStatus[] = [
  "pending",
  "queued",
  "processing",
  "done",
  "skipped",
  "failed",
];

export interface MediaFile {
  id: Uuid;
  library_id: Uuid;
  path: string;
  relative_path: string;
  file_name: string;
  size_bytes: number;
  modified_at: Timestamp;
  status: FileStatus;
  container: string | null;
  video_codec: string | null;
  audio_codec: string | null;
  resolution: string | null;
  hdr: HdrFormat | null;
  duration_secs: number | null;
  bit_rate: number | null;
  original_size_bytes: number | null;
  saved_bytes: number | null;
  skip_reason: string | null;
  error: string | null;
  /** Machine-readable cause of `error` (see `ProblemKind`); set with every `error`. */
  problem: ProblemKind | null;
  job_id: Uuid | null;
  progress: number | null;
  /** Only present on `GET /files/{id}`. */
  probe?: ProbeInfo;
  scanned_at: Timestamp;
  updated_at: Timestamp;
}

export interface LibraryStats {
  file_count: number;
  total_bytes: number;
  pending: number;
  queued: number;
  processing: number;
  done: number;
  skipped: number;
  failed: number;
  saved_bytes: number;
  /**
   * Files still being copied into the library (they are added once they
   * stop changing).
   */
  settling: number;
}

export interface Library {
  id: Uuid;
  name: string;
  path: string;
  enabled: boolean;
  profile: TranscodeProfile;
  stats: LibraryStats;
  scanning: boolean;
  last_scan_at: Timestamp | null;
  path_error: string | null;
  created_at: Timestamp;
}

// ---------------------------------------------------------------------------
// validation.rs
// ---------------------------------------------------------------------------

export type CheckStatus = "pass" | "warn" | "fail" | "skipped";

export interface ValidationCheck {
  id: string;
  label: string;
  status: CheckStatus;
  detail: string;
  value: number | null;
}

export interface ValidationReport {
  passed: boolean;
  level: ValidationLevel;
  checks: ValidationCheck[];
  ssim_min: number | null;
  ssim_avg: number | null;
  psnr_avg: number | null;
  elapsed_secs: number;
}

// ---------------------------------------------------------------------------
// job.rs
// ---------------------------------------------------------------------------

export type JobState = "queued" | "running" | "done" | "skipped" | "failed" | "cancelled";

/**
 * Why a file couldn't be converted (core `ProblemKind`), sent with every
 * `error` so the UI can group problems and offer the fix without parsing
 * sentences. A newer server may add kinds; unknown ones read as `other`.
 */
export type ProblemKind =
  | "unreadable_source"
  | "work_folder"
  | "destination"
  | "disk_full"
  | "encoder"
  | "hardware_unavailable"
  | "verification"
  | "source_changed"
  | "other";

export const PROBLEM_KINDS: readonly ProblemKind[] = [
  "unreadable_source",
  "work_folder",
  "destination",
  "disk_full",
  "encoder",
  "hardware_unavailable",
  "verification",
  "source_changed",
  "other",
];
export type JobStage = "waiting" | "preparing" | "transcoding" | "verifying" | "finalizing";

export interface Job {
  id: Uuid;
  file_id: Uuid;
  library_id: Uuid;
  file_name: string;
  file_path: string;
  state: JobState;
  stage: JobStage;
  priority: number;
  progress: number;
  fps: number | null;
  speed: number | null;
  eta_secs: number | null;
  encoder: string | null;
  hw_api: HwApi | null;
  attempt: number;
  input_size: number;
  output_size: number | null;
  error: string | null;
  /** Machine-readable cause of `error`; set with every `error`. */
  problem: ProblemKind | null;
  skip_reason: string | null;
  validation: ValidationReport | null;
  command: string | null;
  log_tail: string | null;
  /**
   * Plain-language compromises the conversion made, e.g. "Removed 2
   * picture-based subtitles because MP4 can't hold them".
   */
  notes: string[];
  /** Queued with "Convert anyway". */
  force: boolean;
  created_at: Timestamp;
  started_at: Timestamp | null;
  finished_at: Timestamp | null;
}

export interface JobProgress {
  job_id: Uuid;
  file_id: Uuid;
  stage: JobStage;
  progress: number;
  fps: number | null;
  speed: number | null;
  eta_secs: number | null;
  encoder: string | null;
  hw_api: HwApi | null;
  attempt: number;
}

// ---------------------------------------------------------------------------
// event.rs
// ---------------------------------------------------------------------------

export type ActivityLevel = "info" | "success" | "warning" | "error";

export interface ActivityEntry {
  id: number;
  at: Timestamp;
  level: ActivityLevel;
  message: string;
  file_id: Uuid | null;
  job_id: Uuid | null;
  library_id: Uuid | null;
}

export interface QueueState {
  paused: boolean;
  running: number;
  queued: number;
  max_jobs: number;
  max_jobs_auto: boolean;
  /**
   * Where `max_jobs` comes from: hardware detection, the container's
   * `MAX_JOBS` variable, or a number saved in Settings.
   */
  max_jobs_source: MaxJobsSource;
  waiting_for_schedule: boolean;
}

export type MaxJobsSource = "auto" | "env" | "settings";

export type ScanPhase = "discovering" | "analyzing" | "done";

export interface ScanProgress {
  library_id: Uuid;
  library_name: string;
  phase: ScanPhase;
  discovered: number;
  analyzed: number;
  to_analyze: number;
}

/** WebSocket messages from `GET /api/ws`, tagged by `type`. */
export type ServerEvent =
  | ({ type: "job.progress" } & JobProgress)
  | { type: "job.updated"; job: Job }
  | { type: "file.updated"; file: MediaFile }
  | { type: "files.changed"; library_id: Uuid | null }
  | { type: "library.updated"; library: Library }
  | { type: "library.removed"; id: Uuid }
  | ({ type: "scan.progress" } & ScanProgress)
  | ({ type: "queue.state" } & QueueState)
  | { type: "stats.updated"; totals: LibraryStats }
  | { type: "hardware.updated"; hardware: HardwareInfo }
  | { type: "settings.updated"; settings: Settings }
  | { type: "activity"; entry: ActivityEntry };

// ---------------------------------------------------------------------------
// stats.rs
// ---------------------------------------------------------------------------

export interface CodecCount {
  name: string;
  files: number;
  bytes: number;
}

export interface SavingsPoint {
  date: string;
  saved_bytes: number;
  files: number;
}

export interface Overview {
  totals: LibraryStats;
  video_codecs: CodecCount[];
  audio_codecs: CodecCount[];
  resolutions: CodecCount[];
  savings_history: SavingsPoint[];
  projected_savings_bytes: number | null;
  queue: QueueState;
}

// ---------------------------------------------------------------------------
// system.rs
// ---------------------------------------------------------------------------

/** `GET /api/system`: facts about the server the settings screens explain. */
export interface SystemInfo {
  version: string;
  /** Image build label (`CHRYSOPOEIA_VERSION`, e.g. `edge-1a2b3c4`) when it differs from `version`. */
  build: string | null;
  /**
   * Scratch folder used when `Settings.temp_dir` is unset (`--temp-dir` /
   * `TEMP_DIR`, e.g. `/temp` in Docker). `null` means next to each file.
   */
  default_temp_dir: string | null;
  browse_roots: string[];
  data_dir: string;
  in_container: boolean;
}

// ---------------------------------------------------------------------------
// REST-only shapes (docs/ARCHITECTURE.md, "REST API")
// ---------------------------------------------------------------------------

/** Envelope of every list endpoint. */
export interface ListResponse<T> {
  items: T[];
  total: number;
}

export interface Health {
  ok: boolean;
  version: string;
}

export interface FileDetail {
  file: MediaFile;
  jobs: Job[];
}

export interface GoalPreset {
  goal: Goal;
  title: string;
  summary: string;
  profile: TranscodeProfile;
}

export interface VideoCodecPreset {
  codec: VideoCodec;
  label: string;
  royalty_free: boolean;
  hw_accelerated: boolean;
  /** Verified encoder names for this codec. */
  encoders: string[];
}

export interface AudioCodecPreset {
  codec: AudioCodec;
  label: string;
}

export interface ContainerPreset {
  container: Container;
  label: string;
  video: VideoCodec[];
  audio: AudioCodec[];
}

export interface Presets {
  goals: GoalPreset[];
  video_codecs: VideoCodecPreset[];
  audio_codecs: AudioCodecPreset[];
  containers: ContainerPreset[];
}

export interface FsEntry {
  name: string;
  path: string;
  is_dir: boolean;
  /** Videos in this folder and the folders inside it (audio-only files aren't counted). */
  media_count?: number | null;
  /** True when counting stopped early, so `media_count` is a lower bound ("1,000+"). */
  media_count_capped?: boolean;
}

export interface FsBrowse {
  path: string;
  parent: string | null;
  roots: string[];
  entries: FsEntry[];
  /**
   * Videos in the browsed folder itself and the folders inside it, counted
   * like the entries'. Left out when a file inside can't be checked.
   */
  media_count?: number | null;
  /** True when counting the browsed folder stopped early ("1,000+"). */
  media_count_capped?: boolean;
}

export type BulkAction = "queue" | "skip" | "retry_failed";

export interface BulkRequest {
  action: BulkAction;
  ids?: Uuid[];
  library?: Uuid;
  status?: FileStatus;
}

export interface Affected {
  affected: number;
}

/** Answer of `POST /files/bulk`. */
export interface BulkResult extends Affected {
  /**
   * Bulk "queue" with explicit ids: files left out because the library's
   * settings wouldn't convert them (0 for every other request).
   */
  left_out: number;
}

/** Body of `POST /files/{id}/queue`. */
export interface QueueFileRequest {
  priority?: number;
  /** Convert once without the library's skip rules ("Convert anyway"). Checks still run. */
  force?: boolean;
}

export type JobListState = "active" | "running" | "queued" | "history";

export type FileSort = "name" | "-name" | "size" | "-size" | "updated" | "-updated" | "status" | "-status";

export interface FileQuery {
  library?: Uuid;
  status?: FileStatus;
  q?: string;
  sort?: FileSort;
  limit?: number;
  offset?: number;
}

export interface JobQuery {
  state?: JobListState;
  limit?: number;
  offset?: number;
}

export interface CreateLibraryRequest {
  path: string;
  name?: string;
  goal?: Goal;
  profile?: TranscodeProfile;
}

export interface UpdateLibraryRequest {
  name?: string;
  enabled?: boolean;
  profile?: TranscodeProfile;
}
