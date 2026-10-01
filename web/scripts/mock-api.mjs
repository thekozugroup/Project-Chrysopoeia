#!/usr/bin/env node
/**
 * DEV-ONLY mock of the Chrysopoeia API, for UI work and screenshots.
 * Every library, file and number here is FAKE sample data. The app never
 * imports this file; run it with `pnpm mock` and build or run the UI with
 * NEXT_PUBLIC_API_URL=http://localhost:8787.
 *
 * Environment:
 *   PORT=8787               port to listen on
 *   MOCK_SCENARIO=demo      demo (default) | fresh (first run) | nogpu | empty (no libraries)
 *   MOCK_TICK_MS=1000       progress tick interval
 *   MOCK_DETECT_MS=0        answer GET /api/hardware with the server's "Checking your
 *                           hardware…" stand-in for this long after start
 *   MOCK_WS=on              "off" refuses WebSocket upgrades, like a reverse proxy
 *                           without WebSocket support
 *   MOCK_MAX_JOBS=          a number: the job limit comes from the container's
 *                           MAX_JOBS (QueueState.max_jobs_source "env")
 *   MOCK_HOST=allow         "deny" answers every request 403 host_not_allowed, like
 *                           the real server reached under an unknown name
 *   MOCK_SETTLE_MS=60000    the demo's files still being copied settle this long after
 *                           start, one every few seconds ("Found … in <library>"),
 *                           and LibraryStats.settling goes down to 0 (0 = never)
 *   MOCK_FORCE=on           "off" ignores `force` on POST /api/files/{id}/queue, like a
 *                           server older than "Convert anyway" (Job.force stays false)
 *   MOCK_BULK_FAILED=0      add a "Demo Bulk" library with this many files that all
 *                           failed because the work folder can't be used (more than
 *                           one 500-file page: the UI must read them all)
 *
 * Error codes, messages and the `field` of validation errors follow the
 * real server (crates/chrysopoeia-server), so the UI's error handling is
 * exercised the same way. Also served: `GET /api/system` (with `build`),
 * `Job.notes`, `HardwareInfo.detecting`, and the round-3 additions:
 * `QueueState.max_jobs_source`, `LibraryStats.settling`, HDR10 metadata on
 * streams, `force` on queue, `left_out` on bulk queue, recursive video
 * counts with `media_count_capped`, 415 `unsupported_media_type`, and
 * nested error fields (`profile.max_height`). Failed files use the real
 * server's sentences, including its "can't be read as a video" ones.
 *
 * Round 4: every failure carries its `problem` code and the server's own
 * sentence (the demo has damaged originals, a failed check, a work folder
 * that can't be written in two libraries, a destination that can't be
 * written and a name clash in one library, and a file that was moved away
 * while it was converted: `source_changed`; an original whose content
 * changed meanwhile is a skip, as on the server), a second conversion that
 * the work folder stopped (the file stays converted), an old failure the
 * file has been converted since, the browsed folder has its own
 * `media_count`, `Job.force` echoes "Convert anyway", a converted file
 * queued again stays converted (with its savings) when the new job is
 * skipped, stopped or fails, and bulk queue leaves out files the size rule
 * already kept. Settings errors use the server's words ("Files at once
 * must be between 1 and 32.").
 *
 * Round 5 (contract additions C1-C4): `Job.freed_bytes` (0 for a converted
 * hard-linked original, whose old data stays on disk, else input minus
 * output; null until a job is done), `ActivityEntry.problem` (the cause of
 * an entry about a failed or skipped file), `Job.output_name` (a converted
 * file is renamed to its goal's extension: land1080.mp4 becomes
 * land1080.mkv, in replace mode) and a files search that also finds a file by
 * the names its jobs recorded (q=land1080.mp4 finds land1080.mkv). The demo
 * has such a file (a hard-linked one converted anyway), and a "Demo Anime"
 * library on the Plays everywhere goal whose files hold picture-based
 * subtitles or fonts MP4 can't keep: replacing their originals is skipped
 * with the worker's sentence ("MP4 can't hold this file's …, so it was left
 * unchanged. …; Convert anyway converts it without them") until converted
 * anyway.
 */

import { randomUUID } from "node:crypto";
import http from "node:http";
import { WebSocketServer } from "ws";

const PORT = Number(process.env.PORT ?? 8787);
const SCENARIO = process.env.MOCK_SCENARIO ?? "demo";
const TICK_MS = Number(process.env.MOCK_TICK_MS ?? 1000);
const DETECT_MS = Number(process.env.MOCK_DETECT_MS ?? 0);
const WS_ON = (process.env.MOCK_WS ?? "on") !== "off";
const ENV_MAX_JOBS = Number(process.env.MOCK_MAX_JOBS ?? 0) || null;
const HOST_DENY = process.env.MOCK_HOST === "deny";
const SETTLE_MS = Number(process.env.MOCK_SETTLE_MS ?? 60_000);
const BULK_FAILED = Number(process.env.MOCK_BULK_FAILED ?? 0);
const FORCE = process.env.MOCK_FORCE ?? "on";
/** The worker's sentence for a file with other hard links (SHARED_ORIGINAL), left alone when originals are replaced. */
const HARD_LINK_SKIP =
  "This file has another hard link (for example a torrent that is still seeding), so replacing it would use more space instead of saving it. It was left unchanged; Convert anyway converts it all the same";
/** The note on a hard-linked original converted anyway (SHARED_ORIGINAL_NOTE). */
const HARD_LINK_NOTE = "The original has another hard link (for example a seeding torrent), so replacing it freed no space";
/** The worker's sentence when the goal's container can't hold some of a file's tracks (plan::replace_loss). */
const lossSkip = (container, lost) =>
  `${container} can't hold this file's ${lost}, so it was left unchanged. To convert it, choose an MKV goal or save converted files to a separate folder; Convert anyway converts it without them`;
/** The server's sentence when the work folder can't be created (chrysopoeia-worker run.rs). */
const WORK_FOLDER_ERROR =
  "The work folder /temp can't be created because Chrysopoeia doesn't have permission to write in the folder above it (in Docker, the PUID/PGID user needs write access). Fix it, or choose another work folder, in Settings > Output.";
/** Jobs queued with "Convert anyway" (force). */
const forcedJobs = new Set();
const STARTED = Date.now();

// ---------------------------------------------------------------------------
// Deterministic randomness so screenshots are stable between runs.
// ---------------------------------------------------------------------------

let seed = 42;
function rand() {
  seed = (seed * 1664525 + 1013904223) % 4294967296;
  return seed / 4294967296;
}
const pick = (list) => list[Math.floor(rand() * list.length)];
const between = (a, b) => a + rand() * (b - a);

const NOW = Date.now();
const iso = (ms) => new Date(ms).toISOString();
const ago = (secs) => iso(NOW - secs * 1000);
const uuid = () => randomUUID();

// ---------------------------------------------------------------------------
// Profiles, presets and settings (mirror crates/chrysopoeia-core)
// ---------------------------------------------------------------------------

function profileForGoal(goal) {
  const base = {
    goal,
    video_codec: "av1",
    audio_codec: "opus",
    container: "mkv",
    quality: "balanced",
    speed: "balanced",
    quality_override: null,
    max_height: null,
    subtitles: "keep",
    audio_languages: [],
    subtitle_languages: [],
    skip_efficient: true,
    min_savings_pct: 10,
  };
  if (goal === "balanced") return { ...base, video_codec: "hevc", audio_codec: "copy" };
  if (goal === "compatible")
    return { ...base, video_codec: "h264", audio_codec: "aac", container: "mp4", quality: "high", skip_efficient: false, min_savings_pct: null };
  if (goal === "archive") return { ...base, audio_codec: "copy", quality: "best", speed: "thorough", min_savings_pct: 5 };
  return base;
}

const settings = {
  auto_queue: true,
  watch_folders: true,
  rescan_interval_hours: 12,
  max_jobs: null,
  hardware: "auto",
  cpu_fallback: true,
  validation: "standard",
  output_mode: "replace",
  output_folder: null,
  temp_dir: null,
  keep_file_dates: true,
  low_priority: true,
  active_hours: null,
  ignore_patterns: ["**/.*", "**/@eaDir/**", "**/#recycle/**", "**/*.partial~"],
  min_file_size_mb: 0,
  default_profile: profileForGoal("save_space"),
  onboarded: SCENARIO !== "fresh",
};

const CONTAINERS = {
  mkv: { video: ["av1", "hevc", "h264", "vp9"], audio: ["copy", "opus", "aac", "flac", "ac3", "eac3", "mp3", "vorbis"] },
  mp4: { video: ["av1", "hevc", "h264", "vp9"], audio: ["copy", "aac", "ac3", "eac3", "opus", "mp3"] },
  webm: { video: ["av1", "vp9"], audio: ["copy", "opus", "vorbis"] },
};

/** The real server's `check_profile`: values normalizing can't fix, named by their nested field. */
function checkProfile(p, field) {
  if (p && p.max_height !== null && p.max_height !== undefined && p.max_height < 144)
    throw new HttpError(400, "invalid_profile", "The largest picture height must be at least 144 lines.", `${field}.max_height`);
}

function normalizeProfile(p) {
  const out = { ...p };
  if (!CONTAINERS[out.container].video.includes(out.video_codec)) out.container = "mkv";
  if (!CONTAINERS[out.container].audio.includes(out.audio_codec)) out.audio_codec = out.container === "mp4" ? "aac" : "opus";
  if (out.min_savings_pct !== null) out.min_savings_pct = Math.min(90, out.min_savings_pct);
  return out;
}

// ---------------------------------------------------------------------------
// Hardware (fake NVIDIA card, or none)
// ---------------------------------------------------------------------------

