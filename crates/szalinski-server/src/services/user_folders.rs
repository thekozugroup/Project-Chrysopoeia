//! "Your folders": the folders mounted into the container, as the folder
//! picker offers them first.
//!
//! In Docker (and so on Unraid) a person gives the container the folders it
//! may use: media, a work folder, an output folder, the settings folder.
//! Each shows up as a mount in `/proc/self/mountinfo`, a local list, so
//! finding them never touches a share. Everything else under `/` is the
//! container's own system (`/bin`, `/etc`, `/proc`, `/usr`, the app), which
//! nobody wants to browse for their videos: the picker puts the first kind
//! on top and sets the second apart.
//!
//! Outside a container there is no such split (a desktop's disks are all
//! "yours"), so nothing is listed and the picker lists folders as it
//! always did.

use std::path::{Path, PathBuf};

use serde::Serialize;
use szalinski_worker::slow_fs::MountPoint;

use crate::services::library_admin::{OwnFolders, library_folder_refusal};

/// The container's own folders: never "your folders", wherever something is
/// mounted in them (a bind-mounted `/etc/resolv.conf`, the GPU runtime's
/// libraries in `/usr`), and set apart in the picker.
const SYSTEM_ROOTS: [&str; 20] = [
    "/proc",
    "/sys",
    "/dev",
    "/run",
    "/etc",
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    // Empty marker folders newer Debian images keep next to the links above.
    "/bin.usr-is-merged",
    "/sbin.usr-is-merged",
    "/lib.usr-is-merged",
    "/lost+found",
    "/var",
    "/boot",
    "/root",
    "/app",
];

/// Filesystems that are the container's plumbing, never a folder of
/// someone's files, wherever they are mounted.
const PLUMBING_FILESYSTEMS: [&str; 20] = [
    "proc",
    "sysfs",
    "devpts",
    "devtmpfs",
    "cgroup",
    "cgroup2",
    "mqueue",
    "securityfs",
    "debugfs",
    "tracefs",
    "configfs",
    "fusectl",
    "pstore",
    "bpf",
    "binfmt_misc",
    "hugetlbfs",
    "autofs",
    "nsfs",
    "rpc_pipefs",
    "efivarfs",
];

/// A folder the person gave the container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UserFolder {
    /// The last part of its path (`media` for `/media`).
    pub name: String,
    pub path: String,
    /// Why it can't be a library (the settings folder, say), left out when
    /// it can: the picker leaves such a folder out while a library's folder
    /// is chosen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library_blocked: Option<String>,
}

/// The container's own folders (see [`SYSTEM_ROOTS`]), and the app's.
#[derive(Debug, Clone)]
pub struct SystemFolders {
    roots: Vec<PathBuf>,
}

impl SystemFolders {
    pub fn new(own: &OwnFolders) -> Self {
        let mut roots: Vec<PathBuf> = SYSTEM_ROOTS.iter().map(PathBuf::from).collect();
        for app in own.app_folders() {
            if !roots.iter().any(|r| app.starts_with(r)) {
                roots.push(app.to_path_buf());
            }
        }
        Self { roots }
    }

    /// Whether `path` is one of these folders or inside one.
    pub fn contains(&self, path: &Path) -> bool {
        self.roots.iter().any(|r| path.starts_with(r))
    }
}

/// Whether the mount is the container's plumbing, by what is mounted or
/// where.
fn is_plumbing(mount: &MountPoint, system: &SystemFolders) -> bool {
    mount.path == Path::new("/")
        || !mount.path.is_absolute()
        || system.contains(&mount.path)
        || PLUMBING_FILESYSTEMS.contains(&mount.identity.fstype.as_str())
}

