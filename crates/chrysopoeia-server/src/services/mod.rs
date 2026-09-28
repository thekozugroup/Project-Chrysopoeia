//! Background services and the operations the API triggers.
//!
//! - [`library`]: scanning, watch events, library views (LibraryService).
//! - [`library_admin`]: adding, changing and removing libraries.
//! - [`dispatcher`]: the job queue and job execution.
//! - [`queue`]: user actions on files and jobs.
//! - [`hardware`]: hardware detection state.
//! - [`settings`]: settings changes.
//! - [`watcher`]: folder watching.
//! - [`rescan`]: periodic rescans.

pub mod dispatcher;
pub mod hardware;
pub mod library;
pub mod library_admin;
pub mod queue;
pub mod rescan;
pub mod settings;
pub mod watcher;
