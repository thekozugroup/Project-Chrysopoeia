//! Start-up, background services and graceful shutdown.
//!
//! Order at start: data folder → database (migrated) → settings (first run
//! applies `HW_ACCEL` and `LIBRARIES`; a changed `HW_ACCEL` is applied on
//! later starts too) → recovery of interrupted jobs → HTTP server. In the
//! background: hardware detection, crash-artifact recovery (the dispatcher
//! waits for both), the folder watcher, the dispatcher, periodic rescans and
//! history trimming, and a first scan of never-scanned libraries.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use chrysopoeia_core::paths::is_artifact;
use chrysopoeia_core::{ActivityLevel, HwApi, HwPreference, Settings};
use chrysopoeia_scanner::ScanOptions;
use chrysopoeia_worker::finalize::Recovery;
use tokio::net::TcpListener;

use crate::config::Config;
use crate::db::activity::ActivityRefs;
use crate::db::{self, DB_FILE_NAME, Db};
use crate::services::{dispatcher, hardware, library, library_admin, rescan, watcher};
use crate::state::AppState;
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
    let db = Db::open_with(&config.data_dir.join(DB_FILE_NAME), config.db_busy_timeout).await?;
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
    for note in hw_note.into_iter().chain(max_jobs_note(&state)) {
        state
            .activity(ActivityLevel::Info, note, ActivityRefs::default())
            .await;
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
    if env == saved {
        return None;
    }
    let message = format!(
        "MAX_JOBS={env} is not used because Jobs at once is set to {saved} in Settings. Choose \
         Automatic there to use MAX_JOBS."
    );
    tracing::warn!("{message}");
    Some(message)
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
/// temp folders are always checked; library folders only after an unclean
/// shutdown (a clean one lets every job remove its own files).
pub async fn recover_artifacts(state: &AppState, include_libraries: bool) {
    let mut roots = temp_dirs(state);
    if include_libraries {
        match db::libraries::list(state.db.pool()).await {
            Ok(libs) => roots.extend(libs.into_iter().map(|l| PathBuf::from(l.path))),
            Err(e) => tracing::error!("could not list libraries for recovery: {e}"),
        }
    }
    let mut artifacts: Vec<PathBuf> = Vec::new();
    for root in roots {
        if !tokio::fs::metadata(&root).await.is_ok_and(|m| m.is_dir()) {
            continue;
        }
        match state
            .toolkit
            .walk_library(root.clone(), ScanOptions::default())
            .await
        {
            Ok(Ok(walk)) => artifacts.extend(walk.artifacts),
            Ok(Err(e)) => {
                tracing::warn!(root = %root.display(), "could not look for leftovers: {e:#}")
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
    artifacts.sort();
    artifacts.dedup();
    for path in artifacts {
        match state.toolkit.recover_artifact(path.clone()).await {
            Ok(Recovery::RestoredBackup(original)) => {
                state
                    .activity(
                        ActivityLevel::Warning,
                        format!(
                            "Restored {} from its backup after an interrupted conversion.",
                            original.display()
                        ),
                        ActivityRefs::default(),
                    )
                    .await;
            }
            Ok(r) => tracing::debug!(path = %path.display(), "recovery: {r:?}"),
            Err(e) => {
                tracing::warn!(path = %path.display(), "could not clean up a leftover file: {e:#}")
            }
        }
    }
}

/// Start every background task.
pub fn start_background(state: &AppState, startup: Startup) {
    let s = state.clone();
    tokio::spawn(async move {
        hardware::detect(&s).await;
    });

    let s = state.clone();
    tokio::spawn(async move {
        recover_artifacts(&s, !startup.clean_shutdown).await;
        s.dispatcher.set_ready();
    });

    let s = state.clone();
    tokio::spawn(async move { watcher::sync(&s).await });

    tokio::spawn(dispatcher::run(state.clone()));
    tokio::spawn(rescan::run(state.clone()));
    tokio::spawn(rescan::trim_history_loop(state.clone()));

    let s = state.clone();
    tokio::spawn(async move {
        match db::libraries::list(s.db.pool()).await {
            Ok(libs) => {
                for lib in libs
                    .into_iter()
                    .filter(|l| l.enabled && l.last_scan_at.is_none())
                {
                    let _ = library::start_scan(&s, lib.id);
                }
            }
            Err(e) => tracing::error!("could not list libraries: {e}"),
        }
    });
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
pub async fn finish_shutdown(state: &AppState) {
    let clean = state.dispatcher.running_count() == 0;
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
    let (state, startup) = build(config, toolkit).await?;
    let listener = bind(&state.config).await?;
    let addr = listener
        .local_addr()
        .map_or_else(|_| "?".to_string(), |a| a.to_string());
    tracing::info!(
        "Chrysopoeia {} is running on http://{addr} (data in {})",
        env!("CARGO_PKG_VERSION"),
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
