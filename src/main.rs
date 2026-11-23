#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod collect;
mod format;
mod history;
mod model;
mod sampler;
mod settings;
mod ui;

use std::time::Duration;

const APP_NAME: &str = "OpenTemp";

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(());
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("{APP_NAME} {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--json") {
        dump_json();
        return Ok(());
    }
    ui::run()
}

fn print_help() {
    println!(
        "{APP_NAME} {} - hardware monitor\n\n\
         USAGE:\n    opentemp [OPTIONS]\n\n\
         OPTIONS:\n    --json       Print one snapshot of every reading as JSON and exit\n    \
         --version    Print the version\n    -h, --help   Print this help",
        env!("CARGO_PKG_VERSION")
    );
}

/// Diagnostic mode: two samples one second apart (so rates are filled in),
/// printed as JSON. Useful for bug reports and scripting.
fn dump_json() {
    #[cfg(windows)]
    attach_console();
    let mut c = collect::platform();
    let _ = c.sample();
    std::thread::sleep(Duration::from_secs(1));
    let mut snap = c.sample();
    history::derive_sensors(&mut snap);
    collect::trim_processes(&mut snap.processes, 20);
    let out = serde_json::json!({ "info": c.static_info(), "snapshot": snap });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}

/// Release builds use the GUI subsystem; reattach to the parent console so
/// `--json` output is visible when run from a terminal.
#[cfg(windows)]
fn attach_console() {
    unsafe extern "system" {
        fn AttachConsole(pid: u32) -> i32;
    }
    // SAFETY: ATTACH_PARENT_PROCESS; failure (no parent console) is harmless.
    unsafe {
        AttachConsole(u32::MAX);
    }
}
