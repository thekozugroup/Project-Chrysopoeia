//! Command-line flags and environment variables.
//!
//! Every flag has an environment variable fallback (see the configuration
//! table in `docs/ARCHITECTURE.md`). Values arrive as raw strings in [`Cli`]
//! and are resolved into a [`Config`], because container templates (Unraid in
//! particular) often pass optional variables as empty strings, which must mean
//! "not set" rather than abort the start-up.

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use chrysopoeia_core::HwPreference;
use clap::Parser;

/// Raw command line, as parsed by clap. Use [`Cli::resolve`] to get a
/// [`Config`].
#[derive(Debug, Clone, Parser)]
#[command(
    name = "chrysopoeia",
    version,
    about = "Chrysopoeia: converts media libraries in the background and verifies every result."
)]
pub struct Cli {
    /// Port for the web UI and API.
    #[arg(long, env = "PORT", default_value = "8080")]
    pub port: String,
    /// Address to listen on.
    #[arg(long, env = "BIND", default_value = "0.0.0.0")]
    pub bind: String,
    /// Folder for the database.
    #[arg(long, env = "DATA_DIR", default_value = "./data")]
    pub data_dir: String,
    /// Folder with the exported web UI.
    #[arg(long, env = "WEB_DIR", default_value = "./web/out")]
    pub web_dir: String,
    /// ffmpeg binary.
    #[arg(long = "ffmpeg", env = "FFMPEG_PATH", default_value = "ffmpeg")]
    pub ffmpeg: String,
    /// ffprobe binary.
    #[arg(long = "ffprobe", env = "FFPROBE_PATH", default_value = "ffprobe")]
    pub ffprobe: String,
    /// Folders the folder picker may show (comma-separated in the env var).
    #[arg(
        long = "browse-root",
        env = "BROWSE_ROOTS",
        value_delimiter = ',',
        default_value = "/"
    )]
    pub browse_roots: Vec<String>,
    /// Scratch folder for in-progress encodes when the setting is unset.
    #[arg(long, env = "TEMP_DIR")]
    pub temp_dir: Option<String>,
    /// Files converted at once while "Files at once" in Settings is Automatic: 1-32, or
    /// auto for the count recommended for this machine. A number chosen in
    /// Settings wins.
    #[arg(long, env = "MAX_JOBS")]
    pub max_jobs: Option<String>,
    /// Hardware preference: auto, cpu, nvenc, qsv, vaapi, amf, videotoolbox,
    /// rkmpp or v4l2m2m. Applied on the first run and whenever this value
    /// changes; otherwise the choice made in Settings is kept.
    #[arg(long = "hw", env = "HW_ACCEL", default_value = "auto")]
    pub hw: String,
    /// Libraries to create on first run (comma-separated in the env var).
    #[arg(long = "library", env = "LIBRARIES", value_delimiter = ',')]
    pub libraries: Vec<String>,
    /// Log level (error, warn, info, debug, trace). `RUST_LOG` wins when set.
    #[arg(long, env = "LOG_LEVEL", default_value = "info")]
    pub log_level: String,
    /// Allow cross-origin requests (for `next dev` on another port).
    #[arg(
        long,
        env = "DEV_CORS",
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    pub dev_cors: Option<String>,
    /// Extra host names the server answers to (comma-separated in the env
    /// var), e.g. the domain of a reverse proxy. IP addresses, localhost and
    /// local names (tower, tower.local) always work. `*` allows any name.
    #[arg(long = "allowed-host", env = "ALLOWED_HOSTS", value_delimiter = ',')]
    pub allowed_hosts: Vec<String>,
}