function makeHardware() {
  const gpu = SCENARIO !== "nogpu";
  const sw = [
    ["libsvtav1", "av1"],
    ["libaom-av1", "av1"],
    ["libx265", "hevc"],
    ["libx264", "h264"],
    ["libvpx-vp9", "vp9"],
  ].map(([name, codec]) => ({ name, codec, api: "software", available: true, verified: true, device: null, error: null }));
  const hw = gpu
    ? [
        { name: "hevc_nvenc", codec: "hevc", api: "nvenc", available: true, verified: true, device: null, error: null },
        { name: "h264_nvenc", codec: "h264", api: "nvenc", available: true, verified: true, device: null, error: null },
        {
          name: "av1_nvenc",
          codec: "av1",
          api: "nvenc",
          available: true,
          verified: false,
          device: null,
          error: "This GPU can't encode AV1 (it needs an RTX 40-series or newer).\n[av1_nvenc] OpenEncodeSessionEx failed: unsupported device (2): (no details)",
        },
        { name: "hevc_vaapi", codec: "hevc", api: "vaapi", available: true, verified: false, device: null, error: "No VA-API device was found (/dev/dri is not passed to the container)." },
        { name: "h264_vaapi", codec: "h264", api: "vaapi", available: true, verified: false, device: null, error: "No VA-API device was found (/dev/dri is not passed to the container)." },
      ]
    : [
        { name: "hevc_vaapi", codec: "hevc", api: "vaapi", available: true, verified: false, device: null, error: "No VA-API device was found (/dev/dri is not passed to the container)." },
        { name: "hevc_nvenc", codec: "hevc", api: "nvenc", available: true, verified: false, device: null, error: "No NVIDIA GPU is visible inside the container." },
      ];
  return {
    cpu: { model: "Demo CPU (sample data) 8-Core", logical_cores: 16, physical_cores: 8, cgroup_limit: null },
    memory: { total_bytes: 34_359_738_368, available_bytes: 21_474_836_480, cgroup_limit_bytes: null },
    gpus: gpu ? [{ vendor: "nvidia", name: "NVIDIA GeForce RTX 3060 (demo)", render_node: null, driver: "nvidia" }] : [],
    encoders: [...sw, ...hw],
    audio_encoders: ["libopus", "aac", "flac", "ac3", "eac3", "libmp3lame", "libvorbis"],
    filters: ["ssim", "psnr", "blackdetect", "freezedetect", "bwdif"],
    ffmpeg: { ffmpeg_path: "ffmpeg", ffprobe_path: "ffprobe", found: true, ffprobe_found: true, version: "ffmpeg version 7.0.2-Jellyfin (demo)" },
    recommended_jobs: gpu
      ? { cpu_jobs: 4, gpu_jobs: 3, total: 3, reason: "3 at once: your NVIDIA GPU handles up to 3 encodes, and 16 CPU cores leave room for decoding." }
      : { cpu_jobs: 4, gpu_jobs: 0, total: 4, reason: "4 at once: 16 CPU cores and 32 GB of memory, about 4 cores per conversion." },
    hints: gpu
      ? [
          {
            level: "info",
            title: "AV1 on this GPU isn't possible",
            detail: "Your NVIDIA card encodes HEVC and H.264 in hardware. AV1 libraries will be converted on the CPU, which is slower. Consider the Balanced goal for speed.",
            fix: null,
          },
        ]
      : [
          {
            level: "warning",
            title: "No GPU is available to the container",
            detail: "Conversions run on the CPU. If this server has an Intel or AMD GPU, pass it to the container to convert several times faster.",
            fix: "--device=/dev/dri",
          },
          {
            level: "info",
            title: "Have an NVIDIA card?",
            detail: "On Unraid, install the Nvidia-Driver plugin, then add these to the container.",
            fix: "--runtime=nvidia -e NVIDIA_VISIBLE_DEVICES=all -e NVIDIA_DRIVER_CAPABILITIES=all",
          },
        ],
    in_container: true,
    detecting: false,
    detected_at: iso(Date.now() - 3 * 3600 * 1000),
  };
}

let hardware = makeHardware();

/** What the real server answers while its first detection runs (services/hardware.rs placeholder). */
function detectingPlaceholder() {
  return {
    cpu: { model: "Unknown CPU", logical_cores: 16, physical_cores: null, cgroup_limit: null },
    memory: { total_bytes: 0, available_bytes: 0, cgroup_limit_bytes: null },
    gpus: [],
    encoders: [],
    audio_encoders: [],
    filters: [],
    ffmpeg: { ffmpeg_path: "ffmpeg", ffprobe_path: "ffprobe", found: false, ffprobe_found: false, version: null },
    recommended_jobs: { cpu_jobs: 4, gpu_jobs: 0, total: 4, reason: "Checking your hardware…" },
    hints: [
      {
        level: "info",
        title: "Checking your hardware…",
        detail: "Chrysopoeia is testing which encoders work on this machine. This takes a few seconds; conversions start right after.",
        fix: null,
      },
    ],
    in_container: true,
    detecting: true,
    detected_at: iso(STARTED),
  };
}

if (DETECT_MS > 0) {
  setTimeout(() => {
    hardware = { ...makeHardware(), detected_at: iso(Date.now()) };
    broadcast({ type: "hardware.updated", hardware });
  }, DETECT_MS);
}

const presets = {
  goals: [
    { goal: "save_space", title: "Save space", summary: "Smallest files. Takes longest without a recent GPU.", profile: profileForGoal("save_space") },
    { goal: "balanced", title: "Balanced", summary: "Good savings, quick with a GPU, plays on most TVs.", profile: profileForGoal("balanced") },
    { goal: "compatible", title: "Plays everywhere", summary: "Plays on every device. Files may not get smaller.", profile: profileForGoal("compatible") },
    { goal: "archive", title: "Archive", summary: "Near-original quality for keeping masters. Smaller savings.", profile: profileForGoal("archive") },
  ],
  video_codecs: ["av1", "hevc", "h264", "vp9"].map((codec) => ({
    codec,
    label: { av1: "AV1", hevc: "HEVC (H.265)", h264: "H.264", vp9: "VP9" }[codec],
    royalty_free: codec === "av1" || codec === "vp9",
    hw_accelerated: hardware.encoders.some((e) => e.codec === codec && e.verified && e.api !== "software"),
    encoders: hardware.encoders.filter((e) => e.codec === codec && e.verified).map((e) => e.name),
  })),
  audio_codecs: [
    ["copy", "Keep original"],
    ["opus", "Opus"],
    ["aac", "AAC"],
    ["flac", "FLAC (lossless)"],
    ["ac3", "Dolby Digital (AC-3)"],
    ["eac3", "Dolby Digital Plus (E-AC-3)"],
    ["mp3", "MP3"],
    ["vorbis", "Vorbis"],
  ].map(([codec, label]) => ({ codec, label })),
  containers: Object.entries(CONTAINERS).map(([container, c]) => ({
    container,
    label: { mkv: "MKV", mp4: "MP4", webm: "WebM" }[container],
    video: c.video,
    audio: c.audio,
  })),
};

// ---------------------------------------------------------------------------
// Fake filesystem for the folder picker
// ---------------------------------------------------------------------------

const BROWSE_ROOTS = ["/"];
const FS = {
  "/": ["config", "media", "mnt", "temp"],
  "/config": [],
  "/temp": [],
  "/mnt": ["user"],
  "/mnt/user": ["appdata", "downloads"],
  "/mnt/user/appdata": [],
  "/mnt/user/downloads": [],
  "/media": ["demo"],
  "/media/demo": ["movies", "tv", "home-videos", "music"],
  "/media/demo/movies": ["Big Buck Bunny (2008)", "Sintel (2010)", "Tears of Steel (2012)"],
  "/media/demo/movies/Big Buck Bunny (2008)": [],
  "/media/demo/movies/Sintel (2010)": [],
  "/media/demo/movies/Tears of Steel (2012)": [],
  "/media/demo/tv": ["Demo Show", "Another Demo Show"],
  "/media/demo/tv/Demo Show": ["Season 01", "Season 02"],
  "/media/demo/tv/Demo Show/Season 01": [],
  "/media/demo/tv/Demo Show/Season 02": [],
  "/media/demo/tv/Another Demo Show": ["Season 01"],
  "/media/demo/tv/Another Demo Show/Season 01": [],
  "/media/demo/home-videos": [],
  "/media/demo/music": [],
};
/**
 * Videos in each folder and every folder inside it (audio-only files aren't
 * counted); folders not listed hold none.
 */
const MEDIA_COUNT = {
  "/": 1000,
  "/mnt": 3,
  "/mnt/user": 3,
  "/mnt/user/downloads": 3,
  "/media": 1000,
  "/media/demo": 412,
  "/media/demo/movies": 136,
  "/media/demo/tv": 248,
  "/media/demo/home-videos": 28,
  "/media/demo/tv/Demo Show": 164,
  "/media/demo/tv/Another Demo Show": 84,
  "/media/demo/tv/Demo Show/Season 01": 82,
  "/media/demo/tv/Demo Show/Season 02": 82,
  "/media/demo/tv/Another Demo Show/Season 01": 84,
  "/media/demo/movies/Big Buck Bunny (2008)": 1,
  "/media/demo/movies/Sintel (2010)": 1,
  "/media/demo/movies/Tears of Steel (2012)": 1,
};
/** Folders where the server stopped counting (so the count is "1,000+"). */
const MEDIA_COUNT_CAPPED = new Set(["/", "/media"]);

/** A folder's count as the server sends it, for an entry or the browsed folder itself. */
function mediaCount(path) {
  return MEDIA_COUNT_CAPPED.has(path)
    ? { media_count: MEDIA_COUNT[path] ?? 0, media_count_capped: true }
    : { media_count: MEDIA_COUNT[path] ?? 0 };
}

// ---------------------------------------------------------------------------
// Libraries, files, jobs
// ---------------------------------------------------------------------------

/** @type {Map<string, any>} */
const libraries = new Map();
/** @type {Map<string, any>} */
const files = new Map();
/** @type {Map<string, any>} */
const jobs = new Map();
const activity = [];
let activityId = 1;
const savingsByDay = new Map();
/** Files each library's last scan found still being copied (LibraryStats.settling). */
const settling = new Map();
let paused = false;
const scanning = new Set();

const OPEN_MOVIES = [
  "Big Buck Bunny", "Sintel", "Tears of Steel", "Elephants Dream", "Cosmos Laundromat", "Spring", "Agent 327",
  "Caminandes", "Coffee Run", "Hero", "Glass Half", "Sprite Fright", "Charge", "Wing It", "Daily Dweebs",
];

const RESOLUTIONS = {
  "4K": [3840, 2160],
  "1080p": [1920, 1080],
  "720p": [1280, 720],
  "480p": [720, 480],
};

/**
 * `extra.problem` is the cause of an entry about a failed or skipped file
 * (the same codes as `Job.problem`); every entry has the field, null when
 * there is none.
 */
function addActivity(level, message, extra = {}) {
  const entry = { id: activityId++, at: iso(Date.now()), level, message, problem: null, file_id: null, job_id: null, library_id: null, ...extra };
  activity.unshift(entry);
  if (activity.length > 500) activity.pop();
  return entry;
}

function makeProbe(file) {
  const [w, h] = RESOLUTIONS[file.resolution] ?? [1920, 1080];
  const streams = [
    {
      index: 0, kind: "video", codec: file.video_codec, profile: file.video_codec === "hevc" ? "Main 10" : "High", language: null, title: null,
      is_default: true, is_forced: false, is_attached_pic: false, bit_rate: null, width: w, height: h,
      pix_fmt: file.video_codec === "hevc" ? "yuv420p10le" : "yuv420p", bit_depth: file.video_codec === "hevc" ? 10 : 8,
      frame_rate: 23.976, color_primaries: file.hdr ? "bt2020" : "bt709", color_transfer: file.hdr ? "smpte2084" : "bt709",
      color_space: file.hdr ? "bt2020nc" : "bt709", color_range: "tv", hdr: file.hdr, interlaced: file.video_codec === "mpeg2video",
      channels: null, channel_layout: null, sample_rate: null,
      ...(file.hdr === "hdr10"
        ? {
            mastering_display: {
              red: [0.708, 0.292], green: [0.17, 0.797], blue: [0.131, 0.046], white_point: [0.3127, 0.329],
              max_luminance: 1000, min_luminance: 0.005,
            },
            content_light: { max_cll: 1000, max_fall: 400 },
          }
        : {}),
      ...(file.dvNoBaseLayer ? { dolby_vision_without_base_layer: true } : {}),
    },
    {
      index: 1, kind: "audio", codec: file.audio_codec, profile: null, language: "eng", title: "English 5.1", is_default: true,
      is_forced: false, is_attached_pic: false, bit_rate: 640000, width: null, height: null, pix_fmt: null, bit_depth: null,
      frame_rate: null, color_primaries: null, color_transfer: null, color_space: null, color_range: null, hdr: null,
      interlaced: false, channels: 6, channel_layout: "5.1(side)", sample_rate: 48000,
    },
  ];
  if (file.relative_path.includes("Sintel") || file.relative_path.includes("Demo Show")) {
    streams.push({
      index: 2, kind: "audio", codec: "aac", profile: "LC", language: "jpn", title: "Commentary", is_default: false,
      is_forced: false, is_attached_pic: false, bit_rate: 128000, width: null, height: null, pix_fmt: null, bit_depth: null,
      frame_rate: null, color_primaries: null, color_transfer: null, color_space: null, color_range: null, hdr: null,
      interlaced: false, channels: 2, channel_layout: "stereo", sample_rate: 48000,
    });
    streams.push({
      index: 3, kind: "subtitle", codec: "subrip", profile: null, language: "eng", title: null, is_default: false,
      is_forced: false, is_attached_pic: false, bit_rate: null, width: null, height: null, pix_fmt: null, bit_depth: null,
      frame_rate: null, color_primaries: null, color_transfer: null, color_space: null, color_range: null, hdr: null,
      interlaced: false, channels: null, channel_layout: null, sample_rate: null,
    });
  }
  // Tracks the Plays everywhere container can't hold (see `lossTracks`).
  if (file.lossTracks) {
    const track = (index, kind, codec, language) => ({
      index, kind, codec, profile: null, language, title: null, is_default: false, is_forced: false, is_attached_pic: false,
      bit_rate: null, width: null, height: null, pix_fmt: null, bit_depth: null, frame_rate: null, color_primaries: null,
      color_transfer: null, color_space: null, color_range: null, hdr: null, interlaced: false, channels: null,
      channel_layout: null, sample_rate: null,
    });
    if (/picture-based/.test(file.lossTracks)) {
      streams.push(track(2, "subtitle", "hdmv_pgs_subtitle", "eng"), track(3, "subtitle", "hdmv_pgs_subtitle", "jpn"));
    }
    streams.push(track(4, "attachment", /font/.test(file.lossTracks) ? "ttf" : "mjpeg", null));
  }
  return {
    container: file.container,
    format_long_name: file.container === "matroska" ? "Matroska / WebM" : "QuickTime / MOV",
    duration_secs: file.duration_secs,
    bit_rate: file.bit_rate,
    size_bytes: file.size_bytes,
    start_time: 0,
    chapters: file.relative_path.includes("Demo Show") ? 4 : 12,
    streams,
  };
}

