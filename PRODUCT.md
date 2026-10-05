# Product

<!-- impeccable:product-schema 1 -->

<!-- Written from the owner's brief without a live interview; items marked
(assumed) are inferences to confirm. -->

## Platform

web

## Users

People who run a home media server (Unraid, Synology, TrueNAS, a Linux box or
a Mac mini) with a large movie/TV library served by Plex, Jellyfin or Emby.
They want the library smaller or more compatible without learning ffmpeg.
Many have tried Tdarr and bounced off its plugin stacks, node setup and GPU
configuration. Some are technical (comfortable with Docker templates); many
are not video experts and do not know what CRF, NVENC or 10-bit mean (assumed).

They visit the UI in short sessions: set it up once, then check in to see
progress and space saved, occasionally fixing a failed file.

## Product Purpose

Chrysopoeia converts whole media libraries in the background to efficient
codecs (AV1, HEVC, H.264, VP9 with Opus/AAC/FLAC/AC-3/E-AC-3 audio), checks
every result for corruption and visual artifacts, and only then replaces the
original. Success: a user deploys the container, picks a folder and a goal,
and never has to think about it again while their storage frees up.

## Positioning

Tdarr-level robustness with Apple-level ease: one container, one port, no
plugin stack, no separate nodes. Hardware encoders are detected and verified
by a real test encode, job counts are set from the machine's actual cores,
memory and GPU, and every output is verified (full decode + SSIM comparison
against the source) before anything is replaced.

## Operating Context

- Deployed as a Docker container, usually from an Unraid Community
  Applications template or a compose file; accessed from a desktop browser on
  the LAN, sometimes from a phone.
- Runs for days or weeks unattended; the UI must communicate long-running
  state (queued, running, verifying, done, failed) at a glance.
- GPU passthrough (NVIDIA runtime or /dev/dri) is the most common setup
  failure; the UI must explain it in plain language with copy-paste fixes.

## Capabilities and Constraints

- Inputs: anything ffmpeg can read. Outputs: AV1, HEVC, H.264, VP9 video in
  MKV/MP4/WebM; audio copy or Opus/AAC/FLAC/AC-3/E-AC-3/MP3/Vorbis.
- Goals (presets): Save space (AV1+Opus), Balanced (HEVC, original audio),
  Plays everywhere (H.264+AAC MP4), Archive (AV1 high quality, original audio).
- Verification levels: quick, standard (default), thorough.
- Originals are replaced only after verification; output-folder mode keeps
  originals untouched.
- No accounts or authentication in v1; intended for trusted LANs (assumed).
- Terminology: "library" = a watched folder; "goal" = preset; "job" = one
  file's conversion; "verified" = passed checks.

## Brand Commitments

- Name: Chrysopoeia (alchemical transmutation into gold). Existing identity:
  warm near-black surfaces, gold accent, Newsreader display type with DM
  Sans for UI and JetBrains Mono for technical values. Keep this identity.
- Voice: calm, precise, plain. Never cute about failures; never jargon first.

## Evidence on Hand

- No real user data, testimonials or screenshots of real libraries exist.
  Do not fabricate library sizes or savings in marketing copy; the app shows
  only real numbers from the API.

## Product Principles

1. Safe by default: never lose a file. Verify before replacing; explain every skip and failure.
2. Decide for the user, let experts override: goals and automatic hardware/job settings first, raw encoder values behind "Advanced".
3. Show progress as outcomes: space saved and files verified matter more than codec charts.
4. Explain setup problems where they occur, with the exact fix to paste.

## Accessibility & Inclusion

WCAG 2.2 AA contrast and keyboard access for all controls; status never
conveyed by color alone; respects reduced motion.