/// The folders mounted into the container that the picker may show
/// (`within`: inside the browse roots), outermost first: of a mount and
/// the ones inside it, only the outer one, which holds the rest. Sorted by
/// path, so the answer doesn't depend on the order of the mount list.
pub fn user_folders(
    mounts: &[MountPoint],
    system: &SystemFolders,
    own: &OwnFolders,
    within: impl Fn(&Path) -> bool,
) -> Vec<UserFolder> {
    let mut paths: Vec<&Path> = mounts
        .iter()
        .filter(|m| !is_plumbing(m, system) && within(&m.path))
        .map(|m| m.path.as_path())
        .collect();
    // A path sorts right after the one it is inside, so a pass keeps the
    // outer ones.
    paths.sort();
    paths.dedup();
    let mut kept: Vec<&Path> = Vec::new();
    for path in paths {
        if !kept.iter().any(|outer| path.starts_with(outer)) {
            kept.push(path);
        }
    }
    let mut out: Vec<UserFolder> = kept
        .into_iter()
        .filter_map(|path| {
            Some(UserFolder {
                name: path.file_name()?.to_str()?.to_string(),
                path: path.to_str()?.to_string(),
                library_blocked: library_folder_refusal(path, own),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        a.path
            .to_lowercase()
            .cmp(&b.path.to_lowercase())
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use szalinski_worker::slow_fs::parse_mount_points;

    /// The mounts of the container Atlas runs on Unraid 7: the settings
    /// folder, a media share, the Optane work folder and an output folder
    /// (bind mounts of folders on the `shfs` user share and the cache pool),
    /// next to everything Docker, the GPU runtime and Unraid add.
    const UNRAID: &str = "\
2301 1892 0:48 / / rw,relatime master:1090 - overlay overlay rw,lowerdir=/var/lib/docker/overlay2/l/AB:/var/lib/docker/overlay2/l/CD,upperdir=/var/lib/docker/overlay2/f3a/diff,workdir=/var/lib/docker/overlay2/f3a/work
2302 2301 0:52 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw
2303 2301 0:53 / /dev rw,nosuid - tmpfs tmpfs rw,size=65536k,mode=755
2304 2303 0:54 / /dev/pts rw,nosuid,noexec,relatime - devpts devpts rw,gid=5,mode=620,ptmxmode=666
2305 2301 0:55 / /sys ro,nosuid,nodev,noexec,relatime - sysfs sysfs ro
2306 2305 0:25 / /sys/fs/cgroup ro,nosuid,nodev,noexec,relatime - cgroup2 cgroup rw
2307 2303 0:51 / /dev/mqueue rw,nosuid,nodev,noexec,relatime - mqueue mqueue rw
2308 2303 0:56 / /dev/shm rw,nosuid,nodev,noexec,relatime - tmpfs shm rw,size=65536k
2309 2301 0:41 /docker/containers/ab12/resolv.conf /etc/resolv.conf rw,relatime - btrfs /dev/loop2 rw,ssd,space_cache=v2,subvolid=5,subvol=/
2310 2301 0:41 /docker/containers/ab12/hostname /etc/hostname rw,relatime - btrfs /dev/loop2 rw,ssd,space_cache=v2,subvolid=5,subvol=/
2311 2301 0:41 /docker/containers/ab12/hosts /etc/hosts rw,relatime - btrfs /dev/loop2 rw,ssd,space_cache=v2,subvolid=5,subvol=/
2312 2301 0:47 /appdata/szalinski /config rw,noatime - fuse.shfs shfs rw,user_id=0,group_id=0,default_permissions,allow_other
2313 2301 0:47 /Media /media rw,noatime - fuse.shfs shfs rw,user_id=0,group_id=0,default_permissions,allow_other
2314 2301 0:62 /szalinski-temp /temp rw,noatime - xfs /dev/nvme0n1p1 rw,attr2,inode64,logbufs=8,logbsize=32k,noquota
2315 2301 0:47 /Transcoded /output rw,noatime - fuse.shfs shfs rw,user_id=0,group_id=0,default_permissions,allow_other
2316 2302 0:52 /bus /proc/bus ro,nosuid,nodev,noexec,relatime - proc proc rw
2317 2302 0:52 /fs /proc/fs ro,nosuid,nodev,noexec,relatime - proc proc rw
2318 2302 0:52 /sys /proc/sys ro,nosuid,nodev,noexec,relatime - proc proc rw
2319 2302 0:57 / /proc/acpi ro,relatime - tmpfs tmpfs ro
2320 2302 0:53 /null /proc/kcore rw,nosuid - tmpfs tmpfs rw,size=65536k,mode=755
2321 2305 0:58 / /sys/firmware ro,relatime - tmpfs tmpfs ro
2322 2301 0:41 /usr/bin/nvidia-smi /usr/bin/nvidia-smi ro,nosuid,nodev,relatime - ext4 /dev/sda1 ro
2323 2301 0:41 /usr/lib/libcuda.so.535.113.01 /usr/lib/x86_64-linux-gnu/libcuda.so.535.113.01 ro,nosuid,nodev,relatime - ext4 /dev/sda1 ro
2324 2301 0:59 / /run/nvidia-persistenced rw,nosuid,nodev,noexec,relatime - tmpfs tmpfs rw,size=1638400k,mode=755
2325 2302 0:60 / /proc/driver/nvidia rw,nosuid,nodev,noexec,relatime - tmpfs tmpfs rw,mode=555
";

    fn setup() -> (tempfile::TempDir, OwnFolders) {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let web = dir.path().join("web");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&web).unwrap();
        let own = OwnFolders::resolve(&data, &web);
        (dir, own)
    }

    fn found(text: &str, own: &OwnFolders, within: impl Fn(&Path) -> bool) -> Vec<UserFolder> {
        let mounts = parse_mount_points(text.as_bytes());
        user_folders(&mounts, &SystemFolders::new(own), own, within)
    }

    fn paths(folders: &[UserFolder]) -> Vec<&str> {
        folders.iter().map(|f| f.path.as_str()).collect()
    }

    #[test]
    fn an_unraid_containers_own_mounts_are_left_out_and_its_folders_kept() {
        let (_dir, own) = setup();
        let folders = found(UNRAID, &own, |_| true);
        assert_eq!(paths(&folders), ["/config", "/media", "/output", "/temp"]);
        assert_eq!(
            folders.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["config", "media", "output", "temp"]
        );
    }

    #[test]
    fn the_settings_folder_says_it_can_not_be_a_library_and_the_others_can() {
        let (_dir, own) = setup();
        let folders = found(UNRAID, &own, |_| true);
        let config = folders.iter().find(|f| f.path == "/config").unwrap();
        assert!(
            config
                .library_blocked
                .as_deref()
                .is_some_and(|r| r.contains("keeps its database and settings")),
            "{config:?}"
        );
        for other in folders.iter().filter(|f| f.path != "/config") {
            assert_eq!(other.library_blocked, None, "{other:?}");
        }
    }

    #[test]
    fn only_the_outer_one_of_nested_mounts_is_listed() {
        let (_dir, own) = setup();
        let text = "\
30 1 0:1 / / rw - overlay overlay rw
31 30 0:2 /Media /media rw - fuse.shfs shfs rw
32 31 8:17 / /media/Movies/USB\\040Drive rw - vfat /dev/sdb1 rw
33 31 0:3 / /media/Remote rw - nfs4 nas:/export rw
34 30 0:4 / /data rw - ext4 /dev/sdc1 rw
";
        let folders = found(text, &own, |_| true);
        assert_eq!(paths(&folders), ["/data", "/media"]);
    }

    #[test]
    fn mounts_outside_what_the_picker_may_show_are_left_out() {
        let (_dir, own) = setup();
        let folders = found(UNRAID, &own, |p| {
            p.starts_with("/media") || p.starts_with("/temp")
        });
        assert_eq!(paths(&folders), ["/media", "/temp"]);
    }

    #[test]
    fn plumbing_filesystems_are_left_out_wherever_they_are_mounted() {
        let (_dir, own) = setup();
        let text = "\
30 1 0:1 / / rw - overlay overlay rw
31 30 0:2 / /host/proc rw - proc proc rw
32 30 0:3 / /host/cgroup rw - cgroup2 cgroup rw
33 30 0:4 / /transcode rw - tmpfs tmpfs rw,size=8g
";
        // A tmpfs the person asked for (a RAM disk for work files) is theirs.
        assert_eq!(paths(&found(text, &own, |_| true)), ["/transcode"]);
    }

    #[test]
    fn the_apps_own_folders_are_never_listed() {
        let (dir, own) = setup();
        let web = dir.path().join("web").canonicalize().unwrap();
        let text = format!(
            "30 1 0:1 / / rw - overlay overlay rw\n\
             31 30 0:2 / {} rw - ext4 /dev/sda1 rw\n\
             32 30 0:3 / /app/web rw - ext4 /dev/sda1 rw\n\
             33 30 0:4 / /media rw - ext4 /dev/sda1 rw\n",
            web.display()
        );
        assert_eq!(paths(&found(&text, &own, |_| true)), ["/media"]);
    }

    #[test]
    fn a_container_with_nothing_mounted_lists_nothing() {
        let (_dir, own) = setup();
        let text = "30 1 0:1 / / rw - overlay overlay rw\n31 30 0:2 / /proc rw - proc proc rw\n";
        assert!(found(text, &own, |_| true).is_empty());
    }

    #[test]
    fn system_folders_include_the_container_and_the_app() {
        let (dir, own) = setup();
        let system = SystemFolders::new(&own);
        for p in [
            "/bin",
            "/etc/ssl",
            "/usr/local",
            "/proc",
            "/dev/shm",
            "/app",
            "/var/lib",
            "/bin.usr-is-merged",
            "/lost+found",
        ] {
            assert!(system.contains(Path::new(p)), "{p}");
        }
        let web = dir.path().join("web").canonicalize().unwrap();
        assert!(system.contains(&web.join("_next")));
        for p in [
            "/media",
            "/temp",
            "/config",
            "/mnt/user",
            "/home",
            "/binaries",
            "/application",
        ] {
            assert!(!system.contains(Path::new(p)), "{p}");
        }
    }
}