function validationReport(ssimMin = between(0.955, 0.985)) {
  const avg = Math.min(0.999, ssimMin + between(0.005, 0.015));
  return {
    passed: true,
    level: settings.validation === "off" ? "quick" : settings.validation,
    checks: [
      { id: "probe", label: "Opens correctly", status: "pass", detail: "The new file is a valid Matroska file.", value: null },
      { id: "streams", label: "Every track is there", status: "pass", detail: "1 video, 2 audio and 1 subtitle track, as expected.", value: null },
      { id: "duration", label: "Same length", status: "pass", detail: "1:42:10 in both files.", value: 0.02 },
      { id: "decode", label: "Plays start to finish", status: "pass", detail: "Decoded the whole file without a single error.", value: null },
      {
        id: "visual",
        label: "Looks the same as the original",
        status: "pass",
        detail: "Compared 4 moments; the least similar scored " + ssimMin.toFixed(3) + " (0.90 needed).",
        value: Number(ssimMin.toFixed(4)),
      },
    ],
    ssim_min: Number(ssimMin.toFixed(4)),
    ssim_avg: Number(avg.toFixed(4)),
    psnr_avg: Number(between(40, 46).toFixed(2)),
    elapsed_secs: Math.round(between(35, 140)),
  };
}

function commandFor(file, encoder) {
  const out = file.path.replace(/\.[^.]+$/, ".mkv").replace(/([^/]+)$/, ".$1.chrysopoeia-1a2b3c4d.tmp.mkv");
  const hwIn = encoder.endsWith("_nvenc") ? "-hwaccel cuda -hwaccel_output_format cuda " : "";
  return `ffmpeg -hide_banner -nostdin -y ${hwIn}-analyzeduration 100M -probesize 100M -i "${file.path}" -map 0:0 -map 0:1 -map 0:2? -c:v ${encoder} ${
    encoder.includes("nvenc") ? "-preset p5 -cq 28" : "-preset 6 -crf 30"
  } -pix_fmt yuv420p10le -c:a copy -c:s copy -map_metadata 0 -map_chapters 0 -max_muxing_queue_size 9999 -f matroska "${out}"`;
}

function encoderFor(library) {
  const codec = library.profile.video_codec;
  const hw = settings.hardware !== "cpu" && hardware.encoders.find((e) => e.codec === codec && e.verified && e.api !== "software");
  if (hw) return { encoder: hw.name, hw_api: hw.api };
  const sw = hardware.encoders.find((e) => e.codec === codec && e.api === "software");
  return { encoder: sw?.name ?? "libx265", hw_api: "software" };
}

function makeJob(file, state, extra = {}) {
  const library = libraries.get(file.library_id);
  const { encoder, hw_api } = encoderFor(library);
  const job = {
    id: uuid(),
    file_id: file.id,
    library_id: file.library_id,
    file_name: file.file_name,
    file_path: file.path,
    state,
    stage: state === "running" ? "transcoding" : "waiting",
    priority: 0,
    progress: 0,
    fps: null,
    speed: null,
    eta_secs: null,
    encoder: state === "queued" ? null : encoder,
    hw_api: state === "queued" ? null : hw_api,
    attempt: 1,
    input_size: file.original_size_bytes ?? file.size_bytes,
    output_size: null,
    freed_bytes: null,
    output_name: null,
    error: null,
    problem: null,
    skip_reason: null,
    validation: null,
    command: state === "queued" ? null : commandFor(file, encoder),
    log_tail: null,
    notes: [],
    force: false,
    created_at: ago(between(600, 86400)),
    started_at: null,
    finished_at: null,
    ...extra,
  };
  jobs.set(job.id, job);
  file.job_id = job.id;
  return job;
}

function makeFile(library, relative, opts) {
  const fileName = relative.split("/").pop();
  const size = Math.round(opts.size);
  const file = {
    id: uuid(),
    library_id: library.id,
    path: `${library.path}/${relative}`,
    relative_path: relative,
    file_name: fileName,
    size_bytes: size,
    modified_at: ago(between(86400 * 20, 86400 * 900)),
    status: "pending",
    container: fileName.endsWith(".mp4") ? "mov" : fileName.endsWith(".ts") ? "mpegts" : fileName.endsWith(".avi") ? "avi" : "matroska",
    video_codec: opts.codec,
    audio_codec: opts.audio ?? "ac3",
    resolution: opts.resolution,
    hdr: opts.hdr ?? null,
    duration_secs: opts.duration,
    bit_rate: Math.round((size * 8) / opts.duration),
    original_size_bytes: null,
    saved_bytes: null,
    skip_reason: null,
    error: null,
    problem: null,
    job_id: null,
    progress: null,
    scanned_at: ago(between(600, 7200)),
    updated_at: ago(between(600, 86400 * 3)),
  };
  files.set(file.id, file);
  return file;
}

/**
 * Plain-language compromises a conversion made (the worker's
 * `JobOutcome::Done.notes`), for a stable subset of files.
 */
function notesFor(file) {
  const library = libraries.get(file.library_id);
  // Converted anyway: the tracks the container can't hold are left out.
  if (file.lossTracks) return [`Removed ${file.lossTracks} because ${library?.profile.container === "webm" ? "WebM" : "MP4"} can't hold them`];
  if (file.hardLinked) return [HARD_LINK_NOTE];
  if (file.file_name.length % 4 !== 0) return [];
  if (library?.profile.container === "mp4") {
    return ["Removed 2 picture-based subtitles because MP4 can't hold them"];
  }
  return ["Converted the MOV text subtitle to SRT so MKV can hold it"];
}

/**
 * A converted file takes its goal's extension (`land1080.mp4` becomes
 * `land1080.mkv`): the job records the result's name (`output_name`, null
 * while the name stays) and, when originals are replaced, the file itself is
 * renamed. The job keeps the input's name.
 */
function renameToGoal(file, job) {
  const container = libraries.get(file.library_id)?.profile.container ?? "mkv";
  const next = file.file_name.replace(/\.[^.]+$/, `.${container}`);
  if (next === file.file_name) return;
  job.output_name = next;
  if (settings.output_mode !== "replace") return;
  const swap = (path) => path.slice(0, path.length - file.file_name.length) + next;
  file.path = swap(file.path);
  file.relative_path = swap(file.relative_path);
  file.file_name = next;
  file.container = container === "mp4" ? "mov" : "matroska";
}

function markDone(file, ratio, finishedSecsAgo) {
  const original = file.size_bytes;
  const out = Math.round(original * ratio);
  file.original_size_bytes = original;
  file.size_bytes = out;
  file.saved_bytes = original - out;
  file.status = "done";
  file.video_codec = libraries.get(file.library_id).profile.video_codec;
  const finished = NOW - finishedSecsAgo * 1000;
  const job = makeJob(file, "done", {
    stage: "finalizing",
    progress: 100,
    input_size: original,
    output_size: out,
    freed_bytes: original - out,
    validation: validationReport(),
    notes: notesFor(file),
    started_at: iso(finished - between(900, 3600) * 1000),
    finished_at: iso(finished),
  });
  job.created_at = iso(finished - 7200 * 1000);
  renameToGoal(file, job);
  file.updated_at = job.finished_at;
  const day = job.finished_at.slice(0, 10);
  const entry = savingsByDay.get(day) ?? { saved_bytes: 0, files: 0 };
  entry.saved_bytes += original - out;
  entry.files += 1;
  savingsByDay.set(day, entry);
  return job;
}

function createLibrary(name, path, goal) {
  const library = {
    id: uuid(),
    name,
    path,
    enabled: true,
    profile: profileForGoal(goal),
    stats: null,
    scanning: false,
    last_scan_at: ago(3600),
    path_error: null,
    created_at: ago(86400 * 30),
  };
  libraries.set(library.id, library);
  return library;
}

function populateMovies(library, count, doneShare) {
  for (let i = 0; i < count; i++) {
    const title = i < OPEN_MOVIES.length ? OPEN_MOVIES[i] : `Demo Movie ${String(i + 1).padStart(3, "0")}`;
    const year = 2006 + ((i * 7) % 18);
    const variant = "";
    const res = rand() < 0.18 ? "4K" : rand() < 0.8 ? "1080p" : "720p";
    const codec = res === "4K" ? (rand() < 0.6 ? "hevc" : "h264") : rand() < 0.07 ? "mpeg2video" : rand() < 0.1 ? "vc1" : "h264";
    const ext = codec === "mpeg2video" ? "ts" : rand() < 0.25 ? "mp4" : "mkv";
    const duration = between(80, 150) * 60;
    const size = res === "4K" ? between(18e9, 58e9) : res === "1080p" ? between(4e9, 16e9) : between(1.2e9, 4e9);
    const file = makeFile(library, `${title} (${year})${variant}/${title} (${year})${variant}.${ext}`, {
      size,
      codec,
      resolution: res,
      duration,
      hdr: res === "4K" && rand() < 0.5 ? "hdr10" : null,
      audio: pick(["ac3", "dts", "eac3", "truehd", "aac"]),
    });
    const roll = rand();
    if (roll < doneShare) markDone(file, between(0.38, 0.62), between(60, 86400 * 29));
    else if (roll < doneShare + 0.08) {
      file.status = "skipped";
      file.skip_reason = codec === "hevc" && library.profile.video_codec === "hevc" ? "Already HEVC" : "Only 4% smaller — kept the original";
    }
  }
}

