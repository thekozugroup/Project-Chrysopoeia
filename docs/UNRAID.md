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
| Config | `/mnt/user/appdata/chrysopoeia` (the default). Holds the database; it stays small. Keep it a folder of its own: never choose `/mnt/user/appdata` itself, which other apps share. |
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
| UMASK | `002` | For what Chrysopoeia creates itself (work files, folders in an output folder): editable by the `users` group. A converted file keeps the permissions of the original it replaces. |
| HW_ACCEL | `auto` | Uses the best encoder that passes a test encode. `cpu` never uses the GPU. `nvenc` (NVIDIA), `qsv` (Intel) or `vaapi` (Intel or AMD) uses only that kind of GPU; when it is missing or cannot encode the chosen format, files are still converted, on the CPU, so check Settings › Hardware: under *Details: encoders and ffmpeg*, each format its test encode passed shows *Works*. Applied on the first start and on the next start whenever you change it here; in between, the choice in the app (Settings › Hardware) is kept. `auto` never overrides a choice made in the app. |
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
   example `Movies`) and click the button at the bottom, which names the folder
   you are in: **Use “Movies”**. You can add more libraries later.
3. Choose a goal. *Balanced* (HEVC) is fast with a GPU and plays on most TVs;
   *Save space* (AV1) gives the smallest files; *Plays everywhere* (H.264)
   suits old devices; *Archive* keeps near-original quality. Each card shows
   how fast your hardware handles it, and the one that suits this machine is
   marked *Best fit*. You can rename the library here.
4. Click **Start**. Chrysopoeia scans the folder, queues the files that need
   work, and starts converting. The **Overview** shows the space saved so far
   and what is happening now; the **Queue** has three tabs: **Running** (each
   file with its step, progress and time left), **Up next** and **History**.
   Open a finished file in **History** to see the checks it passed before it
   replaced the original.