/// Resolved server configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub bind: IpAddr,
    pub data_dir: PathBuf,
    pub web_dir: PathBuf,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    /// Never empty.
    pub browse_roots: Vec<PathBuf>,
    pub temp_dir: Option<PathBuf>,
    pub max_jobs: Option<u32>,
    pub hw: HwPreference,
    pub libraries: Vec<PathBuf>,
    pub log_level: String,
    pub dev_cors: bool,
    /// Extra host names the API answers to (lowercase, without port).
    /// Contains `*` to allow any.
    pub allowed_hosts: Vec<String>,
    /// How long a file must go unmodified before it is converted, so files
    /// still being copied are left alone. Not a command-line option.
    pub settle: Duration,
    /// How long a database writer waits for another. Not a command-line
    /// option.
    pub db_busy_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 8080,
            bind: IpAddr::from([0, 0, 0, 0]),
            data_dir: PathBuf::from("./data"),
            web_dir: PathBuf::from("./web/out"),
            ffmpeg: PathBuf::from("ffmpeg"),
            ffprobe: PathBuf::from("ffprobe"),
            browse_roots: vec![PathBuf::from("/")],
            temp_dir: None,
            max_jobs: None,
            hw: HwPreference::Auto,
            libraries: Vec::new(),
            log_level: "info".into(),
            dev_cors: false,
            allowed_hosts: Vec::new(),
            settle: DEFAULT_SETTLE,
            db_busy_timeout: crate::db::DEFAULT_BUSY_TIMEOUT,
        }
    }
}

/// How long a file's size and modification time must stay unchanged before
/// it is picked up, so files still being copied are not converted
/// half-written.
pub const DEFAULT_SETTLE: Duration = Duration::from_secs(20);

/// Trimmed value, or `None` when empty.
fn non_empty(value: &str) -> Option<&str> {
    let v = value.trim();
    (!v.is_empty()).then_some(v)
}

fn path_list(values: &[String]) -> Vec<PathBuf> {
    values
        .iter()
        .filter_map(|v| non_empty(v))
        .map(PathBuf::from)
        .collect()
}

/// Parse a hardware preference, accepting a few friendly aliases.
pub fn parse_hw_preference(value: &str) -> anyhow::Result<HwPreference> {
    let v = value.trim().to_ascii_lowercase();
    let canonical = match v.as_str() {
        "" | "automatic" => "auto",
        "none" | "software" | "sw" => "cpu",
        "nvidia" | "cuda" => "nvenc",
        "intel" | "quicksync" => "qsv",
        "amd" | "radeon" => "vaapi",
        "apple" | "mac" => "videotoolbox",
        "rockchip" => "rkmpp",
        "v4l2" | "raspberrypi" => "v4l2m2m",
        other => other,
    };
    serde_json::from_value(serde_json::Value::String(canonical.to_string())).map_err(|_| {
        anyhow::anyhow!(
            "HW_ACCEL must be one of auto, cpu, nvenc, qsv, vaapi, amf, videotoolbox, rkmpp \
             or v4l2m2m (got \"{value}\")"
        )
    })
}

/// Lowercase a host name and drop a port or URL scheme someone pasted along
/// with it (`https://media.example.com:443/` becomes `media.example.com`).
fn normalize_host_name(value: &str) -> String {
    let v = value.trim().to_ascii_lowercase();
    let v = v.split_once("://").map_or(v.as_str(), |(_, rest)| rest);
    let v = v.split('/').next().unwrap_or_default();
    crate::guard::split_host_port(v).0.to_string()
}

fn parse_bool(value: &str) -> anyhow::Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "no" | "off" | "n" => Ok(false),
        "1" | "true" | "yes" | "on" | "y" => Ok(true),
        _ => bail!("DEV_CORS must be true or false (got \"{value}\")"),
    }
}

