//! Filesystem checks on folders that may hang.
//!
//! The checks are the worker's ([`chrysopoeia_worker::slow_fs`]), so the
//! server and running jobs share one set of them: a share that stopped
//! answering costs one blocking thread per thing looked at, however often
//! (and by whom) it is looked at. See that module for how.

pub use chrysopoeia_worker::slow_fs::{MAX_STUCK_CHECKS, guarded, metadata, symlink_metadata};

/// Tests stand in for a share that stopped answering (see
/// [`chrysopoeia_worker::slow_fs::hang`]).
#[cfg(test)]
pub use chrysopoeia_worker::slow_fs::hang;
