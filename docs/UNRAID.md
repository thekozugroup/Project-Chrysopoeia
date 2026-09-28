# Chrysopoeia on Unraid

This guide takes you from nothing to a running, GPU-accelerated Chrysopoeia in
about ten minutes. Hardware acceleration is optional: skip step 1 to encode on
the CPU.

You need Unraid 6.12 or newer (7.x for Intel Arc or Core Ultra graphics) with
the **Community Applications** plugin (the *Apps* tab) installed.

## 1. Make the GPU available to Docker (optional)

Pick the section for your graphics. If you have more than one, you can do
both; Chrysopoeia tests every encoder it can see and uses the best that works.

### NVIDIA

1. **Apps** > search **Nvidia Driver** (by ich777) > **Install**. The plugin
   downloads the driver that matches your Unraid version; wait until it says
   it is done.
2. Open **Settings > Nvidia Driver**. Your card is listed with a **GPU UUID**
   (`GPU-xxxxxxxx-...`). Copy it, or plan to use `all`.
3. Restart Docker so it picks up the NVIDIA runtime: **Settings > Docker >
   Enable Docker: No > Apply**, then **Yes > Apply** (or reboot).

In the template (step 3) you will add `--runtime=nvidia` and set
`NVIDIA_VISIBLE_DEVICES`.

### Intel (Quick Sync)

1. Make sure the integrated graphics are enabled in the BIOS. With a
   discrete GPU installed, many boards turn the iGPU off unless an option such
   as *iGPU Multi-Monitor* is enabled.
2. **Apps** > search **Intel GPU TOP** (by ich777) > **Install**. It loads the
   Intel graphics driver, which Unraid does not load by default.
3. Open the Unraid terminal (the `>_` icon) and run `ls -l /dev/dri`. You
   should see `renderD128`. If not, reboot once.

In the template (step 3) you will add `/dev/dri` as a device.

### AMD

1. **Apps** > search **Radeon TOP** (by ich777) > **Install**. It loads the
   `amdgpu` driver.
2. In the terminal, `ls -l /dev/dri` should list `renderD128`.

In the template (step 3) you will add `/dev/dri` as a device.

## 2. Install Chrysopoeia

**Apps** > search **Chrysopoeia** > **Install**.

If it is not listed yet, add the template by hand. In the Unraid terminal:

```sh
wget -O /boot/config/plugins/dockerMan/templates-user/my-Chrysopoeia.xml \
  https://raw.githubusercontent.com/thekozugroup/Project-Chrysopoeia/main/unraid/chrysopoeia.xml
```

Then **Docker > Add Container**, and choose **Chrysopoeia** in the *Template*
list.

## 3. Fill in the template

