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

> **Before the first release, or from a branch.** The template, its icon and
> the image `ghcr.io/thekozugroup/chrysopoeia:stable` (the template's
> Repository) exist only once the project has been merged to `main`, a first
> version tag (such as `v0.2.0`) has been pushed, its Release workflow has
> published the image, and the package has been made public. Until then the
> `wget` address above answers *404*, and Unraid's pull of `:stable` fails
> (*manifest unknown*, or *denied* while the package is private). Install from
> the branch instead: replace `main` in the `wget` address with the branch's
> name, and before clicking **Apply** in step 3 set **Repository** to
> `ghcr.io/thekozugroup/chrysopoeia:edge`. The details, including making the
> package public, are under [Trying a test build](#trying-a-test-build). Or
> [build the image yourself](../README.md#build-it-yourself) and set
> **Repository** to `chrysopoeia:local`.

## 3. Fill in the template

| Field | What to enter |
|---|---|
| Repository | Leave it at `ghcr.io/thekozugroup/chrysopoeia:stable`: the newest tagged release, which only changes when a new release is published. That is the choice for automatic updates. `:latest` follows every build of `main` and `:edge` is a test build; see [Updates](#updates-stable-latest-and-edge). |
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
   you are in: **Use “Movies”**. You can add more libraries later. The picker
   won't take `/`, `/config` or the app's own and system folders: the button
   is disabled and the picker says why. Choose the folder that holds your videos.
3. Choose a goal. *Balanced* (HEVC) is fast with a GPU and plays on most TVs;
   *Save space* (AV1) gives the smallest files; *Plays everywhere* (H.264)
   suits old devices; *Archive* keeps near-original quality. Audio follows the
   goal: *Save space* converts every track to Opus (*Plays everywhere* to
   AAC), keeping channels, language and flags but not lossless or Atmos
   sound, while *Balanced* and *Archive* copy the original audio unchanged.
   Each card shows how fast your hardware handles it, and the one that suits
   this machine is marked *Best fit*. You can rename the library here. Under
   *Plays everywhere* a file whose subtitles or attachments MP4 can't hold is
   skipped, with the reason shown, instead of losing them; see
   [Troubleshooting](#troubleshooting).
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

The converted file gets the new container's extension, so `Movie.mp4` becomes
`Movie.mkv` (and an `.mkv` becomes `.mp4` under *Plays everywhere*); a file
that is already in that container keeps its name. Plex, Jellyfin and Emby pick
this up at their next scan. Sonarr and Radarr see the old file as missing until
their next disk scan, or until you run *Refresh & Scan* on the series or
movie. In **History** and *Recently finished* the file shows its new name with
*Was Movie.mp4* beneath it, and the library's search finds it by either name.

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

## Keeping your originals

To leave your library untouched and write the converted files somewhere else,
the container needs a second, writable folder, because the template has only
Config, Media and Transcode cache:

1. On the Docker tab, **Edit** Chrysopoeia and click **Add another Path, Port,
   Variable, Label or Device** at the bottom. Set **Config Type** to *Path*,
   **Name** to `Converted files`, **Container Path** to `/output`, **Host
   Path** to a share for the results, e.g. `/mnt/user/converted/`, and **Access
   Mode** to *Read/Write*. Click **Add**, then **Apply**.
2. In the app, open **Settings > Output**, choose **Save to a separate
   folder**, click the **Output folder** field, pick `/` at the top of the
   folder browser, open `output` and click **Use “output”**.
3. Optional, and only after step 2: set the Media path's **Access Mode** to
   *Read Only*, so that nothing can change your originals at all. While
   Chrysopoeia is set to replace originals, a read-only Media path stops every
   conversion.

The new files mirror each library's folder structure inside `/output`, with
the goal's extension, but without the library's own folder name. Two libraries
that hold the same relative path therefore want the same output file. The
second is refused and nothing is overwritten: that file fails with *Another
library's converted file, from Kids, already uses the name "Frozen
(2013)/Frozen.mkv" in the output folder /output, so this file wasn't converted
and that file wasn't overwritten*, and the Overview lists it under *Finished
files can't be saved*. When your libraries are folders of one share (`Movies`,
`TV`), add that share as a single library instead: the output then keeps the
`Movies` and `TV` folders.

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
libraries again, with the same goal as before, and files that were already
converted are recognised as already efficient and skipped. Only history and
statistics are lost. Files that had been tried but kept as originals because
the result was not enough smaller are tried again, and converting them again
takes as long as the first time; a different goal converts everything again.
Your media is not part of this backup: keep a separate backup of anything you
cannot replace, since conversion replaces files.

## Updates: stable, latest and edge

**Docker** tab > **Check for Updates** > **apply update** next to Chrysopoeia.
Your settings and libraries are kept, and the database is brought up to date on
the first start. Files that were converting are stopped, their work files
removed, and they start again from the beginning; originals are never left
half-replaced. Unraid waits for the container to stop before replacing it, and
Chrysopoeia exits within a few seconds. To see which build is running, open
the bottom of **Settings** in the app (*About*) or the first lines of the
container log.

Which update you are offered depends on the tag at the end of **Repository**.
Unraid compares the image your container runs with the image currently behind
that tag, so a tag that rarely moves rarely offers an update.

| Repository ends in | A new image appears | What a daily update check does |
|---|---|---|
| `:stable` (the template's default) | Only when a new release is published (a `vX.Y.Z` tag that is not a prerelease). Never backwards. | Nothing on most days. On the day a release is published, the next check installs it. Only tested, tagged releases arrive. |
| `:latest` | After every merge to the `main` branch, released or not. | Installs every change that reached `main` since the last check, so the server runs ahead of the newest release. |
| `:1.2` | When a new 1.2.x release is published. | Installs fixes for 1.2, never a new minor version. |
| `:1` | When any new 1.x release is published. | Installs every new 1.x release, never 2.0. |
| `:1.2.3` | Never. | Never offers anything. To update, **Edit** the container and change the tag. |
| `:edge` | Whenever someone publishes a branch for testing. | May install a different, unmerged branch from one day to the next. For testing only. |

`:stable` is the choice for automatic updates. `:latest` is not a "latest
release": it is the newest build of `main`, which is usually ahead of the
newest release, and it changes whenever the project does. `:edge` is for trying
a branch (see [Trying a test build](#trying-a-test-build)).

An automatic update recreates the container the same way **apply update** does,
so a conversion that is running is stopped and starts again from the
beginning. Set the update time (Community Applications' auto-update, **Auto
Update Applications** in Settings) outside the hours when Chrysopoeia converts,
for example at night when *When to convert* is off, or after the schedule's
window ends.

### Switching an existing install from :latest to :stable

You keep the template the container was installed with: the Config, Media and
Transcode cache paths, the `/dev/dri` Device for an Intel or AMD GPU, the
variables, and Extra Parameters such as `--cpus`, `--memory` or
`--runtime=nvidia`. Unraid saves them for the container in
`/boot/config/plugins/dockerMan/templates-user/my-Chrysopoeia.xml`, and the
container's **Edit** page changes that file. Do not count on a change to the
template in the Apps store to switch a container that is already installed:
change **Repository** once by hand.

1. Check the version first. Open **Settings** in Chrysopoeia and read the
   *build* under *About*: `main-<commit>` means the container follows `latest`.
   Compare it with the newest release on the project's
   [Releases page](https://github.com/thekozugroup/Project-Chrysopoeia/releases)
   or in the [changelog](../CHANGELOG.md). `:stable` is the newest *release*,
   so it can be older than the `main` build you run. Going to an older build
   works only if it can still read your database: an older build refuses a
   newer database, stops with *This database was created by a newer version of
   Chrysopoeia* in the container log, and changes nothing. Then set
   **Repository** back to `:latest`. So if your build is newer than the newest
   release, wait for the next release before you switch.
2. On the **Docker** tab, click the Chrysopoeia icon, then **Edit**. Do not
   remove the container or install it again from **Apps**: that starts from the
   template's defaults and you would add the paths, the GPU device and the
   limits again.
3. Change **Repository** from `ghcr.io/thekozugroup/chrysopoeia:latest` to
   `ghcr.io/thekozugroup/chrysopoeia:stable`. Leave every other field as it is.
   (If the pull then fails with *manifest unknown*, the first release has not
   been published yet: put `:latest` back.)
4. Click **Apply**. Unraid pulls the image and recreates the container with
   all the paths, the device, the variables and the limits you already had. The
   libraries, settings and history are in the Config folder and are kept.
5. Check the result: the first lines of the container log and *About* in
   Settings show the release number as the build (for example `0.3.0`), not
   `main-<commit>`, and **Settings > Hardware** still lists your GPU.

From then on, Check for Updates and the automatic updater offer releases only.
To follow `main` again, repeat the steps with `:latest`.

To go back to an older version, set **Repository** to that version (for
example `ghcr.io/thekozugroup/chrysopoeia:0.2.0`, which never changes) and
restore a backup of the config folder taken before the upgrade: a database
written by a newer version is refused by an older one with a plain message.

## Trying a test build

A branch that is not merged yet is published as
`ghcr.io/thekozugroup/chrysopoeia:edge` when someone opens **Actions >
Release > Run workflow** on GitHub and picks that branch. The workflow builds
the image, runs its tests and pushes it for amd64 and arm64; `latest` and
`stable` are not touched. To use it, **Edit** the container, set **Repository**
to `ghcr.io/thekozugroup/chrysopoeia:edge` and click **Apply**. Afterwards set
**Repository** back to `ghcr.io/thekozugroup/chrysopoeia:stable` (releases only),
or, once the branch is merged to `main` and you want the change at once, to
`ghcr.io/thekozugroup/chrysopoeia:latest`, which then carries it. Your settings
and libraries are kept either way. Switch back soon: `:edge` changes whenever
anyone publishes a branch, and a daily auto-update would follow it.

If that run published the very first image, the package on GitHub is still
private and Unraid's pull is refused (*denied* or *unauthorized* in the pull
log). The repository owner makes it public once: on GitHub, **Packages** >
**chrysopoeia** > **Package settings** > **Change visibility** > **Public**.

The template's `Icon` and `TemplateURL` point at the `main` branch. Until the
template and `unraid/chrysopoeia.png` are on `main`, a template installed from
a branch shows no icon and cannot refresh itself; the container works the same.
Before the very first release there is also no `stable` (and no `latest` until
`main` has been built): install the template from the branch instead (replace
`main` in the `wget` address of step 2 with the branch name, e.g.
`.../Project-Chrysopoeia/my-branch/unraid/chrysopoeia.xml`) and set
**Repository** to the `edge` image before clicking **Apply**.

Without waiting for either, you can
[build the image yourself](../README.md#build-it-yourself) (10 to 15 minutes)
and set **Repository** to `chrysopoeia:local`.

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
| Docker error when applying: *unknown or invalid runtime name: nvidia* | `--runtime=nvidia` is in Extra Parameters, but Docker does not have the NVIDIA runtime yet, so the container is not even created. Install the **Nvidia-Driver** plugin, wait until it says it is done, then restart Docker (**Settings > Docker > Enable Docker: No > Apply**, then **Yes > Apply**) or reboot. To use the CPU instead, remove `--runtime=nvidia` from Extra Parameters. |
| Intel/AMD: no hardware encoders, `/dev/dri` present | Check that `renderD128` exists (`ls -l /dev/dri`). Settings > Hardware shows the exact error; permission errors mean the container was started with a custom `--user`: remove it and use PUID/PGID. |
| A file fails with *Chrysopoeia doesn't have permission to write in the work folder*, or the Overview says *The work folder can't be used* or *The disk is full* | The **Transcode cache** folder (without one, the folder of each original) is not writable for PUID/PGID, or has no room: it needs space for the largest file you convert, times *Files at once*. Restarting the container makes its top folder belong to PUID/PGID again; for what is inside it, run `chown -R 99:100 /mnt/cache/chrysopoeia-temp` in the Unraid terminal (with your own path), and free up space if the disk is full. Then click **Try again** on the Overview, which queues every file that waited. |
| Log says *cannot write to /media* | See [PUID, PGID and permissions](#puid-pgid-and-permissions). |
| Log says */media is mounted read-only* | The Media path's **Access Mode** is *Read Only*. **Edit** the container, click **Edit** next to Media, set Access Mode to *Read/Write* and click **Apply**. Read-only is fine only when Settings > Output writes new files to a separate output folder (see [Keeping your originals](#keeping-your-originals)). |
| Log says */config already holds other files but no Chrysopoeia database* | The Config path points at a folder that other apps use, such as `/mnt/user/appdata`. Chrysopoeia only adds its own files there and leaves the rest alone, but give it a folder of its own: **Edit** the container, set Config to `/mnt/user/appdata/chrysopoeia` and click **Apply**. |
| Log says *cannot write to /config* | The Config folder belongs to someone else and is not writable for PUID/PGID, and Chrysopoeia will not take over a folder that holds other data. Set PUID/PGID to the folder's owner, or set Config to a new folder such as `/mnt/user/appdata/chrysopoeia`, which Chrysopoeia then takes over. |
| Log says *No host folder is mounted at /config* | The Config path is empty, so settings and history would be lost on the next update. Set it to `/mnt/user/appdata/chrysopoeia`. |
| Log says *Another Chrysopoeia is already using the data folder /config* | A second Chrysopoeia container uses the same Config path (or the first is still running). Nothing was changed. Stop the other one, or **Edit** this one and give it a Config folder of its own, for example `/mnt/user/appdata/chrysopoeia2`. |
| Log says *The disk that holds the data folder (/config) is full* | The disk behind the Config path (your cache pool or an array disk) has no room for the database. Free some space on it, then start the container again. |
| A library is listed under **Needs your attention** as *The folder … isn't responding*, and nothing in it converts | The share or drive behind that folder stopped answering, for example a remote SMB or NFS share that went away, or an Unassigned Devices drive that is stuck. Whenever that happens, even in the middle of a job, the job goes back in the queue and nothing is marked failed; the other libraries keep converting, and **Cancel** and *Stop now* still answer within seconds. Fix the connection or the mount on the Unraid side: when the folder answers, the files start over by themselves. A new file that was being put in place is finished or undone, never left half-replaced. Chrysopoeia exits within seconds when you stop the container, but Linux can't kill a process that is waiting on a hung mount, so Docker may keep showing it as running (and the stop may fail with *did not receive an exit event*) until the share answers or its mount is fixed. |
| A file is skipped with *MP4 can't hold this file's …, so it was left unchanged* | The goal writes MP4 (*Plays everywhere*) and the file has picture-based subtitles (PGS, VobSub), styled ASS/SSA subtitles, fonts or other attached files, or a cover image MP4 can't keep, so replacing the original would lose them. Choose an MKV goal (*Balanced*, *Save space* or *Archive*), which keeps everything; save converted files to a separate folder ([Keeping your originals](#keeping-your-originals)); or open the file and click **Convert anyway** to convert it without them. |
| A file is skipped with *This file has another hard link* | A torrent that is still seeding, or another hard link, shares the file, so replacing it would use more space instead of saving it. Remove the other link, or click **Convert anyway**; the job then reads *Converted, no space freed*. |
| Choosing a folder says *The whole server can't be a library*, or that */config* or a system folder can't be one | A library can't be `/`, `/config`, `/app`, `/proc`, `/sys` or `/dev`, or a folder inside one of them. Choose the folder that holds your videos, inside `/media`. |
| With an output folder, a file fails with *Another library's converted file, from Kids, already uses the name …* | Two libraries hold the same relative path, so both would write the same file in the output folder. The second is refused and nothing is overwritten. Rename one of the two files, or add the share above both folders as one library (see [Keeping your originals](#keeping-your-originals)). |
| New files are not picked up | Folder watching sees changes made through `/mnt/user` shares. Files added directly to a disk (`/mnt/disk1/...`) are found by the periodic rescan (every 12 hours by default), or open the **⋯** menu at the top right of the library's page and choose **Scan now**. |
| The server feels slow while converting | Lower *Files at once* in Settings > Processing, or turn on *When to convert* there so conversions run overnight. |
| Nothing converts at night / during the day as expected | *When to convert* uses the server's time zone. Unraid passes it automatically; check **Settings > Date and Time**. |
| The app says *Chrysopoeia doesn't answer to the address …* | You opened it through a name that is not listed. The screen shows the line to use: add that name to ALLOWED_HOSTS and restart (see [Reverse proxy](#reverse-proxy-swag-nginx-proxy-manager-traefik)). Behind nginx, it also needs `proxy_set_header Host $http_host;`. |
| Through a reverse proxy the page opens, but saving says *This request came from another website, so Chrysopoeia refused it* | The proxy replaces the address the browser used. nginx: add `proxy_set_header Host $http_host;`; Apache: `ProxyPreserveHost On` (see [Reverse proxy](#reverse-proxy-swag-nginx-proxy-manager-traefik)). Opened directly (`http://<server IP>:8080`), this message means a page on another website really did try to change something. |
| MAX_JOBS or HW_ACCEL seem to be ignored | A number chosen in the app under *Files at once* wins over MAX_JOBS; choose *Automatic* there to use MAX_JOBS. HW_ACCEL is applied when its value changes, so a later choice in Settings > Hardware stays until you change HW_ACCEL again. |
| A job's ffmpeg log says `set_mempolicy: Operation not permitted` | Harmless: the HEVC (x265) encoder asks for a memory placement that Docker does not allow, and carries on normally. To silence it, add `--cap-add=SYS_NICE` to Extra Parameters. |