function populateTv(library, shows) {
  for (const [show, seasons, eps] of shows) {
    for (let s = 1; s <= seasons; s++) {
      for (let e = 1; e <= eps; e++) {
        const res = rand() < 0.55 ? "1080p" : "720p";
        const file = makeFile(
          library,
          `${show}/Season ${String(s).padStart(2, "0")}/${show} - S${String(s).padStart(2, "0")}E${String(e).padStart(2, "0")}.mkv`,
          {
            size: res === "1080p" ? between(1.4e9, 3.2e9) : between(0.6e9, 1.4e9),
            codec: rand() < 0.1 ? "hevc" : "h264",
            resolution: res,
            duration: between(21, 58) * 60,
            audio: "aac",
          },
        );
        const roll = rand();
        if (s === 1 && roll < 0.85) markDone(file, between(0.32, 0.55), between(60, 86400 * 25));
        else if (roll < 0.1) {
          file.status = "skipped";
          file.skip_reason = file.video_codec === "hevc" ? "Already efficient (HEVC), nothing to gain" : "Only 6% smaller — kept the original";
        }
      }
    }
  }
}

function seedDemo() {
  const movies = createLibrary("Demo Movies", "/media/demo/movies", "balanced");
  const tv = createLibrary("Demo TV", "/media/demo/tv", "save_space");
  const home = createLibrary("Demo Home Videos", "/media/demo/home-videos", "compatible");
  home.enabled = false;
  populateMovies(movies, 136, 0.45);
  populateTv(tv, [
    ["Demo Show", 2, 82],
    ["Another Demo Show", 1, 84],
  ]);
  for (let i = 1; i <= 28; i++) {
    makeFile(home, `2019/Holiday clip ${String(i).padStart(2, "0")}.mp4`, {
      size: between(0.2e9, 1.1e9), codec: "h264", resolution: "1080p", duration: between(1, 9) * 60, audio: "aac",
    });
  }

  // Failures with real-looking reasons.
  const pendingMovies = [...files.values()].filter((f) => f.library_id === movies.id && f.status === "pending");
  const failedA = pendingMovies[0];
  failedA.status = "failed";
  failedA.error = "The new file didn't match the original: 1 of 4 checked moments looked different (similarity 0.61, 0.90 needed). The original was kept.";
  failedA.problem = "verification";
  const jobA = makeJob(failedA, "failed", {
    stage: "verifying",
    progress: 100,
    input_size: failedA.size_bytes,
    output_size: Math.round(failedA.size_bytes * 0.44),
    error: failedA.error,
    problem: failedA.problem,
    validation: {
      passed: false,
      level: "standard",
      checks: [
        { id: "probe", label: "Opens correctly", status: "pass", detail: "The new file is a valid Matroska file.", value: null },
        { id: "streams", label: "Every track is there", status: "pass", detail: "1 video and 1 audio track, as expected.", value: null },
        { id: "duration", label: "Same length", status: "pass", detail: "1:58:02 in both files.", value: 0.04 },
        { id: "decode", label: "Plays start to finish", status: "pass", detail: "Decoded the whole file without errors.", value: null },
        { id: "visual", label: "Looks the same as the original", status: "fail", detail: "At 1:12:40 the picture differs a lot (similarity 0.61). This usually means corrupted frames in the source.", value: 0.6123 },
      ],
      ssim_min: 0.6123,
      ssim_avg: 0.9012,
      psnr_avg: 31.4,
      elapsed_secs: 96,
    },
    log_tail: "[h264 @ 0x55d0c] error while decoding MB 61 33, bytestream -7\n[h264 @ 0x55d0c] concealing 1204 DC, 1204 AC, 1204 MV errors in P frame\n[vist#0:0/h264 @ 0x55d0e] corrupt decoded frame",
    started_at: ago(5400),
    finished_at: ago(3100),
  });
  jobA.created_at = ago(9000);
  // A damaged original: the worker's own sentence for a file that stops early.
  const failedB = pendingMovies[1];
  failedB.status = "failed";
  failedB.error = "The original file appears damaged or incomplete (it stops after 0.1 s). It was left unchanged.";
  failedB.problem = "unreadable_source";
  makeJob(failedB, "failed", {
    stage: "transcoding",
    progress: 63,
    error: failedB.error,
    problem: failedB.problem,
    log_tail:
      "Svt[info]: -------------------------------------------\nSvt[info]: SVT [version]:\tSVT-AV1 Encoder Lib v2.1.0\nSvt[info]: -------------------------------------------\n[matroska,webm @ 0x5581a2c0] Read error at pos. 4829122560 (0x11fd84000)\n[in#0/matroska @ 0x5580] Error during demuxing: I/O error\nConversion failed!",
    started_at: ago(12000),
    finished_at: ago(10000),
  });
  // Not a video at all: ffprobe couldn't read it when the folder was scanned.
  const fake = makeFile(movies, "Extras/Fake.mp4", { size: 19, codec: null, resolution: null, duration: 1, audio: null });
  Object.assign(fake, {
    status: "failed",
    unreadable: true,
    container: null,
    duration_secs: null,
    bit_rate: null,
    error: "This file can't be read as a video: it has no MP4 index, so it's incomplete or not really a video.",
    problem: "unreadable_source",
  });

  // The work folder can't be written: one setup problem, in two libraries.
  const noWorkFolder = [pendingMovies[4], [...files.values()].find((f) => f.library_id === tv.id && f.status === "pending" && f.video_codec === "h264")];
  for (const f of noWorkFolder.filter(Boolean)) {
    f.status = "failed";
    f.error = WORK_FOLDER_ERROR;
    f.problem = "work_folder";
    makeJob(f, "failed", { stage: "preparing", progress: 0, error: f.error, problem: f.problem, started_at: ago(2600), finished_at: ago(2590) });
  }
  // A season folder the container user can't write to, and a name clash:
  // both "destination", each with its own cause and fix.
  const tvPending = [...files.values()].filter((f) => f.library_id === tv.id && f.status === "pending" && f.video_codec === "h264");
  const lockedEpisode = tvPending.find((f) => f.relative_path.startsWith("Demo Show/Season 02/"));
  if (lockedEpisode) {
    const folder = lockedEpisode.path.slice(0, lockedEpisode.path.lastIndexOf("/"));
    lockedEpisode.status = "failed";
    lockedEpisode.error = `Chrysopoeia doesn't have permission to write in ${folder}, so the new file couldn't be put there and the original was kept. Check the folder's permissions (in Docker, the PUID/PGID user needs write access).`;
    lockedEpisode.problem = "destination";
    makeJob(lockedEpisode, "failed", { stage: "finalizing", progress: 60, error: lockedEpisode.error, problem: "destination", started_at: ago(3300), finished_at: ago(3000) });
  }
  const clash = pendingMovies.slice(10).find((f) => f.file_name.endsWith(".mp4") && f.status === "pending");
  if (clash) {
    const taken = clash.file_name.replace(/\.mp4$/, ".mkv");
    clash.status = "failed";
    clash.error = `A file named "${taken}" is already next to the original, so the new file can't take its name. Move or rename that file, then try again.`;
    clash.problem = "destination";
    makeJob(clash, "failed", { stage: "finalizing", progress: 90, error: clash.error, problem: "destination", started_at: ago(3500), finished_at: ago(3400) });
  }
  // Moved away while it was being converted: the server's `source_changed`.
  const moved = pendingMovies[5];
  moved.status = "failed";
  moved.error = "The file is no longer there. It was moved or deleted while it was being converted.";
  moved.problem = "source_changed";
  makeJob(moved, "failed", { stage: "finalizing", progress: 40, error: moved.error, problem: moved.problem, started_at: ago(4200), finished_at: ago(3900) });
  // Replaced by a new copy meanwhile: a skip on the server, converted again later.
  const replaced = pendingMovies[6];
  if (replaced) {
    makeJob(replaced, "skipped", {
      stage: "finalizing",
      progress: 100,
      output_size: Math.round(replaced.size_bytes * 0.5),
      skip_reason: "The original changed while it was being converted, so it was left alone",
      started_at: ago(8200),
      finished_at: ago(8000),
    });
    replaced.skip_reason = null;
  }

  // Failed once on the work folder, converted since: the old failure is history.
  const recovered = [...files.values()].find(
    (f) => f.library_id === tv.id && f.status === "done" && Date.parse(jobs.get(f.job_id)?.created_at ?? "") < Date.now() - 3 * 3600_000,
  );
  if (recovered) {
    const conversion = jobs.get(recovered.job_id);
    const failedAt = Date.parse(conversion.created_at) - 3600_000;
    const old = makeJob(recovered, "failed", {
      stage: "preparing",
      progress: 0,
      error: WORK_FOLDER_ERROR,
      problem: "work_folder",
      started_at: iso(failedAt + 60_000),
      finished_at: iso(failedAt + 65_000),
    });
    old.created_at = iso(failedAt);
    recovered.job_id = conversion.id;
  }

  // A converted file converted again after a goal change that the work
  // folder stopped: it stays converted, and the Overview says the fix.
  const stoppedAgain = [...files.values()].find(
    (f) => f.library_id === tv.id && f.status === "done" && f !== recovered && Date.parse(jobs.get(f.job_id)?.finished_at ?? "") < Date.now() - 86400_000,
  );
  if (stoppedAgain) {
    const conversion = stoppedAgain.job_id;
    makeJob(stoppedAgain, "failed", {
      stage: "preparing",
      progress: 0,
      input_size: stoppedAgain.size_bytes,
      error: WORK_FOLDER_ERROR,
      problem: "work_folder",
      created_at: ago(500),
      started_at: ago(480),
      finished_at: ago(470),
    });
    stoppedAgain.job_id = conversion;
  }

  // A converted file converted again after a goal change, whose new result
  // wasn't smaller enough: it stays converted ("Kept as converted").
  const again = [...files.values()].find(
    (f) => f.library_id === movies.id && f.status === "done" && Date.parse(jobs.get(f.job_id)?.finished_at ?? "") < Date.now() - 86400_000,
  );
  if (again) {
    // The file keeps pointing at the conversion that made it (like the server).
    const conversion = again.job_id;
    makeJob(again, "skipped", {
      stage: "transcoding",
      progress: 100,
      input_size: again.size_bytes,
      output_size: Math.round(again.size_bytes * 0.97),
      skip_reason: "Only 3% smaller — kept the original",
      created_at: ago(1300),
      started_at: ago(1200),
      finished_at: ago(700),
    });
    again.job_id = conversion;
  }

  // Dolby Vision profile 5: always left unchanged.
  const dv = makeFile(movies, "Demo Dolby Vision (2021)/Demo Dolby Vision (2021).mkv", {
    size: 21e9, codec: "hevc", resolution: "4K", duration: 7200, hdr: "dolby_vision", audio: "eac3",
  });
  Object.assign(dv, {
    status: "skipped",
    dvNoBaseLayer: true,
    skip_reason: "Dolby Vision profile 5 can't be converted without losing its colours — left unchanged",
  });

  // A cancelled and a skipped job in history.
  const cancelled = pendingMovies[2];
  makeJob(cancelled, "cancelled", { stage: "transcoding", progress: 31, started_at: ago(20000), finished_at: ago(19000) });
  const skippedFile = pendingMovies[3];
  skippedFile.status = "skipped";
  skippedFile.skip_reason = "Only 4% smaller — kept the original";
  makeJob(skippedFile, "skipped", {
    stage: "transcoding",
    progress: 100,
    output_size: Math.round(skippedFile.size_bytes * 0.96),
    skip_reason: skippedFile.skip_reason,
    started_at: ago(1500),
    finished_at: ago(900),
  });

  // Running and queued.
  const candidates = [...files.values()].filter((f) => f.status === "pending" && f.library_id !== home.id);
  const running = [candidates[5], candidates[40], candidates[160]].filter(Boolean);
  const stages = [
    { stage: "transcoding", progress: 42.4 },
    { stage: "transcoding", progress: 77.1 },
    { stage: "verifying", progress: 35 },
  ];
  running.forEach((f, i) => {
    f.status = "processing";
    f.progress = stages[i].progress;
    const job = makeJob(f, "running", { ...stages[i], started_at: ago(between(300, 2400)), fps: between(90, 240), speed: between(3.5, 9.5) });
    job.eta_secs = Math.round(between(240, 1800));
  });
  const queued = candidates.filter((f) => f.status === "pending").slice(8, 38);
  queued.forEach((f, i) => {
    f.status = "queued";
    const job = makeJob(f, "queued");
    job.created_at = ago(4000 - i * 30);
  });

  // A damaged episode in the TV library too: grouped problems span libraries.
  const brokenEpisode = [...files.values()].find((f) => f.library_id === tv.id && f.status === "pending");
  if (brokenEpisode) {
    brokenEpisode.status = "failed";
    brokenEpisode.error = "The original file appears damaged or incomplete (it stops after 2.4 s). It was left unchanged.";
    brokenEpisode.problem = "unreadable_source";
    makeJob(brokenEpisode, "failed", {
      stage: "transcoding",
      progress: 4,
      error: brokenEpisode.error,
      problem: brokenEpisode.problem,
      started_at: ago(7000),
      finished_at: ago(6900),
    });
  }

  // Round 5. A hard-linked file converted anyway: the new file is smaller but
  // its original's space wasn't released (`freed_bytes` 0), and its name
  // changed with its format (`output_name`).
  const clip = makeFile(movies, "Clips/land1080.mp4", { size: 718_000, codec: "h264", resolution: "1080p", duration: 12, audio: "aac" });
  clip.hardLinked = true;
  makeJob(clip, "skipped", {
    stage: "preparing",
    skip_reason: HARD_LINK_SKIP,
    created_at: ago(7400),
    started_at: ago(7300),
    finished_at: ago(7290),
  });
  Object.assign(clip, { original_size_bytes: 718_000, size_bytes: 402_000, saved_bytes: 0, status: "done", video_codec: "hevc" });
  const forcedClip = makeJob(clip, "done", {
    force: true,
    stage: "finalizing",
    progress: 100,
    input_size: 718_000,
    output_size: 402_000,
    freed_bytes: 0,
    validation: validationReport(),
    notes: [HARD_LINK_NOTE],
    created_at: ago(900),
    started_at: ago(880),
    finished_at: ago(600),
  });
  renameToGoal(clip, forcedClip);
  clip.updated_at = forcedClip.finished_at;

  // Plays everywhere replaces originals as MP4: files holding tracks MP4 can't keep are left unchanged.
  const anime = createLibrary("Demo Anime", "/media/demo/anime", "compatible");
  for (const [name, lost] of [
    ["Demo Anime - S01E01.mkv", "2 picture-based subtitles and 1 subtitle font"],
    ["Demo Anime - S01E02.mkv", "1 attached file"],
  ]) {
    const episode = makeFile(anime, `Demo Anime/${name}`, { size: between(1.2e9, 1.5e9), codec: "h264", resolution: "1080p", duration: 24 * 60, audio: "aac" });
    episode.lossTracks = lost;
    episode.status = "skipped";
    episode.skip_reason = lossSkip("MP4", lost);
    makeJob(episode, "skipped", { stage: "preparing", skip_reason: episode.skip_reason, created_at: ago(5000), started_at: ago(4990), finished_at: ago(4980) });
    addActivity("warning", `Left ${name} unchanged: MP4 can't hold its ${lost}.`, { library_id: anime.id, file_id: episode.id });
  }
  makeFile(anime, "Demo Anime/Demo Anime - S01E03.mkv", { size: between(1.2e9, 1.5e9), codec: "h264", resolution: "1080p", duration: 24 * 60, audio: "aac" });
  // Scene-release names have no space to break at: a dialog title or a list row must still fit a phone.
  const releaseLoss = makeFile(anime, "Releases/Some.Really.Long.Scene.Release.Name.2021.1080p.BluRay.x264.DTS-HD.MA.5.1.REMUX-GROUP.mkv", {
    size: 9.4e9, codec: "h264", resolution: "1080p", duration: 7200, audio: "dts",
  });
  releaseLoss.lossTracks = "3 picture-based subtitles";
  releaseLoss.status = "skipped";
  releaseLoss.skip_reason = lossSkip("MP4", releaseLoss.lossTracks);
  makeJob(releaseLoss, "skipped", { stage: "preparing", skip_reason: releaseLoss.skip_reason, created_at: ago(4800), started_at: ago(4790), finished_at: ago(4780) });
  // Converted under an older goal (HEVC): "Convert again" is offered for it.
  const releaseDone = makeFile(anime, "Releases/Another.Really.Long.Scene.Release.Name.2021.2160p.UHD.BluRay.x265.DTS-HD.MA.7.1.REMUX-GROUP.mkv", {
    size: 31e9, codec: "hevc", resolution: "4K", duration: 7800, audio: "truehd",
  });
  markDone(releaseDone, 0.48, 3 * 3600);
  releaseDone.video_codec = "hevc";

  settling.set(tv.id, 3);
  addActivity("info", "Scanned Demo TV: 250 files, 3 still being copied (checked again when they're finished)", { library_id: tv.id });
  addActivity("info", "Scanned Demo Movies: 136 files, 2 new.", { library_id: movies.id });
  addActivity("success", "Converted Sintel (2010).mkv and saved 4.1 GB.", { library_id: movies.id });
  addActivity("warning", `Kept the original of ${skippedFile.file_name}: only 4% smaller.`, { library_id: movies.id });
  addActivity("error", `${failedA.file_name} failed its visual check. The original was kept.`, { library_id: movies.id, file_id: failedA.id, problem: "verification" });
  // The real server's wording for a failed job ("Failed <name>: <error>"), here a damaged original.
  // The server sends the cause with it, so the Log doesn't have to read the sentence.
  addActivity("error", `Failed ${failedB.file_name}: ${failedB.error}`, { library_id: movies.id, file_id: failedB.id, problem: "unreadable_source" });
  addActivity("success", "Converted Demo Show - S01E04.mkv and saved 1.1 GB.", { library_id: tv.id });
  activity.forEach((e, i) => (e.at = ago(600 + i * 1900)));
}

