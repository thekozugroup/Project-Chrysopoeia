//! Walking a library folder: which files are media, which are ignored, and
//! which are Chrysopoeia's own leftovers.
//!
//! Walking never probes: it only lists names and reads file metadata, so a
//! library of 100 000 files is listed in seconds even on spinning disks.
//!
//! # Ignore patterns
//!
//! Patterns are globs matched against the path *relative to the library
//! root*, with `/` as the separator and ignoring letter case:
//!
//! - `*` and `?` never cross a `/`; `**` matches any number of folders
//!   (including none), so `**/.*` matches hidden files and folders at any
//!   depth and `**/Extras/**` matches everything inside any `Extras` folder.
//! - A pattern without a `/` matches a name at any depth: `Extras` is the
//!   same as `**/Extras`, and `*.part` the same as `**/*.part`.
//! - A leading `/` anchors the pattern to the library root: `/Downloads/**`
//!   only matches the top-level `Downloads` folder. A trailing `/` is ignored.
//! - A pattern that matches a folder excludes everything inside it, and the
//!   folder is not even read (so large ignored trees cost nothing).
//!
//! Chrysopoeia's temporary and backup files are never listed as media, even
//! when no pattern hides them; they are reported separately so the server can
//! recover or clean them up after a crash.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;

use anyhow::{anyhow, bail};
use chrono::{DateTime, Utc};
use chrysopoeia_core::paths;
use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use walkdir::WalkDir;

use crate::{DiscoveredFile, ScanOptions, WalkResult};

/// Extensions of video containers (and raw video streams) ffmpeg can read.
/// Compared ignoring case.
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mk3d", "mp4", "m4v", "mov", "qt", "avi", "divx", "xvid", "wmv", "asf", "flv", "f4v",
    "webm", "ts", "m2ts", "mts", "m2t", "tp", "trp", "mpg", "mpeg", "mpe", "m1v", "m2v", "mpv",
    "m2p", "vob", "vro", "evo", "ogv", "ogm", "3gp", "3gpp", "3g2", "rm", "rmvb", "mxf", "dv",
    "nut", "y4m", "h264", "h265", "hevc", "264", "265", "ivf", "obu", "wtv", "dvr-ms", "bik",
    "amv", "mod", "tod", "mjpeg", "mjpg",
];

/// Extensions of audio containers. They are listed so the library is
/// complete; the worker leaves audio-only files as they are.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mka", "flac", "mp3", "mp2", "mpa", "m4a", "m4b", "aac", "ogg", "oga", "opus", "spx", "wav",
    "wma", "ape", "wv", "tta", "mpc", "caf", "dsf", "aiff", "aif", "ac3", "eac3", "dts", "thd",
    "mlp",
];

/// Extensions of files that commonly sit next to media (subtitles, artwork,
/// metadata, partial downloads). Used by the watcher to tell a removed
/// sidecar file from a removed folder when the platform does not say which
/// it was.
pub(crate) const SIDECAR_EXTENSIONS: &[&str] = &[
    "srt",
    "ass",
    "ssa",
    "sub",
    "idx",
    "sup",
    "vtt",
    "smi",
    "sami",
    "ttml",
    "dfxp",
    "lrc",
    "txt",
    "nfo",
    "xml",
    "json",
    "yml",
    "yaml",
    "jpg",
    "jpeg",
    "png",
    "gif",
    "bmp",
    "webp",
    "tbn",
    "tif",
    "tiff",
    "heic",
    "svg",
    "ico",
    "log",
    "db",
    "ini",
    "url",
    "lnk",
    "torrent",
    "part",
    "partial",
    "tmp",
    "temp",
    "crdownload",
    "!qb",
    "md5",
    "sfv",
    "par2",
    "rar",
    "zip",
    "7z",
    "pdf",
    "html",
    "htm",
    "ds_store",
];

/// Whether `path` names a media file: its extension is in
/// [`VIDEO_EXTENSIONS`] or [`AUDIO_EXTENSIONS`] and it is not one of
/// Chrysopoeia's temporary or backup files.
pub(crate) fn is_media(path: &Path) -> bool {
    (has_extension_in(path, VIDEO_EXTENSIONS) || has_extension_in(path, AUDIO_EXTENSIONS))
        && !is_artifact_path(path)
}

