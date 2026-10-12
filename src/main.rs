//! Readitor entry point.
// GUI subsystem: no console window is spawned next to the app. Console-style
// output (`--help`, `--pretty`, errors) re-attaches to the parent console
// via `platform::print_*`; test builds stay console-subsystem so `cargo test`
// output is visible when run directly.
#![cfg_attr(all(target_os = "windows", not(test)), windows_subsystem = "windows")]
mod app;
mod cli;
mod core;
mod error;
mod gfx;
mod layout;
#[cfg(any(test, feature = "mem-census"))]
mod mem;
#[cfg(any(test, feature = "mem-census"))]
#[global_allocator]
static GLOBAL_ALLOCATOR: mem::Tracked = mem::Tracked;
mod parse;
mod platform;
#[cfg(test)]
mod scratch_test;
mod settings;
mod ui;

fn main() {
    let code = match cli::run(std::env::args().skip(1).collect()) {
        Ok(()) => 0,
        Err(e) => {
            platform::print_err_line(&format!("readitor: {e}"));
            1
        }
    };
    std::process::exit(code);
}
