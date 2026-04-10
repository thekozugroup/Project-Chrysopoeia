//! Media library scanner and filesystem watcher for Chrysopeia.
//!
//! Discovers media files, probes their format, and watches for changes.

pub mod probe;
pub mod scan;
pub mod watch;

pub use scan::scan_directory;
pub use watch::FileWatcher;
