//! Start-up, background services and graceful shutdown.
//!
//! Order at start: data folder lock → port → database (migrated) → settings
//! (first run applies `HW_ACCEL` and `LIBRARIES`; a changed `HW_ACCEL` is
//! applied on later starts too) → interrupted jobs (finished when their new
//! file was already in place, else re-queued) → HTTP server. In the
//! background: hardware detection, crash-artifact recovery (the dispatcher
//! waits for both), then the folder watcher and catch-up scans; the
//! dispatcher, periodic rescans and history trimming.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use chrysopoeia_core::paths::is_artifact;
use chrysopoeia_core::{ActivityLevel, HwApi, HwPreference, Settings};
use chrysopoeia_scanner::ScanOptions;
use tokio::net::TcpListener;

use crate::config::Config;
use crate::db::activity::ActivityRefs;
use crate::db::{self, DB_FILE_NAME, Db};
use crate::services::{dispatcher, hardware, library, library_admin, rescan, watcher};
use crate::state::{AppState, lock};
use crate::toolkit::Toolkit;

/// Time allowed for running jobs to stop at shutdown.
pub const JOB_STOP_TIMEOUT: Duration = Duration::from_secs(7);
/// Time allowed for open HTTP connections to finish at shutdown.
pub const HTTP_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Facts from start-up that the background tasks need.
#[derive(Debug, Clone, Copy, Default)]
pub struct Startup {
    /// The previous run shut down cleanly (no leftover temp files expected
    /// in library folders).
    pub clean_shutdown: bool,
    /// Jobs that were running when the previous run stopped.
    pub requeued_jobs: u64,
}

/// Name of the lock file in the data folder (see [`lock_data_dir`]).
pub const LOCK_FILE_NAME: &str = "chrysopoeia.lock";

/// Holds the data folder for this process; released when dropped (or when
/// the process ends, however it ends).
#[derive(Debug)]
pub struct DataDirLock {
    _file: std::fs::File,
}

/// Make sure no other Chrysopoeia uses the data folder: a second one would
/// put the first one's running jobs back in the queue, start them twice and
/// delete their temp files. Takes an exclusive lock on a file in the folder,
/// creating the folder if needed. Where the filesystem can't lock (some
/// network shares), a warning is logged and the server starts anyway.
pub fn lock_data_dir(dir: &Path) -> anyhow::Result<DataDirLock> {
    std::fs::create_dir_all(dir).with_context(|| {
        format!(
            "Could not create the data folder {}. Check that it exists and is writable",
            dir.display()
        )
    })?;
    let path = dir.join(LOCK_FILE_NAME);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| {
            format!(
                "Could not open {} in the data folder. Check that the folder is writable",
                path.display()
            )
        })?;
    #[cfg(unix)]
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(e) if e == rustix::io::Errno::WOULDBLOCK => anyhow::bail!(
            "Another Chrysopoeia is already using the data folder {}. Stop it first, or give \
             this one its own data folder (DATA_DIR)",
            dir.display()
        ),
        Err(e) => tracing::warn!(
            "Could not lock the data folder {} ({e}). Make sure only one Chrysopoeia uses it",
            dir.display()
        ),
    }
    Ok(DataDirLock { _file: file })
}