/** Many files that all failed on the work folder: more than one page of failed files. */
function seedBulkFailed(count) {
  const bulk = createLibrary("Demo Bulk", "/media/demo/bulk", "balanced");
  for (let i = 1; i <= count; i++) {
    const file = makeFile(bulk, `clip${String(i).padStart(4, "0")}.mp4`, {
      size: between(2e6, 30e6), codec: "h264", resolution: "1080p", duration: between(5, 60), audio: "aac",
    });
    Object.assign(file, { status: "failed", error: WORK_FOLDER_ERROR, problem: "work_folder" });
    makeJob(file, "failed", { stage: "preparing", progress: 0, error: file.error, problem: "work_folder", started_at: ago(120), finished_at: ago(119) });
  }
}

if (SCENARIO === "demo" || SCENARIO === "nogpu") seedDemo();
if (BULK_FAILED > 0) seedBulkFailed(BULK_FAILED);

// ---------------------------------------------------------------------------
// Derived views
// ---------------------------------------------------------------------------

function statsFor(list, libraryId = null) {
  const s = {
    file_count: 0, total_bytes: 0, pending: 0, queued: 0, processing: 0, done: 0, skipped: 0, failed: 0, saved_bytes: 0,
    settling: libraryId ? (settling.get(libraryId) ?? 0) : [...settling.values()].reduce((a, b) => a + b, 0),
  };
  for (const f of list) {
    s.file_count += 1;
    s.total_bytes += f.size_bytes;
    s[f.status] += 1;
    // Like the server: every file's savings, so a converted file queued
    // again keeps counting.
    if (f.saved_bytes) s.saved_bytes += f.saved_bytes;
  }
  return s;
}

function libraryView(library) {
  const own = [...files.values()].filter((f) => f.library_id === library.id);
  return { ...library, stats: statsFor(own, library.id), scanning: scanning.has(library.id) };
}

function maxJobs() {
  return settings.max_jobs ?? ENV_MAX_JOBS ?? hardware.recommended_jobs.total;
}

function maxJobsSource() {
  return settings.max_jobs !== null ? "settings" : ENV_MAX_JOBS ? "env" : "auto";
}

function inActiveHours() {
  if (!settings.active_hours) return true;
  const { start, end } = settings.active_hours;
  const h = new Date().getHours();
  if (start === end) return true;
  return start < end ? h >= start && h < end : h >= start || h < end;
}

function queueState() {
  const all = [...jobs.values()];
  const queued = all.filter((j) => j.state === "queued").length;
  return {
    paused,
    running: all.filter((j) => j.state === "running").length,
    queued,
    max_jobs: maxJobs(),
    max_jobs_auto: settings.max_jobs === null && !ENV_MAX_JOBS,
    max_jobs_source: maxJobsSource(),
    waiting_for_schedule: !paused && queued > 0 && !inActiveHours(),
  };
}

function counts(key) {
  const map = new Map();
  for (const f of files.values()) {
    const name = f[key] ?? "unknown";
    const c = map.get(name) ?? { name, files: 0, bytes: 0 };
    c.files += 1;
    c.bytes += f.size_bytes;
    map.set(name, c);
  }
  return [...map.values()].sort((a, b) => b.files - a.files);
}

function overview() {
  const totals = statsFor([...files.values()]);
  const history = [];
  for (let i = 29; i >= 0; i--) {
    const date = new Date(Date.now() - i * 86400 * 1000).toISOString().slice(0, 10);
    const entry = savingsByDay.get(date) ?? { saved_bytes: 0, files: 0 };
    history.push({ date, saved_bytes: entry.saved_bytes, files: entry.files });
  }
  const done = [...files.values()].filter((f) => f.status === "done" && f.original_size_bytes);
  let projected = null;
  if (done.length >= 5) {
    const ratio = done.reduce((s, f) => s + f.saved_bytes, 0) / done.reduce((s, f) => s + f.original_size_bytes, 0);
    const remaining = [...files.values()].filter((f) => ["pending", "queued", "processing"].includes(f.status));
    projected = Math.round(remaining.reduce((s, f) => s + f.size_bytes, 0) * ratio);
  }
  return {
    totals,
    video_codecs: counts("video_codec"),
    audio_codecs: counts("audio_codec"),
    resolutions: counts("resolution"),
    savings_history: history,
    projected_savings_bytes: projected,
    queue: queueState(),
  };
}