/// Whether the file name of `path` marks it as a Chrysopoeia temporary or
/// backup file.
pub(crate) fn is_artifact_path(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| paths::is_artifact(&name.to_string_lossy()))
}

/// Whether the extension of `path` is one of `list`, ignoring case.
pub(crate) fn has_extension_in(path: &Path, list: &[&str]) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| list.iter().any(|known| known.eq_ignore_ascii_case(ext)))
}

/// Compiled ignore patterns (see the [module documentation](self) for the
/// matching rules).
#[derive(Debug, Clone)]
pub struct IgnoreRules {
    set: GlobSet,
    invalid: Vec<String>,
}

impl Default for IgnoreRules {
    fn default() -> Self {
        Self {
            set: GlobSet::empty(),
            invalid: Vec::new(),
        }
    }
}

impl IgnoreRules {
    /// Compile `patterns`. Blank patterns are skipped; invalid ones are left
    /// out and described by [`IgnoreRules::invalid_patterns`], so one typo
    /// never stops a scan.
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Self {
        let mut builder = GlobSetBuilder::new();
        let mut invalid = Vec::new();
        for pattern in patterns {
            let pattern = pattern.as_ref().trim();
            if pattern.is_empty() {
                continue;
            }
            match compile_pattern(pattern) {
                Ok(glob) => {
                    builder.add(glob);
                }
                Err(problem) => invalid.push(problem),
            }
        }
        let set = builder.build().unwrap_or_else(|error| {
            invalid.push(format!(
                "The ignore patterns could not be combined ({error}), so none were used"
            ));
            GlobSet::empty()
        });
        Self { set, invalid }
    }

    /// Plain-language descriptions of the patterns that were not valid.
    pub fn invalid_patterns(&self) -> &[String] {
        &self.invalid
    }

    /// True when no pattern is in effect.
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// Whether a file at `relative` (relative to the library root) is ignored.
    pub fn is_ignored(&self, relative: &Path) -> bool {
        !self.set.is_empty() && self.set.is_match(relative)
    }

    /// Whether a folder at `relative` (relative to the library root) is
    /// ignored, including everything inside it.
    pub fn is_ignored_dir(&self, relative: &Path) -> bool {
        if self.set.is_empty() || relative.as_os_str().is_empty() {
            return false;
        }
        if self.set.is_match(relative) {
            return true;
        }
        // `Extras/**` matches every child of `Extras` but not `Extras`
        // itself. Matching `Extras/` means the pattern already covers every
        // direct child, so the whole folder can be skipped.
        let mut with_slash = OsString::from(relative.as_os_str());
        with_slash.push("/");
        self.set.is_match(Path::new(&with_slash))
    }

    /// Whether a file at `relative` is excluded, either by a pattern matching
    /// it or because one of the folders it is in is ignored. This is what a
    /// walk would decide, for a single path (e.g. from a watch event).
    pub fn excludes_file(&self, relative: &Path) -> bool {
        !self.set.is_empty() && (self.is_ignored(relative) || self.excludes_parents(relative))
    }

    /// Whether a folder at `relative` is excluded, by a pattern matching it or
    /// one of the folders it is in.
    pub fn excludes_folder(&self, relative: &Path) -> bool {
        !self.set.is_empty() && (self.is_ignored_dir(relative) || self.excludes_parents(relative))
    }

    fn excludes_parents(&self, relative: &Path) -> bool {
        relative
            .ancestors()
            .skip(1)
            .take_while(|dir| !dir.as_os_str().is_empty())
            .any(|dir| self.is_ignored_dir(dir))
    }
}

/// Check one ignore pattern, returning a plain-language problem when it is
/// not valid. Useful for validating settings before saving them.
pub fn validate_ignore_pattern(pattern: &str) -> Result<(), String> {
    compile_pattern(pattern.trim()).map(drop)
}

fn compile_pattern(pattern: &str) -> Result<Glob, String> {
    let normalized = normalize_pattern(pattern);
    if normalized.is_empty() {
        return Err(format!(
            "The ignore pattern \"{pattern}\" does not match anything, so it was not used"
        ));
    }
    GlobBuilder::new(&normalized)
        .literal_separator(true)
        .case_insensitive(true)
        .build()
        .map_err(|error| {
            format!(
                "The ignore pattern \"{pattern}\" is not valid ({}), so it was not used",
                error.kind()
            )
        })
}