/// Open the database, load settings, apply first-run options and recover
/// interrupted jobs. Does not start any background task.
pub async fn build(config: Config, toolkit: Toolkit) -> anyhow::Result<(AppState, Startup)> {
    tokio::fs::create_dir_all(&config.data_dir)
        .await
        .with_context(|| {
            format!(
                "Could not create the data folder {}. Check that it exists and is writable",
                config.data_dir.display()
            )
        })?;
    let db_path = config.data_dir.join(DB_FILE_NAME);
    let (db, damaged_note) = match Db::open_with(&db_path, config.db_busy_timeout).await {
        Ok(db) => (db, None),
        Err(e) => {
            // A damaged database would stop every start (and a container
            // set to restart would loop): keep it aside and start afresh.
            // It only holds settings, the file list and the history; the
            // media files are never touched.
            let Some(damaged) = e.downcast_ref::<db::DamagedDatabase>() else {
                return Err(e);
            };
            tracing::error!("{damaged}");
            let aside = db::move_damaged_aside(&db_path)?;
            let note = format!(
                "The database was damaged, so Chrysopoeia moved it aside to {} and started \
                 with a new one. Your media files were not touched. Add your libraries and \
                 settings again.",
                aside.display()
            );
            let db = Db::open_with(&db_path, config.db_busy_timeout).await?;
            (db, Some(note))
        }
    };
    let pool = db.pool();
    let (mut settings, first_run) = match db::settings::load(pool).await? {
        Some(s) => (s, false),
        None => {
            let s = crate::services::settings::first_run_settings(&config);
            db::settings::save(pool, &s).await?;
            (s, true)
        }
    };
    let hw_note = apply_hw_accel(pool, &config, &mut settings, first_run).await?;
    let paused = db::settings::get_flag(pool, db::settings::QUEUE_PAUSED_KEY)
        .await?
        .unwrap_or(false);
    let clean_shutdown = db::settings::get_flag(pool, db::settings::CLEAN_SHUTDOWN_KEY)
        .await?
        .unwrap_or(first_run);
    db::settings::set_flag(pool, db::settings::CLEAN_SHUTDOWN_KEY, false).await?;

    let state = AppState::new(config, db, toolkit, settings, paused);
    lock(&state.leftovers).searching = !clean_shutdown;
    // Each note is one feed entry and one log line (the feed logs it).
    let notes = damaged_note
        .map(|n| (ActivityLevel::Warning, n))
        .into_iter()
        .chain(hw_note.map(|n| (ActivityLevel::Info, n)))
        .chain(max_jobs_note(&state).map(|n| (ActivityLevel::Warning, n)));
    for (level, note) in notes {
        state.activity(level, note, ActivityRefs::default()).await;
    }
    if first_run {
        tracing::info!("first start: settings created");
        for path in state.config.libraries.clone() {
            let raw = path.to_string_lossy().into_owned();
            let new = library_admin::NewLibrary {
                path: raw.clone(),
                ..Default::default()
            };
            match library_admin::create(&state, new).await {
                Ok(lib) => {
                    tracing::info!(library = %lib.name, path = %lib.path, "library added from LIBRARIES")
                }
                Err(e) => {
                    tracing::warn!(path = %raw, "could not add a library from LIBRARIES: {}", e.message)
                }
            }
        }
    }
    dispatcher::complete_interrupted(&state).await;
    let requeued_jobs = db::jobs::recover_interrupted(&state.db).await?;
    if requeued_jobs > 0 {
        state
            .activity(
                ActivityLevel::Warning,
                format!(
                    "Chrysopoeia restarted while {} running. {} back in the queue.",
                    crate::format::plural(requeued_jobs, "job was", "jobs were"),
                    if requeued_jobs == 1 {
                        "It is"
                    } else {
                        "They are"
                    }
                ),
                ActivityRefs::default(),
            )
            .await;
    }
    Ok((
        state,
        Startup {
            clean_shutdown,
            requeued_jobs,
        },
    ))
}

/// `HW_ACCEL` sets the hardware preference on the first start, and again
/// whenever its value changes (so editing the container template works), but
/// a choice made in Settings afterwards is kept while `HW_ACCEL` stays the
/// same. Returns a note for the activity feed when it changed the setting.
async fn apply_hw_accel(
    pool: &sqlx::SqlitePool,
    config: &Config,
    settings: &mut Settings,
    first_run: bool,
) -> anyhow::Result<Option<String>> {
    let wanted = db::enum_str(&config.hw);
    let applied = db::settings::get_raw(pool, db::settings::HW_ACCEL_APPLIED_KEY).await?;
    if applied.as_deref() == Some(wanted.as_str()) {
        return Ok(None);
    }
    let mut note = None;
    if !first_run && config.hw != HwPreference::Auto && settings.hardware != config.hw {
        settings.hardware = config.hw;
        db::settings::save(pool, settings).await?;
        let label = config.hw.api().map_or("Automatic", HwApi::label);
        note = Some(format!(
            "Hardware preference set to {label}, because HW_ACCEL={wanted} is set for the \
             container. You can still change it in Settings."
        ));
    }
    db::settings::set_raw(pool, db::settings::HW_ACCEL_APPLIED_KEY, &wanted).await?;
    Ok(note)
}

/// A note when `MAX_JOBS` is set but a number chosen in Settings wins.
fn max_jobs_note(state: &AppState) -> Option<String> {
    let env = state.config.max_jobs?;
    let saved = state.settings().max_jobs?;
    (env != saved).then(|| {
        format!(
            "MAX_JOBS={env} is not used because Files at once is set to {saved} in Settings. \
             Choose Automatic there to use MAX_JOBS."
        )
    })
}