/** A file as the API sends it (mock-only bookkeeping removed). */
function listFile(f) {
  const out = { ...f };
  delete out.probe;
  delete out.unreadable;
  delete out.dvNoBaseLayer;
  delete out.wasDone;
  delete out.hardLinked;
  delete out.lossTracks;
  return out;
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

const sockets = new Set();
function broadcast(event) {
  const data = JSON.stringify(event);
  for (const ws of sockets) if (ws.readyState === 1) ws.send(data);
}
const emitQueue = () => broadcast({ type: "queue.state", ...queueState() });
const emitStats = () => broadcast({ type: "stats.updated", totals: statsFor([...files.values()]) });
const emitLibrary = (lib) => broadcast({ type: "library.updated", library: libraryView(lib) });
const emitActivity = (entry) => broadcast({ type: "activity", entry });

/**
 * Queue a file. A converted file queued again ("Convert again") is read as
 * it is now, and stays converted, with its savings, unless the new job
 * gives a new result (round 4).
 */
function queueFile(file, priority = 0, force = false) {
  const again = file.status === "done";
  const job = makeJob(file, "queued", {
    priority,
    force,
    created_at: iso(Date.now()),
    ...(again ? { input_size: file.size_bytes } : {}),
  });
  if (again) file.wasDone = true;
  if (force) forcedJobs.add(job.id);
  Object.assign(file, { status: "queued", error: null, problem: null, updated_at: iso(Date.now()) });
  return job;
}

/** A job that ended without a new result: a converted file stays converted, anything else takes `status`. */
function settleFile(file, status, extra = {}) {
  const now = iso(Date.now());
  if (file.wasDone) {
    delete file.wasDone;
    Object.assign(file, { status: "done", progress: null, updated_at: now });
  } else Object.assign(file, { status, progress: null, updated_at: now, ...extra });
}

const EFFICIENCY = { av1: 4, hevc: 3, h265: 3, vp9: 3, h264: 2, vp8: 2 };
const TARGET_NAME = { av1: "AV1", hevc: "HEVC", h264: "H.264", vp9: "VP9" };

/**
 * The worker's own check when a job starts (`decide` in the worker): files
 * the library's settings leave alone are skipped straight away, even when
 * they were queued by hand. Returns the reason, or null to convert.
 */
function skipReasonFor(file, lib) {
  if (!file.video_codec) return "Audio-only file — nothing to convert";
  const profile = lib.profile;
  // Originals are replaced: a shared original, or tracks the new container can't hold, are left alone.
  if (settings.output_mode === "replace") {
    if (file.hardLinked) return HARD_LINK_SKIP;
    if (file.lossTracks && profile.container !== "mkv") return lossSkip(profile.container === "webm" ? "WebM" : "MP4", file.lossTracks);
  }
  const source = EFFICIENCY[file.video_codec] ?? 1;
  const target = EFFICIENCY[profile.video_codec] ?? 3;
  if (profile.skip_efficient && source >= target) {
    const name = TARGET_NAME[file.video_codec] ?? file.video_codec.toUpperCase();
    if (file.video_codec === profile.video_codec) return `Already ${name}`;
    return `Already ${name}, which is ${source > target ? "more" : "as"} efficient ${source > target ? "than" : "as"} ${TARGET_NAME[profile.video_codec]}`;
  }
  return null;
}

function skipAtStart(job, file, reason) {
  const now = iso(Date.now());
  Object.assign(job, { state: "skipped", stage: "preparing", progress: 0, skip_reason: reason, started_at: now, finished_at: now });
  settleFile(file, "skipped", { skip_reason: reason });
  broadcast({ type: "job.updated", job });
  broadcast({ type: "file.updated", file: listFile(file) });
}

function startJobs() {
  if (paused || !inActiveHours()) return;
  const running = [...jobs.values()].filter((j) => j.state === "running").length;
  let slots = maxJobs() - running;
  if (slots <= 0) return;
  const next = [...jobs.values()]
    .filter((j) => j.state === "queued")
    .sort((a, b) => b.priority - a.priority || a.created_at.localeCompare(b.created_at));
  for (const job of next) {
    if (slots <= 0) break;
    const file = files.get(job.file_id);
    const lib = libraries.get(job.library_id);
    if (!file || !lib) continue;
    // "Convert anyway" sets the skip rules aside.
    const reason = forcedJobs.has(job.id) ? null : skipReasonFor(file, lib);
    if (reason) {
      skipAtStart(job, file, reason);
      emitLibrary(lib);
      emitStats();
      continue;
    }
    const { encoder, hw_api } = encoderFor(lib);
    Object.assign(job, { state: "running", stage: "preparing", progress: 0, encoder, hw_api, started_at: iso(Date.now()), command: commandFor(file, encoder) });
    file.status = "processing";
    file.progress = 0;
    slots -= 1;
    broadcast({ type: "job.updated", job });
    broadcast({ type: "file.updated", file: listFile(file) });
  }
  emitQueue();
}

function finishJob(job) {
  const file = files.get(job.file_id);
  const lib = libraries.get(job.library_id);
  const ratio = between(0.35, 0.6);
  const out = Math.round(job.input_size * ratio);
  // A hard-linked original's old data stays on disk through its other link: nothing is freed.
  const shared = Boolean(file?.hardLinked);
  Object.assign(job, {
    state: "done",
    stage: "finalizing",
    progress: 100,
    fps: null,
    speed: null,
    eta_secs: null,
    output_size: out,
    freed_bytes: shared ? 0 : job.input_size - out,
    validation: validationReport(),
    notes: file ? notesFor(file) : [],
    finished_at: iso(Date.now()),
  });
  if (file) {
    // A second conversion saves against the first original, not the converted file.
    file.original_size_bytes = file.wasDone ? (file.original_size_bytes ?? job.input_size) : job.input_size;
    delete file.wasDone;
    file.size_bytes = out;
    file.saved_bytes = shared ? 0 : file.original_size_bytes - out;
    file.status = "done";
    file.progress = null;
    file.video_codec = lib?.profile.video_codec ?? file.video_codec;
    // The tracks the container can't hold are gone from the new file.
    file.lossTracks = null;
    renameToGoal(file, job);
    file.updated_at = job.finished_at;
    if (!shared) {
      const day = job.finished_at.slice(0, 10);
      const entry = savingsByDay.get(day) ?? { saved_bytes: 0, files: 0 };
      entry.saved_bytes += file.saved_bytes;
      entry.files += 1;
      savingsByDay.set(day, entry);
    }
    broadcast({ type: "file.updated", file: listFile(file) });
  }
  broadcast({ type: "job.updated", job });
  const freedText = (bytes) => (bytes / 1e9 >= 1 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.round(bytes / 1e6)} MB`);
  emitActivity(
    addActivity("success", shared ? `Converted ${job.file_name}. ${HARD_LINK_NOTE}.` : `Converted ${job.file_name} and saved ${freedText(job.input_size - out)}.`, {
      file_id: job.file_id,
      job_id: job.id,
      library_id: job.library_id,
    }),
  );
  if (lib) emitLibrary(lib);
  emitStats();
}

function tick() {
  for (const job of jobs.values()) {
    if (job.state !== "running") continue;
    const speed = job.hw_api === "software" ? between(0.7, 1.6) : between(2.2, 4.4);
    if (job.stage === "preparing") {
      job.progress = Math.min(100, job.progress + 50);
      if (job.progress >= 100) Object.assign(job, { stage: "transcoding", progress: 0 });
    } else if (job.stage === "transcoding") {
      job.progress = Math.min(100, job.progress + speed);
      job.fps = job.hw_api === "software" ? between(24, 60) : between(140, 260);
      job.speed = job.fps / 24;
      job.eta_secs = Math.round(((100 - job.progress) / speed) * (TICK_MS / 1000) * 12);
      if (job.progress >= 100) Object.assign(job, { stage: "verifying", progress: 0, fps: null, speed: null });
    } else if (job.stage === "verifying") {
      job.progress = Math.min(100, job.progress + 9);
      job.eta_secs = Math.round((100 - job.progress) / 9) * 3;
      if (job.progress >= 100) Object.assign(job, { stage: "finalizing", progress: 0, eta_secs: 2 });
    } else if (job.stage === "finalizing") {
      job.progress = Math.min(100, job.progress + 50);
      if (job.progress >= 100) {
        finishJob(job);
        continue;
      }
    }
    const file = files.get(job.file_id);
    if (file) file.progress = job.progress;
    broadcast({
      type: "job.progress",
      job_id: job.id,
      file_id: job.file_id,
      stage: job.stage,
      progress: Number(job.progress.toFixed(1)),
      fps: job.fps ? Number(job.fps.toFixed(1)) : null,
      speed: job.speed ? Number(job.speed.toFixed(2)) : null,
      eta_secs: job.eta_secs,
      encoder: job.encoder,
      hw_api: job.hw_api,
      attempt: job.attempt,
    });
  }
  // Keep the demo busy: top the queue up from files that still need work.
  if (settings.auto_queue && [...jobs.values()].filter((j) => j.state === "queued").length < 3) {
    const pending = [...files.values()].filter((f) => f.status === "pending" && libraries.get(f.library_id)?.enabled).slice(0, 5);
    if (pending.length) {
      pending.forEach((f) => queueFile(f));
      broadcast({ type: "files.changed", library_id: null });
      emitQueue();
    }
  }
  startJobs();
}
setInterval(tick, TICK_MS);

/**
 * Files still being copied settle like on the real server: each arrives as
 * "Found <name> in <library>" (never as a new scan summary) and the
 * library's `settling` count goes down to 0.
 */
if (SETTLE_MS > 0) {
  setTimeout(() => {
    const timer = setInterval(() => {
      const next = [...settling.entries()].find(([, n]) => n > 0);
      if (!next) {
        clearInterval(timer);
        return;
      }
      const [libraryId, n] = next;
      const library = libraries.get(libraryId);
      settling.set(libraryId, n - 1);
      if (!library) return;
      const file = makeFile(library, `Arrivals/New Episode ${String(4 - n).padStart(2, "0")}.mkv`, {
        size: between(0.8e9, 2.4e9),
        codec: "h264",
        resolution: "1080p",
        duration: between(21, 45) * 60,
        audio: "aac",
      });
      emitActivity(addActivity("info", `Found ${file.file_name} in ${library.name}`, { library_id: library.id, file_id: file.id }));
      emitLibrary(library);
      broadcast({ type: "files.changed", library_id: library.id });
    }, 4000);
  }, SETTLE_MS);
}

function simulateScan(library, generate) {
  if (scanning.has(library.id)) return false;
  scanning.add(library.id);
  emitLibrary(library);
  let discovered = 0;
  const total = generate ? 64 : [...files.values()].filter((f) => f.library_id === library.id).length;
  const step = () => {
    if (!libraries.has(library.id)) return;
    if (discovered < total) {
      discovered = Math.min(total, discovered + 16);
      broadcast({ type: "scan.progress", library_id: library.id, library_name: library.name, phase: "discovering", discovered, analyzed: 0, to_analyze: 0 });
      setTimeout(step, 400);
      return;
    }
    if (generate) {
      for (let i = 0; i < 64; i++) {
        const res = rand() < 0.6 ? "1080p" : "720p";
        makeFile(library, `Sample ${String(Math.floor(i / 8) + 1).padStart(2, "0")}/Sample clip ${String(i + 1).padStart(3, "0")}.mkv`, {
          size: res === "1080p" ? between(1.5e9, 6e9) : between(0.5e9, 2e9),
          codec: rand() < 0.15 ? "hevc" : "h264",
          resolution: res,
          duration: between(20, 110) * 60,
          audio: "aac",
        });
      }
    }
    broadcast({ type: "scan.progress", library_id: library.id, library_name: library.name, phase: "analyzing", discovered, analyzed: total, to_analyze: total });
    setTimeout(() => {
      scanning.delete(library.id);
      library.last_scan_at = iso(Date.now());
      if (settings.auto_queue && library.enabled) {
        [...files.values()].filter((f) => f.library_id === library.id && f.status === "pending").slice(0, 20).forEach((f) => queueFile(f));
      }
      broadcast({ type: "scan.progress", library_id: library.id, library_name: library.name, phase: "done", discovered, analyzed: total, to_analyze: total });
      broadcast({ type: "files.changed", library_id: library.id });
      emitLibrary(library);
      emitStats();
      emitQueue();
      emitActivity(addActivity("info", `Scanned ${library.name}: ${total} files.`, { library_id: library.id }));
    }, 900);
  };
  setTimeout(step, 300);
  return true;
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

class HttpError extends Error {
  constructor(status, code, message, field = null) {
    super(message);
    this.status = status;
    this.code = code;
    this.field = field;
  }
}

function send(res, status, body) {
  res.writeHead(status, {
    "Content-Type": "application/json",
    "Access-Control-Allow-Origin": "*",
    "Access-Control-Allow-Methods": "GET,POST,PATCH,DELETE,OPTIONS",
    "Access-Control-Allow-Headers": "Content-Type, Accept",
    "Cache-Control": "no-store",
  });
  res.end(body === undefined ? "" : JSON.stringify(body));
}

async function readBody(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  if (!chunks.length) return {};
  if (!/^application\/json\b/i.test(req.headers["content-type"] ?? ""))
    throw new HttpError(415, "unsupported_media_type", "Send the request body as JSON (Content-Type: application/json).");
  try {
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } catch {
    throw new HttpError(400, "invalid_json", "The request body isn't valid JSON.");
  }
}

function getFile(id) {
  const file = files.get(id);
  if (!file) throw new HttpError(404, "file_not_found", "There's no file with that id.");
  return file;
}
function getJob(id) {
  const job = jobs.get(id);
  if (!job) throw new HttpError(404, "job_not_found", "There's no job with that id.");
  return job;
}
function getLibrary(id) {
  const lib = libraries.get(id);
  if (!lib) throw new HttpError(404, "library_not_found", "There's no library with that id.");
  return lib;
}

function sortFiles(list, sort) {
  const desc = sort.startsWith("-");
  const key = sort.replace(/^-/, "");
  const order = { failed: 0, processing: 1, queued: 2, pending: 3, done: 4, skipped: 5 };
  const cmp = {
    name: (a, b) => a.relative_path.localeCompare(b.relative_path),
    size: (a, b) => a.size_bytes - b.size_bytes,
    updated: (a, b) => a.updated_at.localeCompare(b.updated_at),
    status: (a, b) => order[a.status] - order[b.status] || a.relative_path.localeCompare(b.relative_path),
  }[key] ?? ((a, b) => a.relative_path.localeCompare(b.relative_path));
  return list.sort((a, b) => (desc ? -cmp(a, b) : cmp(a, b)));
}

function activeOrder(a, b) {
  if (a.state !== b.state) return a.state === "running" ? -1 : 1;
  return b.priority - a.priority || a.created_at.localeCompare(b.created_at);
}

const routes = [];
const route = (method, pattern, handler) => {
  const keys = [];
  const regex = new RegExp("^" + pattern.replace(/:(\w+)/g, (_, k) => (keys.push(k), "([^/]+)")) + "$");
  routes.push({ method, regex, keys, handler });
};

route("GET", "/api/health", () => ({ ok: true, version: "0.2.0-mock" }));
route("GET", "/api/overview", () => overview());
route("GET", "/api/libraries", () => [...libraries.values()].map(libraryView));
route("GET", "/api/libraries/:id", ({ id }) => libraryView(getLibrary(id)));

/**
 * Why a folder can't be a library (library_admin.rs library_folder_refusal):
 * the whole server, the folder holding the database (this mock's is /config),
 * the app (/app) and the system folders. Null when it can.
 */
const libraryBlocked = (path) => {
  const instead = "Choose the folder that holds your videos.";
  const within = (dir) => path === dir || path.startsWith(`${dir}/`);
  if (path === "/")
    return `The whole server can't be a library: it includes Chrysopoeia's own files and every share. ${instead}`;
  for (const dir of ["/proc", "/sys", "/dev"]) if (within(dir)) return `${dir} is a system folder, not a place for videos. ${instead}`;
  if (within("/config")) return `/config is where Chrysopoeia keeps its database and settings. ${instead}`;
  if (within("/app")) return `/app holds the Chrysopoeia app itself, which is read-only. ${instead}`;
  return null;
};

route("POST", "/api/libraries", async (_p, _q, req) => {
  const body = await readBody(req);
  const raw = String(body.path ?? "").trim();
  if (!raw) throw new HttpError(400, "path_required", "Choose a folder for the library.");
  if (!raw.startsWith("/")) throw new HttpError(400, "path_not_absolute", "Use the folder's full path, starting with /.");
  const path = raw.replace(/\/+$/, "") || "/";
  if (!(path in FS))
    throw new HttpError(
      400,
      "path_not_found",
      `The folder ${raw} doesn't exist on the server. In Docker, check that it is mounted into the container.`,
    );
  const blocked = libraryBlocked(path);
  if (blocked) throw new HttpError(400, "folder_not_allowed", blocked, "path");
  if (settings.output_mode === "folder" && settings.output_folder && (settings.output_folder === path || settings.output_folder.startsWith(`${path}/`)))
    throw new HttpError(
      400,
      "contains_output_folder",
      "The output folder is inside this folder, so Chrysopoeia would convert its own results. Pick another folder, or change the output folder in Settings.",
    );
  for (const lib of libraries.values()) {
    if (lib.path === path) throw new HttpError(409, "library_exists", `That folder is already the library ${lib.name}.`);
    if (path.startsWith(lib.path + "/"))
      throw new HttpError(409, "library_overlaps", `That folder is inside ${lib.name}, which is already a library.`);
    if (lib.path.startsWith(path + "/"))
      throw new HttpError(
        409,
        "library_overlaps",
        `That folder contains the library ${lib.name}. Pick a different folder, or remove ${lib.name} first.`,
      );
  }
  if (typeof body.name === "string" && body.name.trim().length > 100)
    throw new HttpError(400, "invalid_name", "Library names can be at most 100 characters.", "name");
  const goal = body.goal ?? body.profile?.goal ?? "save_space";
  const library = {
    id: uuid(),
    name: body.name || path.split("/").pop() || path,
    path,
    enabled: true,
    profile: normalizeProfile(body.profile ?? profileForGoal(goal)),
    stats: null,
    scanning: false,
    last_scan_at: null,
    path_error: null,
    created_at: iso(Date.now()),
  };
  libraries.set(library.id, library);
  simulateScan(library, true);
  emitActivity(addActivity("info", `Added ${library.name}. Scanning…`, { library_id: library.id }));
  return [201, libraryView(library)];
});