/// Apply the anchoring rules described in the module documentation.
fn normalize_pattern(pattern: &str) -> String {
    if let Some(anchored) = pattern.strip_prefix('/') {
        return anchored.trim_end_matches('/').to_string();
    }
    let trimmed = pattern.trim_end_matches('/');
    if trimmed.contains('/') {
        trimmed.to_string()
    } else if trimmed.is_empty() {
        String::new()
    } else {
        format!("**/{trimmed}")
    }
}

/// Implementation of [`crate::walk_library`].
pub(crate) fn walk(root: &Path, opts: &ScanOptions) -> anyhow::Result<WalkResult> {
    check_root(root)?;
    let rules = IgnoreRules::new(&opts.ignore_patterns);
    let mut result = WalkResult::default();
    for problem in rules.invalid_patterns() {
        result.errors.push((root.to_path_buf(), problem.clone()));
    }

    let mut entries = WalkDir::new(root)
        .follow_links(opts.follow_links)
        .into_iter();
    while let Some(entry) = entries.next() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                if error.depth() == 0 {
                    // The root became unreadable between the check and the walk.
                    bail!(
                        "The folder {} could not be read: {}.",
                        root.display(),
                        describe_walk_error(&error)
                    );
                }
                let path = error.path().unwrap_or(root).to_path_buf();
                result.errors.push((path, describe_walk_error(&error)));
                continue;
            }
        };
        if entry.depth() == 0 {
            continue;
        }
        let path = entry.path();
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let file_type = entry.file_type();

        if file_type.is_dir() {
            if rules.is_ignored_dir(relative) {
                entries.skip_current_dir();
            } else if path.to_str().is_none() {
                // Paths are stored as text, so nothing below can be tracked.
                result.errors.push((
                    path.to_path_buf(),
                    "Folder name is not valid UTF-8, so its contents were skipped. Renaming the \
                     folder fixes this"
                        .to_string(),
                ));
                entries.skip_current_dir();
            }
            continue;
        }
        // Symlinks (when not followed), sockets, pipes and devices.
        if !file_type.is_file() {
            continue;
        }
        if is_artifact_path(path) {
            result.artifacts.push(path.to_path_buf());
            continue;
        }
        if !is_media(path) || rules.is_ignored(relative) {
            continue;
        }
        if path.to_str().is_none() {
            result.errors.push((
                path.to_path_buf(),
                "File name is not valid UTF-8, so it was skipped. Renaming the file fixes this"
                    .to_string(),
            ));
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                result
                    .errors
                    .push((path.to_path_buf(), describe_walk_error(&error)));
                continue;
            }
        };
        let size = metadata.len();
        if size < opts.min_size_bytes {
            continue;
        }
        let modified = metadata
            .modified()
            .map(DateTime::<Utc>::from)
            .unwrap_or(DateTime::UNIX_EPOCH);
        result.files.push(DiscoveredFile {
            path: path.to_path_buf(),
            size,
            modified,
        });
    }

    result.files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    result.artifacts.sort_unstable();
    result.errors.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(result)
}

/// Fail with a plain-language message unless `root` is a readable folder.
fn check_root(root: &Path) -> anyhow::Result<()> {
    let shown = root.display();
    let metadata = std::fs::metadata(root).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => anyhow!(
            "The folder {shown} does not exist. Check the path, and when running in Docker that \
             the folder is mounted into the container."
        ),
        io::ErrorKind::PermissionDenied => {
            anyhow!("Chrysopoeia does not have permission to open the folder {shown}.")
        }
        _ => anyhow!("The folder {shown} could not be opened ({error})."),
    })?;
    if !metadata.is_dir() {
        bail!("{shown} is a file, not a folder.");
    }
    std::fs::read_dir(root).map_err(|error| match error.kind() {
        io::ErrorKind::PermissionDenied => anyhow!(
            "Chrysopoeia does not have permission to read the folder {shown}. Check the folder's \
             owner and permissions (PUID/PGID in Docker)."
        ),
        _ => anyhow!("The folder {shown} could not be read ({error})."),
    })?;
    Ok(())
}

