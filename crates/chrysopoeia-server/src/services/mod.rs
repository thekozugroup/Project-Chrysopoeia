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
//! - [`fs_guard`]: checks on folders that may hang (network shares).
//! - [`share_mounts`]: the drives and shares the folders in use are
//!   mounted from, and whether they still are.

pub mod dispatcher;
pub mod fs_guard;
pub mod hardware;
pub mod library;
pub mod library_admin;
pub mod queue;
pub mod rescan;
pub mod settings;
pub mod share_mounts;
pub mod watcher;