route("PATCH", "/api/libraries/:id", async ({ id }, _q, req) => {
  const lib = getLibrary(id);
  const body = await readBody(req);
  if (typeof body.name === "string") {
    if (!body.name.trim()) throw new HttpError(400, "invalid_name", "Give the library a name.", "name");
    if (body.name.trim().length > 100)
      throw new HttpError(400, "invalid_name", "Library names can be at most 100 characters.", "name");
    lib.name = body.name.trim();
  }
  if (typeof body.enabled === "boolean") lib.enabled = body.enabled;
  if (body.profile) {
    checkProfile(body.profile, "profile");
    lib.profile = normalizeProfile(body.profile);
  }
  emitLibrary(lib);
  return libraryView(lib);
});

route("DELETE", "/api/libraries/:id", ({ id }) => {
  getLibrary(id);
  libraries.delete(id);
  for (const [fid, f] of files) if (f.library_id === id) files.delete(fid);
  for (const [jid, j] of jobs) if (j.library_id === id) jobs.delete(jid);
  broadcast({ type: "library.removed", id });
  emitStats();
  emitQueue();
  return [204, undefined];
});

route("POST", "/api/libraries/:id/scan", ({ id }) => {
  const lib = getLibrary(id);
  if (!lib.enabled) throw new HttpError(409, "library_disabled", "This library is turned off. Turn it on to scan it.");
  if (!simulateScan(lib, false)) throw new HttpError(409, "scan_running", "This library is already being scanned.");
  return [202, { started: true }];
});
route("POST", "/api/scan", () => {
  for (const lib of libraries.values()) if (lib.enabled) simulateScan(lib, false);
  return [202, { started: true }];
});

route("GET", "/api/files", (_p, q) => {
  let list = [...files.values()];
  if (q.get("library")) list = list.filter((f) => f.library_id === q.get("library"));
  if (q.get("status")) list = list.filter((f) => f.status === q.get("status"));
  if (q.get("q")) {
    // Like the server: the file's name and folder, and the names its jobs recorded
    // (a conversion that renamed it: q=land1080.mp4 finds land1080.mkv).
    const needle = q.get("q").toLowerCase();
    const earlier = new Set();
    for (const j of jobs.values()) if (j.file_name.toLowerCase().includes(needle)) earlier.add(j.file_id);
    list = list.filter((f) => f.relative_path.toLowerCase().includes(needle) || earlier.has(f.id));
  }
  sortFiles(list, q.get("sort") ?? "name");
  const limit = Math.min(500, Number(q.get("limit") ?? 100));
  const offset = Number(q.get("offset") ?? 0);
  return { items: list.slice(offset, offset + limit).map(listFile), total: list.length };
});

route("GET", "/api/files/:id", ({ id }) => {
  const file = getFile(id);
  const fileJobs = [...jobs.values()].filter((j) => j.file_id === id).sort((a, b) => b.created_at.localeCompare(a.created_at)).slice(0, 10);
  // Files ffprobe couldn't read have no probe.
  const out = listFile(file);
  return { file: file.unreadable ? out : { ...out, probe: makeProbe(file) }, jobs: fileJobs };
});

route("POST", "/api/files/:id/queue", async ({ id }, _q, req) => {
  const file = getFile(id);
  const body = await readBody(req);
  for (const key of Object.keys(body)) {
    if (key === "priority" || key === "force") continue;
    // The real server's words for a field it doesn't know (an older one, for "force").
    throw new HttpError(
      400,
      "invalid_json",
      `The request body isn't valid (unknown field \`${key}\`, expected \`priority\` at line 1 column 8).`,
    );
  }
  if (file.status === "queued" || file.status === "processing")
    throw new HttpError(409, "already_queued", "This file is already in the queue.");
  // Job.force echoes the request; a server that ignores the field doesn't.
  const job = queueFile(file, Number(body.priority ?? 0), body.force === true && FORCE === "on");
  broadcast({ type: "file.updated", file: listFile(file) });
  broadcast({ type: "job.updated", job });
  emitQueue();
  startJobs();
  return job;
});

route("POST", "/api/files/:id/skip", ({ id }) => {
  const file = getFile(id);
  // Like the server: a converting file is cancelled first, then skipped.
  for (const j of jobs.values()) {
    if (j.file_id === id && (j.state === "queued" || j.state === "running")) {
      Object.assign(j, { state: "cancelled", fps: null, speed: null, eta_secs: null, finished_at: iso(Date.now()) });
      broadcast({ type: "job.updated", job: j });
    }
  }
  Object.assign(file, { status: "skipped", skip_reason: "Skipped by you", updated_at: iso(Date.now()) });
  broadcast({ type: "file.updated", file: listFile(file) });
  emitQueue();
  return listFile(file);
});

route("POST", "/api/files/bulk", async (_p, _q, req) => {
  const body = await readBody(req);
  let list = body.ids ? body.ids.map((i) => files.get(i)).filter(Boolean) : [...files.values()];
  if (body.library) list = list.filter((f) => f.library_id === body.library);
  if (body.status) list = list.filter((f) => f.status === body.status);
  let affected = 0;
  let leftOut = 0;
  for (const file of list) {
    // With explicit ids, only files the settings would convert (or failed
    // ones) are queued; files the size rule already kept under this goal
    // are left out too (they would end the same way).
    if (body.action === "queue" && body.ids && file.status !== "failed" && !["queued", "processing"].includes(file.status)) {
      const lib = libraries.get(file.library_id);
      const sizeRule = file.status === "skipped" && /% (?:smaller|larger)|kept the original/i.test(file.skip_reason ?? "");
      if (lib && (skipReasonFor(file, lib) || sizeRule)) {
        leftOut++;
        continue;
      }
    }
    if (body.action === "queue" && !["queued", "processing"].includes(file.status)) {
      queueFile(file);
      affected++;
    } else if (body.action === "retry_failed" && file.status === "failed") {
      queueFile(file);
      affected++;
    } else if (body.action === "skip" && file.status !== "processing") {
      for (const j of jobs.values()) if (j.file_id === file.id && j.state === "queued") Object.assign(j, { state: "cancelled", finished_at: iso(Date.now()) });
      Object.assign(file, { status: "skipped", skip_reason: "Skipped by you" });
      affected++;
    }
  }
  broadcast({ type: "files.changed", library_id: body.library ?? null });
  emitQueue();
  emitStats();
  startJobs();
  // Like the real server: `left_out` is always there (0 unless a queue request left files out).
  return { affected, left_out: body.action === "queue" && body.ids ? leftOut : 0 };
});