impl Cli {
    /// Validate and convert into a [`Config`]. Empty values mean "not set".
    pub fn resolve(&self) -> anyhow::Result<Config> {
        let defaults = Config::default();
        let port =
            match non_empty(&self.port) {
                None => defaults.port,
                Some(p) => p.parse::<u16>().ok().filter(|p| *p > 0).with_context(|| {
                    format!("PORT must be a number from 1 to 65535 (got \"{p}\")")
                })?,
            };
        let bind = match non_empty(&self.bind) {
            None => defaults.bind,
            Some(b) => b.parse::<IpAddr>().with_context(|| {
                format!("BIND must be an IP address like 0.0.0.0 (got \"{b}\")")
            })?,
        };
        let max_jobs = match self.max_jobs.as_deref().and_then(non_empty) {
            None => None,
            Some(v) if v.eq_ignore_ascii_case("auto") => None,
            Some(v) => Some(
                v.parse::<u32>()
                    .ok()
                    .filter(|n| (1..=32).contains(n))
                    .with_context(|| {
                        format!("MAX_JOBS must be a number from 1 to 32 (got \"{v}\")")
                    })?,
            ),
        };
        let mut browse_roots = path_list(&self.browse_roots);
        if browse_roots.is_empty() {
            browse_roots = defaults.browse_roots;
        }
        Ok(Config {
            port,
            bind,
            data_dir: non_empty(&self.data_dir).map_or(defaults.data_dir, PathBuf::from),
            web_dir: non_empty(&self.web_dir).map_or(defaults.web_dir, PathBuf::from),
            ffmpeg: non_empty(&self.ffmpeg).map_or(defaults.ffmpeg, PathBuf::from),
            ffprobe: non_empty(&self.ffprobe).map_or(defaults.ffprobe, PathBuf::from),
            browse_roots,
            temp_dir: self
                .temp_dir
                .as_deref()
                .and_then(non_empty)
                .map(PathBuf::from),
            max_jobs,
            hw: parse_hw_preference(&self.hw)?,
            libraries: path_list(&self.libraries),
            log_level: non_empty(&self.log_level).unwrap_or("info").to_string(),
            dev_cors: match self.dev_cors.as_deref() {
                None => false,
                Some(v) => parse_bool(v)?,
            },
            allowed_hosts: self
                .allowed_hosts
                .iter()
                .filter_map(|h| non_empty(h))
                .map(normalize_host_name)
                .collect(),
            settle: defaults.settle,
            db_busy_timeout: defaults.db_busy_timeout,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<Config> {
        let mut full = vec!["chrysopoeia"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full)?.resolve()
    }

    #[test]
    fn defaults_match_the_architecture_table() {
        let c = parse(&[]).unwrap();
        assert_eq!(c.port, 8080);
        assert_eq!(c.bind.to_string(), "0.0.0.0");
        assert_eq!(c.data_dir, PathBuf::from("./data"));
        assert_eq!(c.web_dir, PathBuf::from("./web/out"));
        assert_eq!(c.browse_roots, vec![PathBuf::from("/")]);
        assert_eq!(c.hw, HwPreference::Auto);
        assert!(c.temp_dir.is_none());
        assert!(c.max_jobs.is_none());
        assert!(!c.dev_cors);
    }

    #[test]
    fn flags_are_parsed() {
        let c = parse(&[
            "--port",
            "9000",
            "--browse-root",
            "/media",
            "--browse-root",
            "/mnt",
            "--hw",
            "nvidia",
            "--max-jobs",
            "3",
            "--library",
            "/media/Movies",
            "--dev-cors",
            "--temp-dir",
            "",
        ])
        .unwrap();
        assert_eq!(c.port, 9000);
        assert_eq!(c.browse_roots.len(), 2);
        assert_eq!(c.hw, HwPreference::Nvenc);
        assert_eq!(c.max_jobs, Some(3));
        assert_eq!(c.libraries, vec![PathBuf::from("/media/Movies")]);
        assert!(c.dev_cors);
        assert!(c.temp_dir.is_none(), "an empty value means unset");
    }

    #[test]
    fn allowed_hosts_are_normalized() {
        let c = parse(&[
            "--allowed-host",
            "Media.Example.com",
            "--allowed-host",
            "https://x.example.org:8443/",
            "--allowed-host",
            " ",
        ])
        .unwrap();
        assert_eq!(c.allowed_hosts, ["media.example.com", "x.example.org"]);
        assert!(parse(&[]).unwrap().allowed_hosts.is_empty());
    }

    #[test]
    fn invalid_values_explain_themselves() {
        let err = parse(&["--max-jobs", "99"]).unwrap_err().to_string();
        assert!(err.contains("MAX_JOBS"), "{err}");
        let err = parse(&["--hw", "gpu9000"]).unwrap_err().to_string();
        assert!(err.contains("HW_ACCEL"), "{err}");
        assert!(parse(&["--port", "http"]).is_err());
    }

    #[test]
    fn hardware_aliases() {
        assert_eq!(
            parse_hw_preference("VideoToolbox").unwrap(),
            HwPreference::VideoToolbox
        );
        assert_eq!(parse_hw_preference("intel").unwrap(), HwPreference::Qsv);
        assert_eq!(parse_hw_preference("").unwrap(), HwPreference::Auto);
        assert_eq!(parse_hw_preference("cpu").unwrap(), HwPreference::Cpu);
    }
}
