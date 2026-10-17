//! Editor entry point.
// GUI subsystem: no console window is spawned next to the app. Console-style
// output (`--help`, `--pretty`, errors) re-attaches to the parent console
// via `platform::print_*`; test builds stay console-subsystem so `cargo test`
// output is visible when run directly.
#![cfg_attr(all(target_os = "windows", not(test)), windows_subsystem = "windows")]
mod app;
mod cli;
mod core;
mod crash;
mod error;
mod gfx;
mod layout;
#[cfg(any(test, feature = "mem-census"))]
mod mem;
#[cfg(any(test, feature = "mem-census"))]
#[global_allocator]
static GLOBAL_ALLOCATOR: mem::Tracked = mem::Tracked;
// Network access without a TLS stack: the updater's three requests go to
// whatever the OS already ships (urlmon/curl on Windows, curl/wget/python3
// elsewhere; never a script host — see src/net.rs).
mod net;
mod parse;
#[cfg(test)]
mod perf;
mod platform;
#[cfg(test)]
mod scratch_test;
mod settings;
mod ui;
mod update;
// update sits with the other top-level app concerns (settings, cli); see
// src/update.rs for the whole self-update story.

fn main() {
    // Before anything else: a release build has no console and aborts on
    // panic, so without this hook a crash leaves nothing behind (see
    // src/crash.rs).
    crash::install();
    let code = match cli::run(std::env::args().skip(1).collect()) {
        Ok(()) => 0,
        Err(e) => {
            platform::print_err_line(&format!("editor: {e}"));
            1
        }
    };
    std::process::exit(code);
}
