//! Small helpers for running external tools (ffmpeg, lspci, nvidia-smi)
//! without blocking the async runtime and without leaving processes behind.

use std::ffi::OsStr;
use std::io;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::process::Command;

/// Output of a finished process.
#[derive(Debug)]
pub(crate) struct Captured {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

/// Why a process could not produce output.
#[derive(Debug)]
pub(crate) enum RunError {
    /// The program does not exist (not installed or not on `PATH`).
    NotFound,
    /// It did not finish in time and was killed.
    TimedOut,
    /// Any other spawn or I/O failure.
    Io(io::Error),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("The program was not found."),
            Self::TimedOut => f.write_str("The program did not finish in time."),
            Self::Io(err) => write!(
                f,
                "The program couldn't be run because {}.",
                szalinski_core::plain::io_reason(err)
            ),
        }
    }
}

/// Run `program` with `args`, capturing stdout and stderr, and kill it if it
/// takes longer than `timeout`. Stdin is closed so nothing can wait for input.
pub(crate) async fn run_capture<I, S>(
    program: impl AsRef<OsStr>,
    args: I,
    timeout: Duration,
) -> Result<Captured, RunError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    szalinski_core::process::end_with_parent(command.as_std_mut());
    let child = command.spawn().map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => RunError::NotFound,
        _ => RunError::Io(err),
    })?;

    // Dropping the future on timeout drops the child, and kill_on_drop
    // sends SIGKILL; tokio reaps it in the background.
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(Captured {
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
        Ok(Err(err)) => Err(RunError::Io(err)),
        Err(_) => Err(RunError::TimedOut),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_program_is_not_found() {
        let result = run_capture(
            "szalinski-definitely-not-a-program",
            ["-version"],
            Duration::from_secs(5),
        )
        .await;
        assert!(matches!(result, Err(RunError::NotFound)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn slow_program_times_out() {
        let started = std::time::Instant::now();
        let result = run_capture("sleep", ["10"], Duration::from_millis(200)).await;
        assert!(matches!(result, Err(RunError::TimedOut)));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