Open **Settings > Hardware** (the page headed *This machine*) to confirm your
GPU was found: it appears as a graphics card, and under **Details: encoders and
ffmpeg** the formats it can encode show *Works*. If it is missing or a format
shows *Failed test*, the page shows a setup tip with the fix; see
[Troubleshooting](#troubleshooting). Anything that stops files from converting
(a GPU the container cannot see, a read-only Media path, a full disk) also
appears on the **Overview** under **Needs your attention**, with the setting to
change and a **Try again** button for the files that waited on it.

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
2. Make sure the proxy passes on the address the browser used (the `Host`
   header). Nginx Proxy Manager, SWAG, Traefik and Caddy do this already. A
   hand-written nginx config needs `proxy_set_header Host $http_host;` in the
   `location` block, and Apache needs `ProxyPreserveHost On`. The README has
   a [complete nginx example](../README.md#behind-a-reverse-proxy).
3. Turn on WebSocket support (Nginx Proxy Manager: *Websockets Support*;
   SWAG and Traefik pass WebSockets through already). Live progress uses
   `/api/ws`.
4. **Edit** the Chrysopoeia container, click **Show more settings**, set
   **ALLOWED_HOSTS** to the domain, e.g. `transcode.example.com` (several:
   separate with commas), and click **Apply**.

Without step 4 the app shows *Chrysopoeia doesn't answer to the address
"transcode.example.com"*. Without step 2 the page opens, but saving anything
fails with *This request came from another website, so Chrysopoeia refused
it*, and progress does not update live. Both checks stop other websites from
reaching Chrysopoeia through your browser. Chrysopoeia has no login of its own
yet, so add authentication at the proxy (Authelia, Authentik or basic auth)
before making it reachable from the internet.

## PUID, PGID and permissions

Unraid shares are normally owned by `nobody:users` (99:100), which is why the
template uses those ids. Chrysopoeia starts as root only long enough to adopt
them, join the group that owns your GPU device, and fix the owner of its own
files in the config folder (the database and its lock file); then it drops to
that user. It never changes the owner of anything else there.

A converted file takes the permissions of the original it replaces. Its owner
and group stay the same too when Chrysopoeia runs as that owner, which is the
case with 99/100 on a normal share. If it cannot (the original belonged to
someone else), the file belongs to PUID/PGID and its details in the Queue say
so. UMASK applies to what Chrysopoeia creates itself, such as work files and
the folders of an output folder.

If the log says Chrysopoeia *cannot write to /media*, your media is owned by a
different user: either set PUID/PGID to that owner, or run **Tools > New
Permissions** on the share (this resets it to `nobody:users`). Until then the
Overview lists the files under **Needs your attention** as *Finished files
can't be saved*.

## Backing up

Everything Chrysopoeia keeps is in `/mnt/user/appdata/chrysopoeia`: the
database with your libraries, settings and history (`chrysopoeia.db` and,
while it runs, its `-wal` and `-shm` files). Include the folder in your appdata
backup, for example the *Appdata Backup* plugin, which stops the container
while it copies and so keeps the database consistent. Copying it by hand? Stop
the container first.

If the folder is ever lost, nothing in your media is affected: add the
libraries again and files that were already converted are recognised as
already efficient and skipped. Only history and statistics are lost. Your
media is not part of this backup: keep a separate backup of anything you cannot
replace, since conversion replaces files.

## Upgrading

**Docker** tab > **Check for Updates** > **apply update** next to Chrysopoeia.
Your settings and libraries are kept, and the database is brought up to date on
the first start. Files that were converting are stopped, their work files
removed, and they start again from the beginning; originals are never left
half-replaced. Unraid waits for the container to stop before replacing it, and
Chrysopoeia exits within a few seconds. To see which build is running, open
the bottom of **Settings** in the app (*About*) or the first lines of the
container log.

To go back to an older version, set **Repository** to that version and restore
a backup of the config folder taken before the upgrade: a database written by a
newer version is refused by an older one with a plain message.

The `latest` image follows the project's main branch, so every tested change
arrives as an update. To stay on released versions only, **Edit** the
container and set **Repository** to a version tag once releases exist, for
example `ghcr.io/thekozugroup/chrysopoeia:1.2.3` (exactly that release),
`:1.2` (only fixes for 1.2) or `:1` (the newest 1.x).

## Trying a test build

A branch that is not merged yet is published as
`ghcr.io/thekozugroup/chrysopoeia:edge` when someone opens **Actions >
Release > Run workflow** on GitHub and picks that branch. The workflow builds
the image, runs its tests and pushes it for amd64 and arm64; `latest` is not
touched. To use it, **Edit** the container, set **Repository** to
`ghcr.io/thekozugroup/chrysopoeia:edge` and click **Apply**. After the branch
is merged to `main`, `latest` carries the same change: set **Repository** back
to `ghcr.io/thekozugroup/chrysopoeia:latest`. Your settings and libraries are
kept either way.

If that run published the very first image, the package on GitHub is still
private and Unraid's pull is refused (*denied* or *unauthorized* in the pull
log). The repository owner makes it public once: on GitHub, **Packages** >
**chrysopoeia** > **Package settings** > **Change visibility** > **Public**.

The template's `Icon` and `TemplateURL` point at the `main` branch. Until the
template and `unraid/chrysopoeia.png` are on `main`, a template installed from
a branch shows no icon and cannot refresh itself; the container works the same.
Before the very first release there is also no `latest`: install the template
from the branch instead (replace `main` in the `wget` address of step 2 with
the branch name, e.g. `.../Project-Chrysopoeia/my-branch/unraid/chrysopoeia.xml`)
and set **Repository** to the `edge` image before clicking **Apply**.

## Troubleshooting

Start with the app itself:

- **Settings > Hardware** lists the processor, the memory and every GPU the
  container can see, with a setup tip giving the exact fix when something is
  missing, for example a GPU visible on the host but not passed to the
  container. Under **Details: encoders and ffmpeg**, each encoder shows
  *Works* or *Failed test* (with the error), from a short test encode. After
  changing the template, click **Check again** on that page, or restart the
  container.
- Each failed file in the **Queue** (under **History**) says why in a
  sentence; open it for the checks that ran and, under *Technical details*,
  the ffmpeg command and the end of ffmpeg's log. The **Log** at the bottom of
  that tab lists scans, warnings and problems.

Then the container log (Docker tab > Chrysopoeia icon > **Logs**). The first
lines list the version, the user it runs as, the transcode folder and every
GPU device it can see.

| Problem | Fix |
|---|---|
| Container will not start: *error gathering device information while adding custom device "/dev/dri"* | The template has a `/dev/dri` device, but the server has no `/dev/dri`. Install Intel GPU TOP or Radeon TOP and reboot, or remove the device: **Edit** the container and click **Remove** next to it. |
| Log warns that a device *belongs to the root group* | Chrysopoeia does not join the root group, for safety. In the Unraid terminal run `chgrp video /dev/dri/renderD128 && chmod g+rw /dev/dri/renderD128` (with the device named in the warning), then restart the container. To keep it after a reboot, add the same line to `/boot/config/go`. |
| NVIDIA card not used; log says *NVIDIA_VISIBLE_DEVICES is set but no NVIDIA GPU is visible* | Add `--runtime=nvidia` to Extra Parameters (Advanced View). After installing the Nvidia-Driver plugin, restart Docker once. |
| NVIDIA encoders show *Failed test* in Settings > Hardware | Check that the driver plugin shows your card, that `NVIDIA_VISIBLE_DEVICES` is `all` or the right UUID, and that another container is not holding all encode sessions. `docker exec Chrysopoeia nvidia-smi` should list the card. |
| Intel/AMD: no hardware encoders, `/dev/dri` present | Check that `renderD128` exists (`ls -l /dev/dri`). Settings > Hardware shows the exact error; permission errors mean the container was started with a custom `--user`: remove it and use PUID/PGID. |
| Log says *cannot write to /media* | See [PUID, PGID and permissions](#puid-pgid-and-permissions). |
| Log says */media is mounted read-only* | The Media path's **Access Mode** is *Read Only*. **Edit** the container, click **Edit** next to Media, set Access Mode to *Read/Write* and click **Apply**. Read-only is fine only when Settings > Output writes new files to a separate output folder. |
| Log says */config already holds other files but no Chrysopoeia database* | The Config path points at a folder that other apps use, such as `/mnt/user/appdata`. Chrysopoeia only adds its own files there and leaves the rest alone, but give it a folder of its own: **Edit** the container, set Config to `/mnt/user/appdata/chrysopoeia` and click **Apply**. |
| Log says *cannot write to /config* | The Config folder belongs to someone else and is not writable for PUID/PGID, and Chrysopoeia will not take over a folder that holds other data. Set PUID/PGID to the folder's owner, or set Config to a new folder such as `/mnt/user/appdata/chrysopoeia`, which Chrysopoeia then takes over. |
| Log says *No host folder is mounted at /config* | The Config path is empty, so settings and history would be lost on the next update. Set it to `/mnt/user/appdata/chrysopoeia`. |
| New files are not picked up | Folder watching sees changes made through `/mnt/user` shares. Files added directly to a disk (`/mnt/disk1/...`) are found by the periodic rescan (every 12 hours by default), or click **Scan now** on the library. |
| The server feels slow while converting | Lower *Files at once* in Settings > Processing, or turn on *When to convert* there so conversions run overnight. |
| Nothing converts at night / during the day as expected | *When to convert* uses the server's time zone. Unraid passes it automatically; check **Settings > Date and Time**. |
| The app says *Chrysopoeia doesn't answer to the address …* | You opened it through a domain name. Add that name to ALLOWED_HOSTS (see [Reverse proxy](#reverse-proxy-swag-nginx-proxy-manager-traefik)). |
| Through a reverse proxy the page opens, but saving says *This request came from another website, so Chrysopoeia refused it* | The proxy replaces the address the browser used. nginx: add `proxy_set_header Host $http_host;`; Apache: `ProxyPreserveHost On` (see [Reverse proxy](#reverse-proxy-swag-nginx-proxy-manager-traefik)). Opened directly (`http://<server IP>:8080`), this message means a page on another website really did try to change something. |
| MAX_JOBS or HW_ACCEL seem to be ignored | A number chosen in the app under *Files at once* wins over MAX_JOBS; choose *Automatic* there to use MAX_JOBS. HW_ACCEL is applied when its value changes, so a later choice in Settings > Hardware stays until you change HW_ACCEL again. |
| A job's ffmpeg log says `set_mempolicy: Operation not permitted` | Harmless: the HEVC (x265) encoder asks for a memory placement that Docker does not allow, and carries on normally. To silence it, add `--cap-add=SYS_NICE` to Extra Parameters. |