/// Describe a per-entry walk error in plain language.
fn describe_walk_error(error: &walkdir::Error) -> String {
    if let Some(ancestor) = error.loop_ancestor() {
        return format!(
            "This linked folder points back to {}, so it was skipped to avoid an endless loop",
            ancestor.display()
        );
    }
    let Some(io_error) = error.io_error() else {
        return "Could not be read".to_string();
    };
    match io_error.kind() {
        io::ErrorKind::PermissionDenied => {
            "Chrysopoeia does not have permission to read this".to_string()
        }
        io::ErrorKind::NotFound => {
            let is_broken_link = error.path().is_some_and(|path| {
                std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
            });
            if is_broken_link {
                "This is a link to something that no longer exists".to_string()
            } else {
                "This disappeared while the library was being scanned".to_string()
            }
        }
        _ => format!("Could not be read ({io_error})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrysopoeia_core::Settings;
    use std::fs;

    fn touch(path: &Path, bytes: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, vec![0u8; bytes]).unwrap();
    }

    fn relative_files(root: &Path, result: &WalkResult) -> Vec<String> {
        result
            .files
            .iter()
            .map(|f| {
                f.path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    fn default_options() -> ScanOptions {
        ScanOptions {
            ignore_patterns: Settings::default().ignore_patterns,
            ..ScanOptions::default()
        }
    }

    #[test]
    fn media_extensions() {
        for name in [
            "a.mkv",
            "A.MKV",
            "dir/b.Mp4",
            "c.m2ts",
            "d.dvr-ms",
            "e.ts",
            "f.264",
            "g.flac",
            "h.M4B",
            "i.webm",
            "j.vob",
            "k.rmvb",
        ] {
            assert!(is_media(Path::new(name)), "{name} should be media");
        }
        for name in [
            "a.srt",
            "a.ass",
            "a.sub",
            "a.idx",
            "a.sup",
            "a.nfo",
            "a.jpg",
            "a.png",
            "a.txt",
            "a.xml",
            "noext",
            ".mkv",
            "movie.mkv.part",
            ".Movie.chrysopoeia-01234567.tmp.mkv",
            ".Movie.mkv.chrysopoeia-01234567.bak",
        ] {
            assert!(!is_media(Path::new(name)), "{name} should not be media");
        }
    }

    #[test]
    fn media_and_sidecar_lists_do_not_overlap() {
        for ext in SIDECAR_EXTENSIONS {
            assert!(!VIDEO_EXTENSIONS.contains(ext) && !AUDIO_EXTENSIONS.contains(ext));
        }
    }

    #[test]
    fn walk_lists_media_sorted_with_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("b/Movie B.mkv"), 10);
        touch(&root.join("a/Movie A.MP4"), 20);
        touch(&root.join("a b/Movie C.avi"), 30);
        touch(&root.join("a/Movie A.srt"), 5);
        touch(&root.join("a/poster.jpg"), 5);
        touch(&root.join("a/movie.nfo"), 5);
        touch(&root.join("Music/Song.flac"), 7);

        let before = Utc::now() - chrono::Duration::seconds(5);
        let result = walk(root, &ScanOptions::default()).unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(result.artifacts.is_empty());
        assert_eq!(
            relative_files(root, &result),
            [
                "Music/Song.flac",
                "a/Movie A.MP4",
                "a b/Movie C.avi",
                "b/Movie B.mkv"
            ]
        );
        let movie_a = &result.files[1];
        assert_eq!(movie_a.size, 20);
        assert!(movie_a.modified >= before && movie_a.modified <= Utc::now());
    }

    #[test]
    fn default_patterns_hide_hidden_and_nas_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Movies/Keep.mkv"), 1);
        touch(&root.join("Movies/.hidden.mkv"), 1);
        touch(&root.join(".Trash-1000/files/Deleted.mkv"), 1);
        touch(&root.join("Movies/.cache/Cached.mkv"), 1);
        touch(&root.join("Movies/@eaDir/Keep.mkv/SYNOVIDEO.mkv"), 1);
        touch(&root.join("#recycle/Old.mkv"), 1);
        touch(&root.join("Movies/Sub/#recycle/Old.mkv"), 1);
        touch(&root.join("Movies/Download.mkv.partial~"), 1);
        // Hidden only relative to a hidden root must still be found.
        let hidden_root = root.join(".library");
        touch(&hidden_root.join("Show/Episode.mkv"), 1);

        let result = walk(root, &default_options()).unwrap();
        assert_eq!(relative_files(root, &result), ["Movies/Keep.mkv"]);

        let result = walk(&hidden_root, &default_options()).unwrap();
        assert_eq!(relative_files(&hidden_root, &result), ["Show/Episode.mkv"]);
    }

    #[test]
    fn custom_patterns_follow_documented_rules() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Movies/A/A.mkv"), 1);
        touch(&root.join("Movies/A/Extras/Making Of.mkv"), 1);
        touch(&root.join("Movies/B/extras/Deleted Scene.mkv"), 1);
        touch(&root.join("Movies/B/B-sample.mkv"), 1);
        touch(&root.join("Movies/B/B.mkv"), 1);
        touch(&root.join("Downloads/Incomplete.mkv"), 1);
        touch(&root.join("TV/Downloads/Episode.mkv"), 1);
        touch(&root.join("TV/Show/Episode.m2ts"), 1);

        let opts = ScanOptions {
            ignore_patterns: vec![
                "**/Extras/**".into(), // case-insensitive
                "*-sample.*".into(),   // no slash: any depth
                "/Downloads/".into(),  // anchored to the root
                "TV/**/*.m2ts".into(), // relative path with slashes
                "  ".into(),           // blank: skipped
            ],
            ..ScanOptions::default()
        };
        let result = walk(root, &opts).unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(
            relative_files(root, &result),
            [
                "Movies/A/A.mkv",
                "Movies/B/B.mkv",
                "TV/Downloads/Episode.mkv"
            ]
        );
    }

    #[test]
    fn ignore_rules_match_folders_and_files() {
        let rules = IgnoreRules::new(&["**/@eaDir/**", "Samples", "**/.*"]);
        assert!(rules.is_ignored_dir(Path::new("x/@eaDir")));
        assert!(rules.is_ignored_dir(Path::new("@eaDir")));
        assert!(rules.is_ignored_dir(Path::new("a/b/samples")));
        assert!(rules.is_ignored_dir(Path::new("a/.git")));
        assert!(!rules.is_ignored_dir(Path::new("Movies")));
        assert!(!rules.is_ignored_dir(Path::new("Mr. Robot")));
        assert!(!rules.is_ignored_dir(Path::new("")));
        assert!(rules.is_ignored(Path::new("a/.b.mkv")));
        assert!(!rules.is_ignored(Path::new("a/b.mkv")));
        // A file inside an ignored folder is excluded even though the folder
        // pattern does not match the file itself.
        assert!(!rules.is_ignored(Path::new("x/Samples/s.mkv")));
        assert!(rules.excludes_file(Path::new("x/Samples/s.mkv")));
        assert!(rules.excludes_file(Path::new(".hidden/deep/s.mkv")));
        assert!(!rules.excludes_file(Path::new("Movies/Movie/Movie.mkv")));
        assert!(rules.excludes_folder(Path::new("x/Samples/deeper")));
        assert!(!rules.excludes_folder(Path::new("Movies/Movie")));
        assert!(IgnoreRules::default().is_empty());
        assert!(!IgnoreRules::default().is_ignored(Path::new("a")));
    }

    #[test]
    fn invalid_patterns_are_reported_not_fatal() {
        assert!(validate_ignore_pattern("**/Extras/**").is_ok());
        let problem = validate_ignore_pattern("Movies/[abc").unwrap_err();
        assert!(problem.contains("Movies/[abc"), "{problem}");
        assert!(validate_ignore_pattern("/").is_err());

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Movies/A.mkv"), 1);
        touch(&root.join("Movies/Extras/B.mkv"), 1);
        let opts = ScanOptions {
            ignore_patterns: vec!["Movies/[abc".into(), "Extras".into()],
            ..ScanOptions::default()
        };
        let result = walk(root, &opts).unwrap();
        assert_eq!(relative_files(root, &result), ["Movies/A.mkv"]);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].0, root);
        assert!(result.errors[0].1.contains("not valid"));
    }

    #[test]
    fn artifacts_are_collected_not_listed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let id = uuid_for_test();
        let tmp = root
            .join("Movies")
            .join(paths::temp_file_name("Movie", id, "mkv"));
        let bak = root
            .join("Movies")
            .join(paths::backup_file_name("Movie.avi", id));
        touch(&tmp, 3);
        touch(&bak, 3);
        touch(&root.join("Movies/Movie.avi"), 3);

        // Artifacts are hidden files, but are still reported with the default
        // patterns and regardless of the minimum size.
        let opts = ScanOptions {
            min_size_bytes: 100,
            ..default_options()
        };
        let result = walk(root, &opts).unwrap();
        assert!(result.files.is_empty());
        let mut expected = vec![tmp, bak];
        expected.sort();
        assert_eq!(result.artifacts, expected);
    }

    fn uuid_for_test() -> uuid::Uuid {
        uuid::Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef)
    }

    #[test]
    fn min_size_skips_small_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("sample.mkv"), 99);
        touch(&root.join("exact.mkv"), 100);
        touch(&root.join("movie.mkv"), 1000);
        let opts = ScanOptions {
            min_size_bytes: 100,
            ..ScanOptions::default()
        };
        let result = walk(root, &opts).unwrap();
        assert_eq!(relative_files(root, &result), ["exact.mkv", "movie.mkv"]);
    }

    #[test]
    fn root_errors_are_plain_sentences() {
        let dir = tempfile::tempdir().unwrap();
        let missing = walk(&dir.path().join("nope"), &ScanOptions::default()).unwrap_err();
        assert!(missing.to_string().contains("does not exist"), "{missing}");

        let file = dir.path().join("file.mkv");
        touch(&file, 1);
        let not_dir = walk(&file, &ScanOptions::default()).unwrap_err();
        assert!(
            not_dir.to_string().contains("is a file, not a folder"),
            "{not_dir}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loops_are_reported_and_survived() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Movies/A.mkv"), 1);
        symlink(root.join("Movies"), root.join("Movies/loop")).unwrap();
        symlink(root.join("Movies/A.mkv"), root.join("Linked.mkv")).unwrap();
        symlink(root.join("gone.mkv"), root.join("Broken.mkv")).unwrap();

        let followed = walk(
            root,
            &ScanOptions {
                follow_links: true,
                ..ScanOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            relative_files(root, &followed),
            ["Linked.mkv", "Movies/A.mkv"]
        );
        let messages: Vec<&str> = followed.errors.iter().map(|e| e.1.as_str()).collect();
        assert!(
            messages.iter().any(|m| m.contains("endless loop")),
            "{messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("no longer exists")),
            "{messages:?}"
        );

        let not_followed = walk(root, &ScanOptions::default()).unwrap();
        assert_eq!(relative_files(root, &not_followed), ["Movies/A.mkv"]);
        assert!(not_followed.errors.is_empty(), "{:?}", not_followed.errors);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn non_utf8_names_are_reported_and_skipped() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Good.mkv"), 1);
        let bad_file = root.join(OsStr::from_bytes(b"Bad \xff.mkv"));
        touch(&bad_file, 1);
        let bad_dir = root.join(OsStr::from_bytes(b"Folder \xfe"));
        touch(&bad_dir.join("Inside.mkv"), 1);
        touch(&root.join(OsStr::from_bytes(b"notes \xff.txt")), 1);

        let result = walk(root, &ScanOptions::default()).unwrap();
        assert_eq!(relative_files(root, &result), ["Good.mkv"]);
        let mut reported: Vec<&Path> = result.errors.iter().map(|e| e.0.as_path()).collect();
        reported.sort();
        let mut expected = vec![bad_file.as_path(), bad_dir.as_path()];
        expected.sort();
        assert_eq!(reported, expected);
        assert!(
            result
                .errors
                .iter()
                .all(|e| e.1.contains("not valid UTF-8"))
        );
    }

    /// Benchmark: `cargo test -p chrysopoeia-scanner --release -- --ignored
    /// --nocapture walk_100k`. Builds a 100 000-file library (80 000 media
    /// files plus sidecars and ignored folders) and times one walk.
    #[test]
    #[ignore = "benchmark; creates 100 000 files"]
    fn walk_100k_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for show in 0..400 {
            for season in 0..5 {
                let folder = root.join(format!("Show {show:03}/Season {season:02}"));
                fs::create_dir_all(folder.join("@eaDir")).unwrap();
                for episode in 0..40 {
                    fs::write(folder.join(format!("E{episode:02}.mkv")), b"x").unwrap();
                }
                for extra in 0..10 {
                    fs::write(folder.join(format!("E{extra:02}.srt")), b"x").unwrap();
                }
            }
        }
        let started = std::time::Instant::now();
        let result = walk(root, &default_options()).unwrap();
        let elapsed = started.elapsed();
        eprintln!("walked {} media files in {elapsed:?}", result.files.len());
        assert_eq!(result.files.len(), 80_000);
        assert!(elapsed < std::time::Duration::from_secs(30));
    }
}
