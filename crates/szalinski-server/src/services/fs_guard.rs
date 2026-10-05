//! Filesystem checks on folders that may hang.
//!
//! The checks are the worker's ([`szalinski_worker::slow_fs`]), so the
//! server and running jobs share one set of them: a share that stopped
//! answering costs a few blocking threads at most, however often (and by
//! whom) things on it are looked at, and never the checks of other mounts.
//! A check that couldn't start because too many are stuck elsewhere is
//! [`NoAnswer::Busy`]: unknown, never taken for a folder that stopped
//! answering. See that module for how.

pub use szalinski_worker::slow_fs::{
    MAX_STUCK_CHECKS, MAX_STUCK_PER_MOUNT, NoAnswer, guarded, metadata, symlink_metadata,
};

/// Tests stand in for a share that stopped answering (see
/// [`szalinski_worker::slow_fs::hang`]).
#[cfg(test)]
pub use szalinski_worker::slow_fs::hang;
