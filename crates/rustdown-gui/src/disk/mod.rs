//! Disk I/O subsystem — file reading/writing, synchronisation state,
//! and file-watcher integration.

pub mod io;
pub mod sync;
pub(crate) mod watcher;
