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

### AMD

1. **Apps** > search **Radeon TOP** (by ich777) > **Install**. It loads the
   `amdgpu` driver.
2. In the terminal, `ls -l /dev/dri` should list `renderD128`.

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
| Media | The share to convert, e.g. `/mnt/user/media/`. The default `/mnt/user/` exposes every share; narrowing it is safer. Inside the app this folder is `/media`. |
| Transcode cache | Optional. A folder on an SSD pool for in-progress files, e.g. `/mnt/cache/chrysopoeia-temp/`. See [Transcode cache](#transcode-cache-on-an-ssd). |
| Intel/AMD GPU | Keep `/dev/dri` if you did the Intel or AMD step. **Remove this entry** (the *Remove* button next to it) if your server has no `/dev/dri` (NVIDIA-only or no GPU): Docker refuses to start a container with a device that does not exist. |

Under **Show more settings**:

| Field | Default | Notes |
|---|---|---|
| PUID / PGID | `99` / `100` | Unraid's `nobody` / `users`. Keep them unless your media is owned by someone else. |
| UMASK | `002` | New files are readable by everyone and editable by the `users` group. |
| HW_ACCEL | `auto` | Uses the best encoder that passes a test encode. `cpu` forces CPU encoding. |
| MAX_JOBS | empty | Empty = automatic. You can change it later in the app. |
| NVIDIA_VISIBLE_DEVICES | empty | NVIDIA: your GPU UUID or `all`. |

**NVIDIA only:** switch the editor to **Advanced View** (toggle at the top
right), and put `--runtime=nvidia` in **Extra Parameters**.

The time zone does not need setting: Unraid passes your server's time zone to
every container.

Click **Apply**. Unraid pulls the image and starts the container.

## 4. First run

1. On the **Docker** tab, click the Chrysopoeia icon > **WebUI**.
2. The setup screen asks for a folder: pick one under `/media` (for example
   `/media/Movies`). You can add more libraries later.
3. Choose a goal. *Balanced* (HEVC) is fast with a GPU and plays on most TVs;
   *Save space* (AV1) gives the smallest files; *Plays everywhere* (H.264)
   suits old devices.
4. Click **Start**. Chrysopoeia scans the folder, queues the files that need
   work, and starts converting.

Open **Settings > Hardware** to confirm your GPU was found: its encoders show as
*verified*. If they do not, the page shows a hint with the fix; see
[Troubleshooting](#troubleshooting).

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

## Troubleshooting

Start with **Settings > Hardware** in the app and the container log (Docker
tab > Chrysopoeia icon > **Logs**). The first lines of the log list the user it
runs as and every GPU device it can see.

| Problem | Fix |
|---|---|
| Container will not start: *error gathering device information while adding custom device "/dev/dri"* | Your server has no `/dev/dri`. Remove the *Intel/AMD GPU* entry from the template, or install Intel GPU TOP / Radeon TOP first. |
| NVIDIA card not used; log says *NVIDIA_VISIBLE_DEVICES is set but no NVIDIA GPU is visible* | Add `--runtime=nvidia` to Extra Parameters (Advanced View). After installing the Nvidia-Driver plugin, restart Docker once. |
| NVIDIA encoders fail on the Hardware page | Check that the driver plugin shows your card, that `NVIDIA_VISIBLE_DEVICES` is `all` or the right UUID, and that another container is not holding all encode sessions. `docker exec Chrysopoeia nvidia-smi` should list the card. |
| Intel/AMD: no hardware encoders, `/dev/dri` present | Check that `renderD128` exists (`ls -l /dev/dri`). The Hardware page shows the exact error; permission errors mean the container was started with a custom `--user`: remove it and use PUID/PGID. |
| Log says *cannot write to /media* | See [PUID, PGID and permissions](#puid-pgid-and-permissions). |
| New files are not picked up | Folder watching sees changes made through `/mnt/user` shares. Files added directly to a disk (`/mnt/disk1/...`) are found by the periodic rescan (every 12 hours by default), or click **Scan** on the library. |
| The server feels slow while converting | Lower *Jobs at once* in Settings > Processing, or set *Active hours* so conversions run overnight. |
