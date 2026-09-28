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
 *
 * Error codes, messages and the `field` of validation errors follow the
 * real server (crates/chrysopoeia-server), so the UI's error handling is
 * exercised the same way. Also served: `GET /api/system`, `Job.notes` and
 * `HardwareInfo.detecting` (docs/ARCHITECTURE.md, "Contract additions").
 */

import { randomUUID } from "node:crypto";
import http from "node:http";
import { WebSocketServer } from "ws";

const PORT = Number(process.env.PORT ?? 8787);
const SCENARIO = process.env.MOCK_SCENARIO ?? "demo";
const TICK_MS = Number(process.env.MOCK_TICK_MS ?? 1000);
const DETECT_MS = Number(process.env.MOCK_DETECT_MS ?? 0);
const WS_ON = (process.env.MOCK_WS ?? "on") !== "off";
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
const MEDIA_COUNT = {
  "/media": 412,
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

function addActivity(level, message, extra = {}) {
  const entry = { id: activityId++, at: iso(Date.now()), level, message, file_id: null, job_id: null, library_id: null, ...extra };
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
    error: null,
    skip_reason: null,
    validation: null,
    command: state === "queued" ? null : commandFor(file, encoder),
    log_tail: null,
    notes: [],
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
  if (file.file_name.length % 4 !== 0) return [];
  if (library?.profile.container === "mp4") {
    return ["Removed 2 picture-based subtitles because MP4 can't hold them"];
  }
  return ["Converted the MOV text subtitle to SRT so MKV can hold it"];
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
    validation: validationReport(),
    notes: notesFor(file),
    started_at: iso(finished - between(900, 3600) * 1000),
    finished_at: iso(finished),
  });
  job.created_at = iso(finished - 7200 * 1000);
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
      file.skip_reason = codec === "hevc" && library.profile.video_codec === "hevc" ? "Already HEVC" : "Only 4% smaller, so the original was kept";
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
          file.skip_reason = file.video_codec === "hevc" ? "Already efficient (HEVC), nothing to gain" : "Only 6% smaller, so the original was kept";
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
  const jobA = makeJob(failedA, "failed", {
    stage: "verifying",
    progress: 100,
    input_size: failedA.size_bytes,
    output_size: Math.round(failedA.size_bytes * 0.44),
    error: failedA.error,
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
  const failedB = pendingMovies[1];
  failedB.status = "failed";
  failedB.error = "ffmpeg stopped: the source file ends early (it may be incomplete). The original was kept.";
  makeJob(failedB, "failed", {
    stage: "transcoding",
    progress: 63,
    error: failedB.error,
    log_tail: "[matroska,webm @ 0x5581] Read error at pos. 4829122560 (0x11fd84000)\n[in#0/matroska @ 0x5580] Error during demuxing: I/O error\nConversion failed!",
    started_at: ago(12000),
    finished_at: ago(10000),
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

  addActivity("info", "Scanned Demo Movies: 136 files, 2 new.", { library_id: movies.id });
  addActivity("success", "Converted Sintel (2010).mkv and saved 4.1 GB.", { library_id: movies.id });
  addActivity("warning", `Kept the original of ${skippedFile.file_name}: only 4% smaller.`, { library_id: movies.id });
  addActivity("error", `${failedA.file_name} failed its visual check. The original was kept.`, { library_id: movies.id });
  addActivity("success", "Converted Demo Show - S01E04.mkv and saved 1.1 GB.", { library_id: tv.id });
  activity.forEach((e, i) => (e.at = ago(600 + i * 1900)));
}

if (SCENARIO === "demo" || SCENARIO === "nogpu") seedDemo();

// ---------------------------------------------------------------------------
// Derived views
// ---------------------------------------------------------------------------

function statsFor(list) {
  const s = { file_count: 0, total_bytes: 0, pending: 0, queued: 0, processing: 0, done: 0, skipped: 0, failed: 0, saved_bytes: 0 };
  for (const f of list) {
    s.file_count += 1;
    s.total_bytes += f.size_bytes;
    s[f.status] += 1;
    if (f.status === "done" && f.saved_bytes) s.saved_bytes += f.saved_bytes;
  }
  return s;
}

function libraryView(library) {
  const own = [...files.values()].filter((f) => f.library_id === library.id);
  return { ...library, stats: statsFor(own), scanning: scanning.has(library.id) };
}

function maxJobs() {
  return settings.max_jobs ?? hardware.recommended_jobs.total;
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
    max_jobs_auto: settings.max_jobs === null,
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

function listFile(f) {
  const out = { ...f };
  delete out.probe;
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

function queueFile(file, priority = 0) {
  const job = makeJob(file, "queued", { priority, created_at: iso(Date.now()) });
  file.status = "queued";
  file.updated_at = iso(Date.now());
  return job;
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
  Object.assign(job, {
    state: "done",
    stage: "finalizing",
    progress: 100,
    fps: null,
    speed: null,
    eta_secs: null,
    output_size: out,
    validation: validationReport(),
    notes: file ? notesFor(file) : [],
    finished_at: iso(Date.now()),
  });
  if (file) {
    file.original_size_bytes = job.input_size;
    file.size_bytes = out;
    file.saved_bytes = job.input_size - out;
    file.status = "done";
    file.progress = null;
    file.video_codec = lib?.profile.video_codec ?? file.video_codec;
    file.updated_at = job.finished_at;
    const day = job.finished_at.slice(0, 10);
    const entry = savingsByDay.get(day) ?? { saved_bytes: 0, files: 0 };
    entry.saved_bytes += file.saved_bytes;
    entry.files += 1;
    savingsByDay.set(day, entry);
    broadcast({ type: "file.updated", file: listFile(file) });
  }
  broadcast({ type: "job.updated", job });
  emitActivity(addActivity("success", `Converted ${job.file_name} and saved ${(job.input_size - out) / 1e9 >= 1 ? ((job.input_size - out) / 1e9).toFixed(1) + " GB" : Math.round((job.input_size - out) / 1e6) + " MB"}.`, { file_id: job.file_id, job_id: job.id, library_id: job.library_id }));
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
  if (body.profile) lib.profile = normalizeProfile(body.profile);
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
    const needle = q.get("q").toLowerCase();
    list = list.filter((f) => f.relative_path.toLowerCase().includes(needle));
  }
  sortFiles(list, q.get("sort") ?? "name");
  const limit = Math.min(500, Number(q.get("limit") ?? 100));
  const offset = Number(q.get("offset") ?? 0);
  return { items: list.slice(offset, offset + limit).map(listFile), total: list.length };
});

route("GET", "/api/files/:id", ({ id }) => {
  const file = getFile(id);
  const fileJobs = [...jobs.values()].filter((j) => j.file_id === id).sort((a, b) => b.created_at.localeCompare(a.created_at)).slice(0, 10);
  return { file: { ...file, probe: makeProbe(file) }, jobs: fileJobs };
});

route("POST", "/api/files/:id/queue", async ({ id }, _q, req) => {
  const file = getFile(id);
  const body = await readBody(req);
  if (file.status === "queued" || file.status === "processing")
    throw new HttpError(409, "already_queued", "This file is already in the queue.");
  const job = queueFile(file, Number(body.priority ?? 0));
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
  for (const file of list) {
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
  return { affected };
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
    Object.assign(file, { status: "pending", progress: null, updated_at: iso(Date.now()) });
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
    throw invalid("Jobs at once must be between 1 and 32.", "max_jobs");
  if (next.active_hours && (next.active_hours.start > 23 || next.active_hours.end > 23))
    throw invalid("Active hours must be whole hours from 0 to 23.", "active_hours");
  for (const pattern of next.ignore_patterns ?? []) {
    if (/\[[^\]]*$/.test(pattern))
      throw invalid(`"${pattern}" isn't a valid ignore pattern: unclosed character class`, "ignore_patterns");
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
  if (patch.default_profile) next.default_profile = normalizeProfile(patch.default_profile);
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
      return { name, path: full, is_dir: true, media_count: MEDIA_COUNT[full] ?? null };
    }),
  };
});

route("GET", "/api/activity", (_p, q) => ({ items: activity.slice(0, Math.min(500, Number(q.get("limit") ?? 100))) }));

const server = http.createServer(async (req, res) => {
  if (req.method === "OPTIONS") return send(res, 204);
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