| Field | What to enter |
|---|---|
| Web UI port | `8080`, or any free port. |
| Config | `/mnt/user/appdata/chrysopoeia` (the default). Holds the database; it stays small. |
| Media | **Required.** The share that holds your videos, e.g. `/mnt/user/media/` (click the field to browse). Choose only what you want converted, never all of `/mnt/user/`: Chrysopoeia replaces files in this folder, so other apps' folders (appdata, photo libraries, camera recordings) must stay out of it. Inside the app this folder is `/media`. |
| Transcode cache | Optional. A folder on an SSD pool for in-progress files, e.g. `/mnt/cache/chrysopoeia-temp/`. See [Transcode cache](#transcode-cache-on-an-ssd). |

**Intel or AMD graphics:** add the GPU as a device. Do this only if
`ls -l /dev/dri` listed `renderD128` in step 1: Docker will not start a
container whose device does not exist, which is why the template does not
include it by default.

1. At the bottom of the template, click **Add another Path, Port, Variable,
   Label or Device**.
2. Set **Config Type** to *Device*, **Name** to `Intel/AMD GPU` and **Value**
   to `/dev/dri`.
3. Click **Add**.

Under **Show more settings**:

| Field | Default | Notes |
|---|---|---|
| PUID / PGID | `99` / `100` | Unraid's `nobody` / `users`. Keep them unless your media is owned by someone else. |
| UMASK | `002` | New files are readable by everyone and editable by the `users` group. |
| HW_ACCEL | `auto` | Uses the best encoder that passes a test encode. `cpu` never uses the GPU; `nvenc` (NVIDIA), `qsv` (Intel) or `vaapi` (Intel or AMD) forces one. Applied on the first start and on the next start whenever you change it here; in between, the choice in the app (Settings › Hardware) is kept. `auto` never overrides a choice made in the app. |
| MAX_JOBS | empty | Empty = automatic. A number here replaces the automatic count while *Files at once* is *Automatic* in the app (Settings › Processing); a number chosen in the app wins. |
| ALLOWED_HOSTS | empty | Only behind a reverse proxy: the domain name you open Chrysopoeia at. See [Reverse proxy](#reverse-proxy-swag-nginx-proxy-manager-traefik). |
| NVIDIA_VISIBLE_DEVICES | empty | NVIDIA: `all`, or one GPU UUID to use only that card. |

**NVIDIA only:** switch the editor to **Advanced View** (toggle at the top
right), and put `--runtime=nvidia` in **Extra Parameters**.

The time zone does not need setting: Unraid passes your server's time zone to
every container.

Click **Apply**. Unraid pulls the image and starts the container.

## 4. First run

1. On the **Docker** tab, click the Chrysopoeia icon > **WebUI**. The welcome
   page already says which GPU it found (or that your CPU will do the work).
2. Click **Choose a folder**. The folder browser opens at `/media`, which is
   the Media share you picked. Open the folder you want converted (for
   example `Movies`) and click **Use this folder**. You can add more libraries
   later.
3. Choose a goal. *Balanced* (HEVC) is fast with a GPU and plays on most TVs;
   *Save space* (AV1) gives the smallest files; *Plays everywhere* (H.264)
   suits old devices. The cards show how fast your hardware handles each one.
4. Click **Start**. Chrysopoeia scans the folder, queues the files that need
   work, and starts converting. The **Overview** shows progress and space
   saved; the **Queue** shows each running file with its speed and time left.

Open **Settings > Hardware** to confirm your GPU was found: its encoders show as
*verified*. If they do not, the page shows a hint with the fix; see
[Troubleshooting](#troubleshooting).

Files that are new or changed later are picked up on their own: folder
watching sees changes made through `/mnt/user` shares, and every library is
rescanned every 12 hours (Settings > Processing).

## Transcode cache on an SSD

Without a cache folder, Chrysopoeia writes each new file next to the original,
which on Unraid means onto the array while it encodes. A folder on an SSD pool
keeps that work off the array and your parity drive, and the finished file is
copied into place once, after it passes verification.

1. Create the folder on your pool, e.g. in the terminal:
   `mkdir -p /mnt/cache/chrysopoeia-temp` (use your pool's name instead of
   `cache` if it differs). Using the pool path directly, rather than a
   `/mnt/user/...` share path, avoids Unraid's share overhead.
2. Set **Transcode cache** in the template to that folder.
3. Leave enough free space for the largest file you convert, times the number
   of jobs that run at once.

Never point it at `/tmp` or leave it inside the container: large files there
end up in RAM or in `docker.img`, which can fill up and stop all containers.

## Reverse proxy (SWAG, Nginx Proxy Manager, Traefik)

Opening Chrysopoeia at `http://<server IP>:8080`, `http://tower:8080` or
`http://tower.local:8080` needs no setup. To open it at a domain name through
a reverse proxy:

1. Point the proxy at `http://<server IP>:8080` (or the container name, when
   both are on the same custom Docker network).
2. Turn on WebSocket support (Nginx Proxy Manager: *Websockets Support*;
   SWAG and Traefik pass WebSockets through already). Live progress uses
   `/api/ws`.
3. **Edit** the Chrysopoeia container, click **Show more settings**, set
   **ALLOWED_HOSTS** to the domain, e.g. `transcode.example.com` (several:
   separate with commas), and click **Apply**.

Without step 3 the app shows *Chrysopoeia doesn't answer to the address
"transcode.example.com"*. This check stops other websites from reaching
Chrysopoeia through your browser. Chrysopoeia has no login of its own yet, so
add authentication at the proxy (Authelia, Authentik or basic auth) before
making it reachable from the internet.

## PUID, PGID and permissions

Unraid shares are normally owned by `nobody:users` (99:100), which is why the
template uses those ids. Chrysopoeia starts as root only long enough to adopt
them, join the group that owns your GPU device, and fix the owner of its
config folder; then it drops to that user. If the log says it *cannot write to
/media*, your media is owned by a different user: either set PUID/PGID to that
owner, or run **Tools > New Permissions** on the share (this resets it to
`nobody:users`).

## Backups

Everything Chrysopoeia needs is in `/mnt/user/appdata/chrysopoeia`: the
database with your libraries, settings and history. Include it in your appdata
backup (for example the *Appdata Backup* plugin, which stops the container
while it copies, keeping the database consistent).

If the folder is ever lost, nothing in your media is affected: add the
libraries again and files that were already converted are recognised as
already efficient and skipped. Only history and statistics are lost.

## Updating

**Docker** tab > **Check for Updates** > **apply update** next to Chrysopoeia.
Your settings and libraries are kept. Jobs that were running are restarted
from the beginning after the update; originals are never left half-replaced.

## Trying a test build

Builds that are not released yet (for example a branch before it is merged)
are published as `ghcr.io/thekozugroup/chrysopoeia:edge` when someone runs the
*Release* workflow on that branch. To use one, **Edit** the container, set
**Repository** to `ghcr.io/thekozugroup/chrysopoeia:edge` and click **Apply**.
Set it back to `ghcr.io/thekozugroup/chrysopoeia:latest` to return to the
released version; your settings and libraries are kept either way.

Before the very first release there is no `latest` yet: install the template
from the branch instead (replace `main` in the `wget` address of step 2 with
the branch name, e.g. `.../Project-Chrysopoeia/my-branch/unraid/chrysopoeia.xml`)
and set Repository to the `edge` image before clicking **Apply**.

## Troubleshooting

Start with the app itself:

- **Settings > Hardware** lists the CPU and every GPU the container can see,
  each hardware encoder with the result of its test encode (and the error when
  it failed), and a hint with the exact fix when something is missing, for
  example a GPU visible on the host but not passed to the container. After
  changing the template, click **Check again** on that page, or restart the
  container.
- Each failed file in the **Queue** says why in a sentence; open it for the
  verification report, the ffmpeg command and the end of ffmpeg's log.

Then the container log (Docker tab > Chrysopoeia icon > **Logs**). The first
lines list the version, the user it runs as, the transcode folder and every
GPU device it can see.

| Problem | Fix |
|---|---|
| Container will not start: *error gathering device information while adding custom device "/dev/dri"* | The template has a `/dev/dri` device, but the server has no `/dev/dri`. Install Intel GPU TOP or Radeon TOP and reboot, or remove the device: **Edit** the container and click **Remove** next to it. |
| Log warns that a device *belongs to the root group* | Chrysopoeia does not join the root group, for safety. In the Unraid terminal run `chgrp video /dev/dri/renderD128 && chmod g+rw /dev/dri/renderD128` (with the device named in the warning), then restart the container. To keep it after a reboot, add the same line to `/boot/config/go`. |
| NVIDIA card not used; log says *NVIDIA_VISIBLE_DEVICES is set but no NVIDIA GPU is visible* | Add `--runtime=nvidia` to Extra Parameters (Advanced View). After installing the Nvidia-Driver plugin, restart Docker once. |
| NVIDIA encoders fail on the Hardware page | Check that the driver plugin shows your card, that `NVIDIA_VISIBLE_DEVICES` is `all` or the right UUID, and that another container is not holding all encode sessions. `docker exec Chrysopoeia nvidia-smi` should list the card. |
| Intel/AMD: no hardware encoders, `/dev/dri` present | Check that `renderD128` exists (`ls -l /dev/dri`). The Hardware page shows the exact error; permission errors mean the container was started with a custom `--user`: remove it and use PUID/PGID. |
| Log says *cannot write to /media* | See [PUID, PGID and permissions](#puid-pgid-and-permissions). |
| New files are not picked up | Folder watching sees changes made through `/mnt/user` shares. Files added directly to a disk (`/mnt/disk1/...`) are found by the periodic rescan (every 12 hours by default), or click **Scan now** on the library. |
| The server feels slow while converting | Lower *Files at once* in Settings > Processing, or turn on *When to convert* there so conversions run overnight. |
| Nothing converts at night / during the day as expected | *When to convert* uses the server's time zone. Unraid passes it automatically; check **Settings > Date and Time**. |
| The app says *Chrysopoeia doesn't answer to the address …* | You opened it through a domain name. Add that name to ALLOWED_HOSTS (see [Reverse proxy](#reverse-proxy-swag-nginx-proxy-manager-traefik)). |
| MAX_JOBS or HW_ACCEL seem to be ignored | A number chosen in the app under *Files at once* wins over MAX_JOBS; choose *Automatic* there to use MAX_JOBS. HW_ACCEL is applied when its value changes, so a later choice in Settings > Hardware stays until you change HW_ACCEL again. |
| A job's ffmpeg log says `set_mempolicy: Operation not permitted` | Harmless: the HEVC (x265) encoder asks for a memory placement that Docker does not allow, and carries on normally. To silence it, add `--cap-add=SYS_NICE` to Extra Parameters. |