/// Folders that may hold leftover temp files.
fn temp_dirs(state: &AppState) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for d in [
        state.settings().temp_dir.map(PathBuf::from),
        state.config.temp_dir.clone(),
    ]
    .into_iter()
    .flatten()
    {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs
}

/// Hand every leftover temp/backup file to the worker's crash recovery. The
/// temp folders are always checked; library folders and the output folder
/// (where folder mode writes when no temp folder is set) only after an
/// unclean shutdown (a clean one lets every job remove its own files).
/// Returns the library and output folders that couldn't be searched (missing,
/// unreadable or not responding, or a library with files that is empty now:
/// an unmounted share); a scan that reaches one searches it later.
pub async fn recover_artifacts(state: &AppState, include_libraries: bool) -> Vec<PathBuf> {
    // (folder, whether it must be searched later when it can't be now)
    let mut roots: Vec<(PathBuf, bool)> = temp_dirs(state).into_iter().map(|d| (d, false)).collect();
    let mut libraries_with_files: Vec<PathBuf> = Vec::new();
    if include_libraries {
        match db::libraries::list(state.db.pool()).await {
            Ok(libs) => {
                for lib in libs {
                    if db::files::any_in_library(state.db.pool(), lib.id)
                        .await
                        .unwrap_or(true)
                    {
                        libraries_with_files.push(PathBuf::from(&lib.path));
                    }
                    roots.push((PathBuf::from(lib.path), true));
                }
            }
            Err(e) => tracing::error!("could not list libraries for recovery: {e}"),
        }
        if let Some(out) = state.settings().output_folder.map(PathBuf::from)
            && !roots.iter().any(|(r, _)| *r == out)
        {
            roots.push((out, true));
        }
    }
    let mut unreached = Vec::new();
    let mut artifacts: Vec<PathBuf> = Vec::new();
    for (root, keep_for_later) in roots {
        let root_str = root.to_string_lossy().into_owned();
        let problem = if libraries_with_files.contains(&root) {
            library::root_unavailable(&root_str).await
        } else {
            library::path_problem(&root_str).await
        };
        if let Some(problem) = problem {
            if keep_for_later {
                tracing::info!("will look for leftovers later: {problem}");
                unreached.push(root);
            }
            continue;
        }
        match state
            .toolkit
            .walk_library(root.clone(), ScanOptions::default())
            .await
        {
            Ok(Ok(walk)) => artifacts.extend(walk.artifacts),
            Ok(Err(e)) => {
                tracing::warn!(root = %root.display(), "could not look for leftovers: {e:#}");
                if keep_for_later {
                    unreached.push(root);
                }
            }
            Err(_) => {
                // Fall back to the top level of the folder.
                if let Ok(mut rd) = tokio::fs::read_dir(&root).await {
                    while let Ok(Some(entry)) = rd.next_entry().await {
                        if entry.file_name().to_str().is_some_and(is_artifact) {
                            artifacts.push(entry.path());
                        }
                    }
                }
            }
        }
    }
    library::recover_leftovers(state, artifacts, None).await;
    unreached
}

/// Start every background task.
pub fn start_background(state: &AppState, startup: Startup) {
    let s = state.clone();
    tokio::spawn(async move {
        hardware::detect(&s).await;
    });

    // Leftovers first (a backup put back must not look like a removed
    // file), then the watcher, then the catch-up scans, so nothing that
    // changes meanwhile falls between them.
    let s = state.clone();
    tokio::spawn(async move {
        let unreached = recover_artifacts(&s, !startup.clean_shutdown).await;
        {
            let mut leftovers = lock(&s.leftovers);
            leftovers.searching = false;
            leftovers.unreached.extend(unreached);
        }
        s.dispatcher.set_ready();
        watcher::sync(&s).await;
        startup_scans(&s).await;
    });

    tokio::spawn(dispatcher::run(state.clone()));
    tokio::spawn(hardware::recheck_loop(state.clone()));
    tokio::spawn(rescan::run(state.clone()));
    tokio::spawn(rescan::trim_history_loop(state.clone()));
}

