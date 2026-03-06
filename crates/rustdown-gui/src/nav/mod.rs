//! Navigation subsystem — heading outline extraction, table-of-contents
//! panel, and debug diagnostics.

pub mod outline;
pub mod panel;

#[cfg(debug_assertions)]
pub mod debug;
