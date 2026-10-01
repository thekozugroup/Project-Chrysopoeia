//! Chrysopoeia server: HTTP API, WebSocket events, library scanning service,
//! job queue, and static hosting of the web UI. See `docs/ARCHITECTURE.md`.
//!
//! The media crates (hardware detection, scanner, worker) are reached through
//! [`toolkit::MediaToolkit`], so the whole server can run against a fake
//! toolkit in tests.

pub mod api;
pub mod app;
pub mod config;
pub mod db;
pub mod error;
pub mod format;
pub mod guard;
pub mod http;
pub mod services;
pub mod state;
pub mod toolkit;
pub mod web;
pub mod ws;

#[cfg(test)]
mod tests;

use std::sync::Arc;

pub use config::{Cli, Config};
pub use state::AppState;
pub use toolkit::{MediaToolkit, RealToolkit, Toolkit};

/// Run the server with the real media tools until SIGINT/SIGTERM.
pub async fn run(config: Config) -> anyhow::Result<()> {
    let toolkit = Toolkit::new(Arc::new(RealToolkit::new(config.ffprobe.clone())));
    app::run(config, toolkit).await
}

/// Route panics through the log (with their location) instead of bare
/// stderr, so they show up in `docker logs` next to everything else.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        tracing::error!(%location, "internal error (panic): {message}");
    }));
}

/// Set up logging: `RUST_LOG` when set, else `level` (one of `error`,
/// `warn`, `info`, `debug`, `trace`, checked by
/// [`config::parse_log_level`]). Colors only on a terminal (and never with
/// `NO_COLOR`), so `docker logs` stays readable.
pub fn init_tracing(level: &str) {
    use std::io::IsTerminal;
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(format!("{level},sqlx=warn,hyper=warn,tower_http=info")))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(color)
        .with_writer(|| OneLine(std::io::stdout()))
        .try_init();
}

/// Text made safe for one log line: line breaks, tabs and other control
/// characters in it (a file name can hold any of them) are shown as
/// escapes, so they can't start a line that looks like one of the server's
/// own, or send commands to a terminal.
pub fn log_text(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.chars().any(char::is_control) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&c.escape_unicode().to_string()),
            c => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// A log writer that keeps each message on one line: a line break inside a
/// message (from a file name, say) is written as `\n`, so no message can
/// forge another. `tracing` writes each message with a single `write_all`,
/// ending in its own line break.
struct OneLine<W>(W);

impl<W: std::io::Write> std::io::Write for OneLine<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_all(buf)?;
        Ok(buf.len())
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        let body = buf.strip_suffix(b"\n").unwrap_or(buf);
        if !body.iter().any(|b| matches!(b, b'\n' | b'\r')) {
            return self.0.write_all(buf);
        }
        let mut out = Vec::with_capacity(buf.len() + 8);
        for &b in body {
            match b {
                b'\n' => out.extend_from_slice(b"\\n"),
                b'\r' => out.extend_from_slice(b"\\r"),
                b => out.push(b),
            }
        }
        if body.len() < buf.len() {
            out.push(b'\n');
        }
        self.0.write_all(&out)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

#[cfg(test)]
mod log_tests {
    use super::*;
    use std::io::Write as _;

    /// A file named `nl\n2026-… ERROR forged line.mkv` used to print a
    /// second, forged log line.
    #[test]
    fn a_message_stays_on_one_line() {
        let name = "nl\n2026-09-28T16:00:00.000000Z ERROR forged\r line\t\u{1b}[31m.mkv";
        assert_eq!(
            log_text(name),
            "nl\\n2026-09-28T16:00:00.000000Z ERROR forged\\r line\\t\\u{1b}[31m.mkv"
        );
        assert!(matches!(
            log_text("Movie (2020).mkv"),
            std::borrow::Cow::Borrowed(_)
        ));

        let mut w = OneLine(Vec::new());
        w.write_all(b"INFO Converted a\nERROR forged.mkv\n")
            .unwrap();
        w.write_all(b"INFO plain\n").unwrap();
        w.write_all(b"no end").unwrap();
        assert_eq!(
            String::from_utf8(w.0).unwrap(),
            "INFO Converted a\\nERROR forged.mkv\nINFO plain\nno end"
        );
    }
}
