//! Child processes (ffmpeg, ffprobe) that must not outlive the server.

/// Make a child process end when the server does, even when the server is
/// killed (`kill -9`, the out-of-memory killer, a crash) and no cleanup
/// runs. Otherwise an orphaned ffmpeg would keep encoding, and after a
/// restart compete with the same job started again.
///
/// Linux only (elsewhere this does nothing): the child is sent SIGKILL when
/// the thread that started it exits. Children are started from the async
/// runtime's worker threads, which live as long as the server; don't start
/// one from a short-lived thread.
pub fn end_with_parent(command: &mut std::process::Command) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt as _;
        let parent = rustix::process::getpid();
        // SAFETY: the closure runs in the child between fork and exec. It
        // only makes raw system calls (prctl, getppid), which are
        // async-signal-safe, and allocates nothing.
        unsafe {
            command.pre_exec(move || {
                rustix::process::set_parent_process_death_signal(Some(
                    rustix::process::Signal::KILL,
                ))?;
                // The server may have died before the signal was armed.
                if rustix::process::getppid() != Some(parent) {
                    return Err(std::io::Error::other("the server stopped"));
                }
                Ok(())
            });
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = command;
}

/// Waits between attempts to start a program while too many files are
/// open (see [`out_of_file_handles`]): about half a minute in all.
pub const SPAWN_RETRY_DELAYS: [std::time::Duration; 5] = [
    std::time::Duration::from_secs(1),
    std::time::Duration::from_secs(2),
    std::time::Duration::from_secs(4),
    std::time::Duration::from_secs(8),
    std::time::Duration::from_secs(15),
];

/// Whether starting a program failed because this process (`EMFILE`) or the
/// whole system (`ENFILE`) has too many files open. That passes once other
/// work finishes, so it is worth waiting for rather than failing the file.
pub fn out_of_file_handles(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(23 | 24))
}

#[cfg(test)]
mod handle_tests {
    use super::*;

    #[test]
    fn running_out_of_files_is_recognised() {
        assert!(out_of_file_handles(&std::io::Error::from_raw_os_error(24)));
        assert!(out_of_file_handles(&std::io::Error::from_raw_os_error(23)));
        assert!(!out_of_file_handles(&std::io::Error::from_raw_os_error(2)));
        assert!(!out_of_file_handles(&std::io::Error::other("x")));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn children_still_run_normally() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "exit 3"]);
        end_with_parent(&mut command);
        let status = command.status().unwrap();
        assert_eq!(status.code(), Some(3));
    }

    #[test]
    fn a_child_ends_when_the_thread_that_started_it_ends() {
        use std::os::unix::process::ExitStatusExt as _;
        // The thread stands in for the server: when it is gone, so is the
        // child, instead of sleeping on for a minute.
        let mut child = std::thread::spawn(|| {
            let mut command = std::process::Command::new("sleep");
            command.arg("60");
            end_with_parent(&mut command);
            command.spawn().unwrap()
        })
        .join()
        .unwrap();
        let started = std::time::Instant::now();
        let status = child.wait().unwrap();
        assert_eq!(status.signal(), Some(9), "{status:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
    }
}
