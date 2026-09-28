//! Shared application state: configuration, database, settings cache,
//! hardware, event bus and the handles of the background services.

use std::ops::Deref;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use chrysopoeia_core::{ActivityLevel, Event, Settings};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::Config;
use crate::db::Db;
use crate::db::activity::ActivityRefs;
use crate::services::dispatcher::DispatcherHandle;
use crate::services::hardware::HardwareState;
use crate::services::library::LibraryHandle;
use crate::toolkit::Toolkit;

/// Capacity of the event bus. Slow WebSocket clients that fall further
/// behind just miss events and refetch.
pub const EVENT_CAPACITY: usize = 1024;

/// Read a lock, recovering from poisoning (a panic while holding the lock
/// leaves the data intact for our plain-value locks).
pub fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Write a lock, recovering from poisoning.
pub fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Lock a mutex, recovering from poisoning.
pub fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Cheaply clonable handle to everything the server shares.
#[derive(Clone)]
pub struct AppState(Arc<AppInner>);

impl Deref for AppState {
    type Target = AppInner;

    fn deref(&self) -> &AppInner {
        &self.0
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AppState")
    }
}

/// The shared state behind [`AppState`].
pub struct AppInner {
    /// Resolved command-line configuration.
    pub config: Config,
    /// Database pool.
    pub db: Db,
    /// Media tools (panic-safe).
    pub toolkit: Toolkit,
    /// Event bus for the WebSocket.
    pub events: broadcast::Sender<Event>,
    settings: RwLock<Settings>,
    /// Serializes settings writes (read-merge-write).
    pub settings_write: tokio::sync::Mutex<()>,
    /// Detected hardware.
    pub hardware: HardwareState,
    /// Job queue state.
    pub dispatcher: DispatcherHandle,
    /// Scans in progress and the folder watcher.
    pub library: LibraryHandle,
    /// Cancelled when the server shuts down.
    pub shutdown: CancellationToken,
}

impl AppState {
    /// Assemble the state. Background services are started separately
    /// (see `app::start`).
    pub fn new(
        config: Config,
        db: Db,
        toolkit: Toolkit,
        settings: Settings,
        queue_paused: bool,
    ) -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self(Arc::new(AppInner {
            config,
            db,
            toolkit,
            events,
            settings: RwLock::new(settings),
            settings_write: tokio::sync::Mutex::new(()),
            hardware: HardwareState::default(),
            dispatcher: DispatcherHandle::new(queue_paused),
            library: LibraryHandle::default(),
            shutdown: CancellationToken::new(),
        }))
    }

    /// A copy of the current settings.
    pub fn settings(&self) -> Settings {
        read(&self.settings).clone()
    }

    /// Replace the cached settings (after they were persisted).
    pub fn replace_settings(&self, settings: Settings) {
        *write(&self.settings) = settings;
    }

    /// Publish an event to WebSocket clients. Having no clients is fine.
    pub fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    /// Whether the database was closed at the end of shutdown. A job that
    /// finished just before then has nothing left to publish or record, and
    /// no error to report about it.
    fn closed(&self) -> bool {
        self.db.pool().is_closed()
    }

    /// Record an activity entry and publish it. Failures are logged only:
    /// the feed must never break the operation it describes.
    pub async fn activity(
        &self,
        level: ActivityLevel,
        message: impl Into<String>,
        refs: ActivityRefs,
    ) {
        let message = message.into();
        match level {
            ActivityLevel::Error => tracing::warn!("{message}"),
            ActivityLevel::Warning => tracing::warn!("{message}"),
            _ => tracing::info!("{message}"),
        }
        if self.closed() {
            return;
        }
        match crate::db::activity::insert(self.db.pool(), level, &message, refs).await {
            Ok(entry) => self.emit(Event::Activity { entry }),
            Err(e) => tracing::error!("could not record activity: {e}"),
        }
    }

    /// Activity about a library.
    pub async fn library_activity(
        &self,
        level: ActivityLevel,
        message: impl Into<String>,
        library_id: Uuid,
    ) {
        let refs = ActivityRefs {
            library_id: Some(library_id),
            ..ActivityRefs::default()
        };
        self.activity(level, message, refs).await;
    }

    /// Publish the queue state.
    pub async fn broadcast_queue_state(&self) {
        if self.closed() {
            return;
        }
        match crate::services::dispatcher::queue_state(self).await {
            Ok(q) => self.emit(Event::QueueState(q)),
            Err(e) => tracing::error!("could not compute the queue state: {e}"),
        }
    }

    /// Publish overall stats.
    pub async fn broadcast_stats(&self) {
        if self.closed() {
            return;
        }
        match crate::db::stats::totals(self.db.pool()).await {
            Ok(totals) => self.emit(Event::StatsUpdated { totals }),
            Err(e) => tracing::error!("could not compute stats: {e}"),
        }
    }

    /// Publish a library with fresh stats.
    pub async fn broadcast_library(&self, id: Uuid) {
        if self.closed() {
            return;
        }
        match crate::services::library::view_by_id(self, id).await {
            Ok(Some(library)) => self.emit(Event::LibraryUpdated { library }),
            Ok(None) => {}
            Err(e) => tracing::error!("could not load library {id}: {e}"),
        }
    }

    /// Publish a file.
    pub async fn broadcast_file(&self, id: Uuid) {
        if self.closed() {
            return;
        }
        match crate::db::files::get(self.db.pool(), id, false).await {
            Ok(Some(file)) => self.emit(Event::FileUpdated { file }),
            Ok(None) => {}
            Err(e) => tracing::error!("could not load file {id}: {e}"),
        }
    }

    /// Publish a job.
    pub async fn broadcast_job(&self, id: Uuid) {
        if self.closed() {
            return;
        }
        match crate::db::jobs::get(self.db.pool(), id).await {
            Ok(Some(job)) => self.emit(Event::JobUpdated { job }),
            Ok(None) => {}
            Err(e) => tracing::error!("could not load job {id}: {e}"),
        }
    }
}