route("GET", "/api/jobs", (_p, q) => {
  const state = q.get("state") ?? "active";
  let list = [...jobs.values()];
  if (state === "active") list = list.filter((j) => j.state === "queued" || j.state === "running").sort(activeOrder);
  else if (state === "running") list = list.filter((j) => j.state === "running").sort(activeOrder);
  else if (state === "queued") list = list.filter((j) => j.state === "queued").sort(activeOrder);
  else list = list.filter((j) => !["queued", "running"].includes(j.state)).sort((a, b) => (b.finished_at ?? "").localeCompare(a.finished_at ?? ""));
  const limit = Math.min(500, Number(q.get("limit") ?? 100));
  const offset = Number(q.get("offset") ?? 0);
  return { items: list.slice(offset, offset + limit), total: list.length };
});
route("GET", "/api/jobs/:id", ({ id }) => getJob(id));

route("POST", "/api/jobs/:id/cancel", ({ id }) => {
  const job = getJob(id);
  if (job.state !== "queued" && job.state !== "running") throw new HttpError(409, "job_finished", "This job has already finished.");
  Object.assign(job, { state: "cancelled", fps: null, speed: null, eta_secs: null, finished_at: iso(Date.now()) });
  const file = files.get(job.file_id);
  if (file) {
    // A converted file queued again stays converted.
    settleFile(file, "pending");
    broadcast({ type: "file.updated", file: listFile(file) });
  }
  broadcast({ type: "job.updated", job });
  emitQueue();
  return job;
});

route("POST", "/api/jobs/:id/priority", async ({ id }, _q, req) => {
  const job = getJob(id);
  const body = await readBody(req);
  if (!["queued", "running"].includes(job.state)) throw new HttpError(409, "job_finished", "This job has already finished.");
  if (body.move !== undefined && body.move !== "top")
    throw new HttpError(400, "invalid_request", 'The only supported move is "top".');
  if (body.move === undefined && typeof body.priority !== "number")
    throw new HttpError(400, "invalid_request", 'Send a priority number or {"move": "top"}.');
  if (body.move === "top") job.priority = Math.max(0, ...[...jobs.values()].filter((j) => j.state === "queued").map((j) => j.priority)) + 1;
  else if (typeof body.priority === "number") job.priority = body.priority;
  broadcast({ type: "job.updated", job });
  return job;
});

route("POST", "/api/jobs/clear", () => {
  let affected = 0;
  for (const [id, j] of jobs) {
    if (!["queued", "running"].includes(j.state)) {
      jobs.delete(id);
      affected++;
    }
  }
  return { affected };
});

route("GET", "/api/queue", () => queueState());
route("POST", "/api/queue/pause", () => {
  paused = true;
  emitQueue();
  return queueState();
});
route("POST", "/api/queue/resume", () => {
  paused = false;
  startJobs();
  emitQueue();
  return queueState();
});
route("POST", "/api/queue/stop", () => {
  for (const job of jobs.values()) {
    if (job.state !== "running") continue;
    Object.assign(job, { state: "queued", stage: "waiting", progress: 0, fps: null, speed: null, eta_secs: null, started_at: null });
    const file = files.get(job.file_id);
    if (file) Object.assign(file, { status: "queued", progress: null });
    broadcast({ type: "job.updated", job });
  }
  paused = true;
  broadcast({ type: "files.changed", library_id: null });
  emitQueue();
  return queueState();
});

route("GET", "/api/settings", () => settings);
route("PATCH", "/api/settings", async (_p, _q, req) => {
  const patch = await readBody(req);
  if (!patch || typeof patch !== "object" || Array.isArray(patch))
    throw new HttpError(400, "invalid_settings", "Send the settings to change as a JSON object.");
  for (const key of Object.keys(patch)) {
    if (!(key in settings)) throw new HttpError(400, "unknown_setting", `There's no setting called "${key}".`);
  }
  const next = { ...settings, ...patch };
  const blank = (v) => (typeof v === "string" && !v.trim() ? null : typeof v === "string" ? v.trim() : v);
  next.temp_dir = blank(next.temp_dir);
  next.output_folder = blank(next.output_folder);
  // Same code, wording and `field` as the server (services/settings.rs):
  // `invalid_settings` for every validation failure, naming the setting.
  const invalid = (message, field) => new HttpError(400, "invalid_settings", message, field);
  if (next.max_jobs !== null && !(Number.isInteger(next.max_jobs) && next.max_jobs >= 1 && next.max_jobs <= 32))
    throw invalid("Files at once must be between 1 and 32.", "max_jobs");
  if (next.active_hours && (next.active_hours.start > 23 || next.active_hours.end > 23))
    throw invalid("Active hours must be whole hours from 0 to 23.", "active_hours");
  for (const pattern of next.ignore_patterns ?? []) {
    if (/\[[^\]]*$/.test(pattern))
      throw invalid(`The ignore pattern "${pattern}" has a [ without a closing ].`, "ignore_patterns");
  }
  if (next.temp_dir && !next.temp_dir.startsWith("/"))
    throw invalid("The temporary folder must be a full path starting with /.", "temp_dir");
  if (next.temp_dir && !(next.temp_dir in FS))
    throw invalid(
      `The temporary folder ${next.temp_dir} doesn't exist on the server. In Docker, check that it is mounted.`,
      "temp_dir",
    );
  if (next.output_mode === "folder" && !next.output_folder)
    throw invalid("Choose an output folder, or switch back to replacing the originals.", "output_folder");
  if (next.output_mode === "folder" && !next.output_folder.startsWith("/"))
    throw invalid("The output folder must be a full path starting with /.", "output_folder");
  if (next.output_mode === "folder" && !(next.output_folder in FS))
    throw invalid(
      `The output folder ${next.output_folder} doesn't exist on the server. In Docker, check that it is mounted.`,
      "output_folder",
    );
  if (next.output_mode === "folder") {
    for (const lib of libraries.values()) {
      if (next.output_folder === lib.path || next.output_folder.startsWith(`${lib.path}/`))
        throw invalid(
          `The output folder can't be inside the library ${lib.name}, or Chrysopoeia would convert its own results.`,
          "output_folder",
        );
    }
  }
  if (patch.default_profile) {
    checkProfile(patch.default_profile, "default_profile");
    next.default_profile = normalizeProfile(patch.default_profile);
  }
  Object.assign(settings, next);
  broadcast({ type: "settings.updated", settings });
  emitQueue();
  startJobs();
  return settings;
});

route("GET", "/api/hardware", () => (DETECT_MS > 0 && Date.now() - STARTED < DETECT_MS ? detectingPlaceholder() : hardware));
route("POST", "/api/hardware/detect", async () => {
  await new Promise((r) => setTimeout(r, 2500));
  hardware = { ...makeHardware(), detected_at: iso(Date.now()) };
  broadcast({ type: "hardware.updated", hardware });
  return hardware;
});

route("GET", "/api/presets", () => presets);

route("GET", "/api/system", () => ({
  version: "0.2.0-mock",
  build: "edge-mock",
  // The Docker image sets TEMP_DIR=/temp when that folder is mapped.
  default_temp_dir: "/temp",
  browse_roots: BROWSE_ROOTS,
  data_dir: "/config",
  in_container: true,
}));

route("GET", "/api/fs/browse", (_p, q) => {
  const path = (q.get("path")?.trim() || BROWSE_ROOTS[0]).replace(/(.)\/+$/, "$1");
  if (!path.startsWith("/")) throw new HttpError(400, "path_not_absolute", "Use a full folder path, starting with /.");
  if (!BROWSE_ROOTS.some((r) => r === "/" || path === r || path.startsWith(r + "/")))
    throw new HttpError(403, "outside_roots", "That folder is outside the folders Chrysopoeia may show.");
  if (!(path in FS)) throw new HttpError(404, "path_not_found", "That folder doesn't exist.");
  const parent = path === "/" ? null : path.slice(0, path.lastIndexOf("/")) || "/";
  return {
    path,
    parent,
    roots: BROWSE_ROOTS,
    entries: FS[path].map((name) => {
      const full = path === "/" ? `/${name}` : `${path}/${name}`;
      return { name, path: full, is_dir: true, ...mediaCount(full) };
    }),
    // Round 4: the browsed folder's own count, by the same rules.
    ...mediaCount(path),
    // The picker disables "Use" for a library's folder with this.
    ...(libraryBlocked(path) ? { library_blocked: libraryBlocked(path) } : {}),
  };
});

route("GET", "/api/activity", (_p, q) => ({ items: activity.slice(0, Math.min(500, Number(q.get("limit") ?? 100))) }));

const server = http.createServer(async (req, res) => {
  if (req.method === "OPTIONS") return send(res, 204);
  if (HOST_DENY) {
    return send(res, 403, {
      error: `Chrysopoeia doesn't answer to the address "${req.headers.host ?? ""}". Add it to ALLOWED_HOSTS.`,
      code: "host_not_allowed",
    });
  }
  const url = new URL(req.url ?? "/", "http://localhost");
  for (const r of routes) {
    if (r.method !== req.method) continue;
    const m = url.pathname.match(r.regex);
    if (!m) continue;
    const params = Object.fromEntries(r.keys.map((k, i) => [k, decodeURIComponent(m[i + 1])]));
    try {
      const result = await r.handler(params, url.searchParams, req);
      if (Array.isArray(result) && typeof result[0] === "number" && result.length === 2) return send(res, result[0], result[1]);
      return send(res, 200, result);
    } catch (err) {
      if (err instanceof HttpError) {
        const body = { error: err.message, code: err.code };
        if (err.field) body.field = err.field;
        return send(res, err.status, body);
      }
      console.error(err);
      return send(res, 500, { error: "The mock server hit a bug.", code: "internal" });
    }
  }
  if (url.pathname.startsWith("/api/") && routes.some((r) => url.pathname.match(r.regex))) {
    return send(res, 405, { error: "This API endpoint doesn't accept that kind of request.", code: "method_not_allowed" });
  }
  send(res, 404, { error: `There's no API endpoint at ${url.pathname}.`, code: "not_found" });
});

const wss = new WebSocketServer({ noServer: true });
server.on("upgrade", (req, socket, head) => {
  const { pathname } = new URL(req.url ?? "/", "http://localhost");
  if (pathname !== "/api/ws" || !WS_ON) return socket.destroy();
  wss.handleUpgrade(req, socket, head, (ws) => {
    sockets.add(ws);
    ws.on("close", () => sockets.delete(ws));
    ws.send(JSON.stringify({ type: "queue.state", ...queueState() }));
    ws.send(JSON.stringify({ type: "stats.updated", totals: statsFor([...files.values()]) }));
  });
});
setInterval(() => {
  for (const ws of sockets) ws.ping();
}, 30_000);

server.listen(PORT, () => {
  console.log(
    `Chrysopoeia mock API (FAKE sample data, scenario "${SCENARIO}"${WS_ON ? "" : ", WebSocket off"}) on http://localhost:${PORT}/api`,
  );
});
