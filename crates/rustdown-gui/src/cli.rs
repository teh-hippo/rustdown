//! Command-line argument parsing, version output, and platform workarounds.

use std::{ffi::OsString, path::PathBuf};

use super::{DIAGNOSTICS_DEFAULT_ITERATIONS, DIAGNOSTICS_DEFAULT_RUNS, Mode};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchOptions {
    pub mode: Mode,
    /// `true` when the user explicitly chose a mode via CLI flag (`-p`, `-s`).
    pub mode_explicit: bool,
    pub path: Option<PathBuf>,
    pub print_version: bool,
    pub diagnostics: DiagnosticsMode,
    pub diagnostics_iterations: usize,
    pub diagnostics_runs: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiagnosticsMode {
    #[default]
    Off,
    OpenPipeline,
    #[cfg(debug_assertions)]
    NavPipeline,
}

/// Parse a `--long-name=VALUE` or `--short-name=VALUE` positive integer argument.
fn parse_kv_usize(arg: &OsString, long: &str, short: &str) -> Option<usize> {
    arg.to_str()
        .and_then(|s| s.strip_prefix(short).or_else(|| s.strip_prefix(long)))
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
}

#[must_use]
pub fn parse_launch_options<I, S>(args: I) -> LaunchOptions
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut mode = None;
    let mut path = None;
    let mut print_version = false;
    let mut diagnostics = DiagnosticsMode::Off;
    let mut diagnostics_iterations = DIAGNOSTICS_DEFAULT_ITERATIONS;
    let mut diagnostics_runs = DIAGNOSTICS_DEFAULT_RUNS;
    let mut parse_flags = true;

    for arg in args {
        let arg = arg.into();
        if arg == "-v" || arg == "--version" {
            print_version = true;
            continue;
        }
        if parse_flags {
            if arg == "--" {
                parse_flags = false;
                continue;
            }
            if arg == "-p" {
                mode = Some(Mode::Preview);
                continue;
            }
            if arg == "-s" {
                mode = Some(Mode::SideBySide);
                continue;
            }
            if arg == "--diagnostics-open" || arg == "--diag-open" {
                diagnostics = DiagnosticsMode::OpenPipeline;
                continue;
            }
            #[cfg(debug_assertions)]
            if arg == "--diagnostics-nav" || arg == "--diag-nav" {
                diagnostics = DiagnosticsMode::NavPipeline;
                continue;
            }
            if let Some(v) = parse_kv_usize(&arg, "--diagnostics-iterations=", "--diag-iterations=")
            {
                diagnostics_iterations = v;
                continue;
            }
            if let Some(v) = parse_kv_usize(&arg, "--diagnostics-runs=", "--diag-runs=") {
                diagnostics_runs = v;
                continue;
            }
            if arg.to_str().is_some_and(|value| value.starts_with('-')) {
                continue;
            }
        }

        if path.is_none() {
            path = Some(PathBuf::from(arg));
        }
    }

    let mode_explicit = mode.is_some();
    let mode = mode.unwrap_or_else(|| {
        if path.is_some() {
            Mode::Preview
        } else {
            Mode::Edit
        }
    });

    LaunchOptions {
        mode,
        mode_explicit,
        path,
        print_version,
        diagnostics,
        diagnostics_iterations,
        diagnostics_runs,
    }
}

#[must_use]
pub const fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// On WSL, smithay-clipboard connects via Wayland and panics with
/// "Broken pipe (os error 32)" during window resize.  Clearing
/// `WAYLAND_DISPLAY` forces the clipboard backend to X11 (arboard),
/// which avoids the crash while keeping clipboard fully functional.
/// See <https://github.com/emilk/egui/issues/3805>.
///
/// The workaround is only applied when `libxkbcommon-x11.so` is present;
/// without it the X11 backend cannot initialise and the app would panic
/// on startup.  When the library is absent, Wayland remains active and
/// the user sees a diagnostic hint to install it.
#[cfg(target_os = "linux")]
pub fn apply_wsl_workarounds() {
    if let Ok(ver) = std::fs::read_to_string("/proc/version")
        && ver.to_ascii_lowercase().contains("microsoft")
    {
        if x11_keyboard_lib_available() {
            // SAFETY: called at the top of main() before any threads are
            // spawned, so there is no concurrent access to the environment.
            #[allow(unsafe_code)]
            unsafe {
                std::env::remove_var("WAYLAND_DISPLAY");
            }
        } else {
            eprintln!(
                "rustdown: WSL detected but libxkbcommon-x11.so not found; \
                 X11 clipboard workaround disabled. Install libxkbcommon-x11-dev \
                 to avoid resize crashes."
            );
        }
    }
}

/// Returns `true` when `libxkbcommon-x11.so` can be loaded by the dynamic
/// linker, meaning the X11 keyboard backend will work at runtime.
#[cfg(target_os = "linux")]
fn x11_keyboard_lib_available() -> bool {
    // SAFETY: libxkbcommon-x11 is a well-known system library with no
    // harmful init-time side effects.  We load only to probe availability
    // and the library is dropped immediately.
    #[allow(unsafe_code)]
    let result = unsafe { libloading::Library::new("libxkbcommon-x11.so") };
    result.is_ok()
}

/// Attach to the parent process's console so that `println!` output is
/// visible when the GUI-subsystem binary is invoked from `PowerShell` or cmd.
#[cfg(windows)]
pub fn attach_parent_console() {
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    // SAFETY: `AttachConsole` is a well-documented Win32 API.  Calling it
    // with ATTACH_PARENT_PROCESS is harmless even if there is no parent
    // console — it simply returns FALSE.
    #[allow(unsafe_code)]
    {
        unsafe extern "system" {
            safe fn AttachConsole(process_id: u32) -> i32;
        }
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
