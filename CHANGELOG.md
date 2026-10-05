# Changelog

All notable changes to this project are written here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

Each version tag (`vX.Y.Z`) publishes a GitHub Release whose notes start with
that version's section from this file; see
[CONTRIBUTING.md](CONTRIBUTING.md#cutting-a-release). Images: `:stable` is the
newest release, `:latest` is every build of `main`, and `:edge` is a test build
of a branch (see the [README](README.md#which-image)).

## [Unreleased]

What is coming after 0.2.0, from an evaluation on a real AMD Unraid server.

### Added

- A **stable update channel**. `ghcr.io/thekozugroup/szalinski:stable`
  follows the newest tagged release (not a prerelease) and only moves when a
  newer release is published, never backwards, so automatic updates (such as
  the Unraid Community Applications updater) install releases rather than every
  change on `main`. The Unraid template now defaults to it. `:latest` still
  follows every build of `main` and `:edge` is still the manual test build of a
  branch. `:stable` does not exist until the first version tag is pushed.
- **GitHub Releases** for every version tag, with notes made from this
  changelog and the commits since the previous release. A tag with a suffix
  such as `v1.2.0-rc.1` is published as a prerelease under its exact version
  only. The `MAJOR.MINOR` and `MAJOR` image tags also move forward only.
- **Attempt history.** Every encode attempt of a file is kept: encoder,
  decoding path, duration, command, the check or error that failed it and the
  end of ffmpeg's log. It is shown in the file's details and in the bug-report
  export, and the note about falling back to another encoder names the check
  that actually failed. Before, only the last attempt was kept, so a metadata
  problem looked like a hardware problem.
- **Progress without output time.** When ffmpeg reports frames and speed but
  not yet how much output it has written (typical on the CPU fallback), the
  Queue shows progress estimated from the frames processed, or says plainly
  that it cannot tell yet and shows frames and time elapsed, instead of 0% and
  no time left. It never shows a made-up percentage or time left.

### Changed

- **Renamed to Szalinski** (it was Chrysopoeia), after the inventor in *Honey,
  I Shrunk the Kids*. The image is now `ghcr.io/thekozugroup/szalinski`, the
  binary `szalinski`, the Unraid template `unraid/szalinski.xml`, and the
  build label variable `SZALINSKI_VERSION`. An existing install keeps working:
  the database (`chrysopoeia.db`) is carried over to `szalinski.db` on the
  first start, backup and temporary files left by the old name are still found
  and put back after an interrupted conversion, the container treats the old
  database and lock as its own (no warning about a shared Config folder, and
  their owner is fixed like the new files'), and the old image's lock is
  honoured so the two can never share a `/config` folder. Every release is
  also published under the old image name for a while, so an install that
  still pulls `ghcr.io/thekozugroup/chrysopoeia` keeps updating; switch its
  Repository to `ghcr.io/thekozugroup/szalinski:stable` when convenient (see
  [docs/UNRAID.md](docs/UNRAID.md#moving-from-chrysopoeia)).
- The display font is now **Newsreader** (it was Instrument Serif).
- Web UI notes: the UI no longer says "100% finished" while another discovered
  file is still settling; the folder picker puts the mounted media and output
  folders first instead of listing the container's system folders beside them;
  and the track list shows each track's codec on its own, so a long release-name
  title no longer hides it.
- Documentation: *Save space* converts audio to Opus (keeping channel layout,
  language and flags). It does not keep lossless or Atmos audio; *Balanced* and
  *Archive* copy the original audio instead.

### Fixed

- **Stale duration tags no longer fail verification.** A clip cut from a longer
  film with ffmpeg's remux keeps the old localized `DURATION-eng` tag next to
  the current `DURATION`. Verification used whichever it found first, compared
  the whole film's length with the clip and failed the file after minutes of
  retries on every encoder. The current `DURATION` tag is now preferred every
  time, a duration that disagrees with the actual timing of the media is
  checked against it, and obsolete localized duration and statistics tags are
  removed from converted files. A truncated output still fails, and the full
  decode and the visual comparison are unchanged.
- **Files whose stated length can't be trusted** are measured by where their
  packets end. A Matroska file written as a live stream states no length but
  its tags, so an old `DURATION-eng` was believed: every attempt failed and
  a healthy original was reported as "damaged or incomplete". A file whose
  timestamps start later than zero (a clip starting at 10:00) states where it
  ends rather than how long it is, so its one minute was read as eleven and
  even a conversion with current tags failed. Both are now measured from
  their start by their packets (without decoding; a whole-file listing, about
  5 s per GB, only when the end can't be read directly), by the scanner as
  well as verification, and the length is left unknown rather than guessed
  when that fails (verification then keeps the original rather than pass a
  new file it can't check). An original is only called cut off when its
  length, read again this way, shows it, so an original that really is cut
  short is still reported. Statistics tags stored for a whole file (an MP4's
  `DURATION-eng`) are also removed from converted MKV files.

## [0.2.0] - 2026-10-04

The first release, published as Chrysopoeia (from `main`; it was not tagged).

### Added

- **One container, one web page.** The server, job queue, workers and web UI
  are a single process on port 8080, with images for linux/amd64 and
  linux/arm64, and nothing else to set up.
- **Goals instead of plugins.** *Save space* (AV1 video, Opus audio, MKV),
  *Balanced* (HEVC video, original audio, MKV), *Plays everywhere* (H.264 video,
  AAC audio, MP4) and *Archive* (highest-quality AV1, original audio, MKV). Every
  setting can still be changed under *More format options*. The first-run
  screen marks the goal that suits the machine.
- **Formats.** Reads anything ffmpeg can read. Writes AV1, HEVC, H.264 or VP9 in
  MKV, MP4 or WebM, with audio copied or converted to Opus, AAC, FLAC, AC-3,
  E-AC-3, MP3 or Vorbis.
- **Hardware set up for you.** NVIDIA NVENC, Intel Quick Sync, VA-API (Intel and
  AMD) and Apple VideoToolbox are found at start and used only if a test encode
  passes; CPU encoders (SVT-AV1, x265, x264, libvpx) are the fallback, and a
  hardware encode that fails is retried on the CPU. How many files convert at
  once follows the CPU cores, memory, container limits and GPU.
- **Verified before replaced.** Each result must decode from start to finish,
  keep the expected streams and duration, and match the original visually (SSIM
  at several points). Only then is the original swapped out, in a way that
  survives crashes and power loss. Checks come in three levels: *Quick*,
  *Standard* (the default) and *Thorough*.
- **Nothing is lost silently.** Subtitles, fonts, cover images, chapters,
  metadata, HDR signalling and 10-bit colour are kept where the target format
  allows. When it cannot hold something the original has, the file is skipped
  with the reason, or converted into a separate output folder, instead of being
  trimmed.
- **Libraries.** Watched folders with a schedule (*When to convert*), pause,
  priorities and retry; originals replaced in place, or converted files written
  to a separate folder; files that are already efficient are skipped.
- **Plain explanations.** Every skipped or failed file says why in a sentence,
  and setup problems appear under *Needs your attention* on the Overview with
  the setting to change. Shares or drives that stop answering never fail a job:
  it waits and starts again when they come back.
- **Installing.** An Unraid Community Applications template with a step-by-step
  [guide](docs/UNRAID.md), `docker run` and Compose files (with NVIDIA and
  Intel/AMD overlays), `PUID`/`PGID`/`UMASK` handling, reverse-proxy support
  through `ALLOWED_HOSTS`, and the published images `:latest` (every build of
  `main`) and `:edge` (a test build of a branch).

[Unreleased]: https://github.com/thekozugroup/Project-Chrysopoeia/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/thekozugroup/Project-Chrysopoeia/releases/tag/v0.2.0
