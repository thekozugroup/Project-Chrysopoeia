# Hardware encoding

Chrysopoeia works without a GPU. When one is available it can convert many
times faster, at the cost of somewhat larger files than the CPU encoders give
at the same visual quality.

You do not pick encoders by hand. At startup (and whenever you click **Detect
again** in Settings > Hardware) Chrysopoeia lists the encoders its ffmpeg
build has, runs a one-second test encode on each hardware encoder, and marks
only the ones that succeed as *verified*. Jobs use the best verified encoder
for the library's codec; if a hardware encode fails on a particular file, it is
retried with CPU decoding and then on the CPU. So the practical question is only
*can the container see the GPU?* The table in each section says what that takes.

The Docker image ships [jellyfin-ffmpeg 7](https://github.com/jellyfin/jellyfin-ffmpeg),
which includes NVENC, Intel Quick Sync (oneVPL and the older Media SDK), VA-API
with the Intel iHD/i965 and AMD radeonsi drivers bundled, and V4L2 and Rockchip
encoders on ARM. No driver packages are needed inside the container; NVIDIA's
driver libraries are injected by the NVIDIA runtime.

## Support matrix

Encode support by codec. Decoding is broader (every listed GPU also decodes
the codecs it encodes). "Gen" means Intel Core generation. Where a claim is
uncertain it says so: hardware support varies by exact model, so the test
encode on the Hardware page is the final word.

| Vendor | H.264 | HEVC (H.265) | AV1 | VP9 |
|---|---|---|---|---|
| NVIDIA (NVENC) | Kepler (GTX 600/700) and newer | Maxwell 2nd gen (GTX 950/960/970/980) and newer; 10-bit from Pascal (GTX 10) | Ada Lovelace (RTX 40) and newer | no |
| Intel (Quick Sync / VA-API) | Sandy Bridge (2nd gen) and newer | 8-bit from Skylake (6th gen), 10-bit from Kaby Lake (7th gen) | Arc (Alchemist, Battlemage) and Core Ultra (Meteor Lake and newer) | Ice Lake / Tiger Lake (10th/11th gen) and newer |
| AMD (VA-API) | GCN cards (HD 7700 and newer; the oldest are patchy under VA-API) | Polaris (RX 400/500) and newer, and Ryzen APUs from Raven Ridge | RDNA 3 (RX 7000) and newer, and Ryzen 7040-series APUs (Radeon 780M/760M) | no |
| Apple (VideoToolbox) | Apple Silicon; Intel Macs with Quick Sync | Apple Silicon; 2016+ Intel Macs (likely; model-dependent) | no (M3/M4 decode AV1 only) | no |
| Raspberry Pi 4 (V4L2) | yes, up to 1080p | no | no | no |
| Raspberry Pi 5 | no hardware encoder | no | no | no |
| Rockchip RK3588 (MPP) | yes (experimental in Chrysopoeia) | yes (experimental) | no | no |

Notes worth knowing:

- **NVIDIA session limit.** GeForce cards limit how many encodes run at once
  (8 per system with Linux driver 550.54 or newer; 3 to 5 with older drivers).
  Chrysopoeia runs at most 3 jobs per NVIDIA GPU by default. Professional (RTX
  A-series, Quadro) cards have no such limit; raise *Jobs at once* if you like.
- **GTX 1650.** The original GTX 1650 (TU117) has the older Volta-generation
  encoder, which does not support HEVC B-frames and gives larger files than
  other Turing cards; it still works.
- **AMD RX 6400 / 6500 XT** (Navi 24) have no video encoder at all. The rest of
  the RX 6000 series encodes H.264 and HEVC, and decodes but does not encode AV1.
- **AMD AMF** needs AMD's proprietary Linux driver stack, which the image does
  not include; AMD GPUs are used through VA-API instead, which needs nothing
  extra.
- **Older Intel (2nd to 4th gen)** is supported through VA-API with the legacy
  i965 driver; Quick Sync proper needs 5th gen (Broadwell) or newer.
- **Intel Arc and Core Ultra** need a recent kernel. On Unraid, use 7.x.
- **Intel 12th to 14th gen desktop and mobile** decode AV1 but do not encode
  it; for AV1 output on those, Chrysopoeia uses the CPU (SVT-AV1).

## NVIDIA

| Where | What to do |
|---|---|
| Host | Install the NVIDIA driver and the [NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html). Unraid: install the **Nvidia-Driver** plugin instead. |
| docker run | `--runtime=nvidia -e NVIDIA_VISIBLE_DEVICES=all`, or `--gpus all` |
| Compose | add `-f docker-compose.nvidia.yml` |
| Unraid | Extra Parameters: `--runtime=nvidia`; `NVIDIA_VISIBLE_DEVICES`: `all` or the GPU UUID from the plugin page |

The image already sets `NVIDIA_DRIVER_CAPABILITIES=compute,video,utility`;
without `video` NVENC is not available. Check from the host with
`docker exec chrysopoeia nvidia-smi`.

## Intel

| Where | What to do |
|---|---|
| Host | The `i915` (or, for Lunar Lake, Battlemage and newer, `xe`) kernel driver must be loaded so that `/dev/dri/renderD128` exists. Unraid: install the **Intel GPU TOP** plugin, which loads it. |
| docker run | `--device /dev/dri:/dev/dri` |
| Compose | add `-f docker-compose.intel-amd.yml` |
| Unraid | keep the *Intel/AMD GPU* device (`/dev/dri`) in the template |

The container adds itself to the group that owns the render node, so no
`group_add` is needed unless you start it with `--user`. Check with
`docker exec chrysopoeia vainfo --display drm --device /dev/dri/renderD128`.

## AMD

| Where | What to do |
|---|---|
| Host | The `amdgpu` driver must be loaded so that `/dev/dri/renderD128` exists. Unraid: install the **Radeon TOP** plugin. |
| docker run | `--device /dev/dri:/dev/dri` |
| Compose | add `-f docker-compose.intel-amd.yml` |
| Unraid | keep the *Intel/AMD GPU* device (`/dev/dri`) in the template |

## Apple Silicon (macOS)

Docker on macOS runs Linux in a virtual machine that cannot reach the Apple
GPU, so run Chrysopoeia natively instead:

```sh
brew install ffmpeg rust node pnpm
git clone https://github.com/thekozugroup/Project-Chrysopoeia && cd Project-Chrysopoeia
make run        # builds the UI and the server, then serves http://localhost:8080
```

Homebrew's ffmpeg includes VideoToolbox (H.264, HEVC) and the CPU encoders.

## Raspberry Pi and other ARM boards

The image is published for `linux/arm64`, so it runs on a Raspberry Pi 4 or 5
with a 64-bit OS and on other ARM64 boards.

- **Pi 4:** hardware H.264 encoding up to 1080p through V4L2. Pass the
  encoder device: `--device /dev/video11` (add `/dev/video10` for hardware
  decoding). HEVC and AV1 output use the CPU, which is slow on a Pi.
- **Pi 5:** no hardware video encoder; everything is encoded on the CPU.
- **Rockchip RK3588** boards: jellyfin-ffmpeg includes Rockchip MPP encoders.
  Pass `--device /dev/dri --device /dev/dma_heap --device /dev/mpp_service
  --device /dev/rga`. Support in Chrysopoeia is experimental.

## CPU only

No setup needed. CPU encoders give the best quality per byte: SVT-AV1 for AV1,
x265 for HEVC, x264 for H.264 and libvpx for VP9. Chrysopoeia runs one job per
four CPU cores (up to 8) and runs ffmpeg at low priority by default, so the
rest of the server stays responsive.

## When a GPU is not detected

Open Settings > Hardware. It lists the devices the container can see, each
encoder's test result with the error if it failed, and a hint with the fix.
The container log also prints the GPU devices it found at startup
(`docker logs chrysopoeia`). The common causes:

| Symptom | Fix |
|---|---|
| No `/dev/dri` in the container | Pass `--device /dev/dri:/dev/dri` (Unraid: the *Intel/AMD GPU* device). If the host has no `/dev/dri` either, load the driver (Unraid: Intel GPU TOP / Radeon TOP plugin). |
| `/dev/dri` exists but encoders fail with "permission denied" | Start the container as root with `PUID`/`PGID` (the default) so it can join the render group, or add `--group-add <gid of /dev/dri/renderD128>` when using `--user`. |
| NVIDIA GPU listed on the host, no NVENC in the container | Add `--runtime=nvidia` and `NVIDIA_VISIBLE_DEVICES=all`; restart Docker after installing the NVIDIA driver or plugin. |
| Encoders verified but files still use the CPU | Settings > Hardware > preference is set to *CPU only*, or the library's codec has no hardware encoder on this GPU (see the matrix above). |