/// Scans at start: libraries never scanned, and, when changes are meant to
/// be found by themselves (folder watching or periodic rescans), every
/// enabled library, since the watcher could not see what changed while the
/// server was down. Scans only compare sizes and dates and probe new or
/// changed files, so this is cheap.
async fn startup_scans(state: &AppState) {
    let settings = state.settings();
    let catch_up = settings.watch_folders || settings.rescan_interval_hours > 0;
    // Files the last scan left for later (still being copied then) are
    // looked at again by a scan: the server stopped before it got to them.
    let settling = db::libraries::with_settling_files(state.db.pool())
        .await
        .unwrap_or_default();
    match db::libraries::list(state.db.pool()).await {
        Ok(libs) => {
            for lib in libs.into_iter().filter(|l| {
                l.enabled && (catch_up || l.last_scan_at.is_none() || settling.contains(&l.id))
            }) {
                if state.shutdown.is_cancelled() {
                    return;
                }
                let _ = library::start_scan(state, lib.id);
            }
        }
        Err(e) => tracing::error!("could not list libraries: {e}"),
    }
}

/// Bind the listening socket, with a plain explanation when that fails.
pub async fn bind(config: &Config) -> anyhow::Result<TcpListener> {
    let addr = SocketAddr::new(config.bind, config.port);
    TcpListener::bind(addr).await.with_context(|| {
        format!(
            "Could not listen on {addr}. Another program may be using port {}; set PORT to a \
             free port",
            config.port
        )
    })
}

/// Serve HTTP until shutdown begins.
pub async fn serve(state: AppState, listener: TcpListener) -> anyhow::Result<()> {
    let router = crate::web::app(state.clone()).await;
    let shutdown = state.shutdown.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await
        .context("the HTTP server stopped")
}

/// Begin shutting down: no new jobs, running jobs back to the queue, scans
/// and the watcher stopped. The HTTP server starts draining too.
pub async fn begin_shutdown(state: &AppState) {
    state.shutdown.cancel();
    state.library.cancel_all_scans();
    watcher::stop(state);
    dispatcher::shutdown(state, JOB_STOP_TIMEOUT).await;
}

/// Finish shutting down: record whether it was clean and close the database.
/// It isn't while crash leftovers are still to be looked for (the start-up
/// search didn't finish, or a library folder was out of reach).
pub async fn finish_shutdown(state: &AppState) {
    let clean = state.dispatcher.running_count() == 0 && !lock(&state.leftovers).due();
    if let Err(e) =
        db::settings::set_flag(state.db.pool(), db::settings::CLEAN_SHUTDOWN_KEY, clean).await
    {
        tracing::warn!("could not record the shutdown: {e}");
    }
    state.db.close().await;
}

/// Wait for SIGINT or SIGTERM.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!("could not listen for Ctrl-C: {e}");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::warn!("could not listen for SIGTERM: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

/// Run the server until SIGINT/SIGTERM.
pub async fn run(config: Config, toolkit: Toolkit) -> anyhow::Result<()> {
    // Nothing is written before this process owns the data folder and the
    // port, so a second copy started by mistake changes nothing.
    let _lock = lock_data_dir(&config.data_dir)?;
    let listener = bind(&config).await?;
    let (state, startup) = build(config, toolkit).await?;
    let addr = listener
        .local_addr()
        .map_or_else(|_| "?".to_string(), |a| a.to_string());
    let version = match crate::api::system::build_label() {
        Some(build) => format!("{} (build {build})", env!("CARGO_PKG_VERSION")),
        None => env!("CARGO_PKG_VERSION").to_string(),
    };
    tracing::info!(
        "Chrysopoeia {version} is running on http://{addr} (data in {})",
        state.config.data_dir.display()
    );
    start_background(&state, startup);
    let mut server = tokio::spawn(serve(state.clone(), listener));
    let server_result = tokio::select! {
        () = shutdown_signal() => None,
        r = &mut server => Some(r),
    };
    tracing::info!("shutting down");
    begin_shutdown(&state).await;
    if server_result.is_none()
        && tokio::time::timeout(HTTP_DRAIN_TIMEOUT, &mut server)
            .await
            .is_err()
    {
        server.abort();
    }
    finish_shutdown(&state).await;
    match server_result {
        Some(Ok(Err(e))) => Err(e),
        Some(Err(e)) => Err(anyhow::anyhow!("the HTTP server stopped unexpectedly: {e}")),
        _ => {
            tracing::info!("stopped");
            Ok(())
        }
    }
}
