// Release builds on Windows are GUI apps (no console window when opened from
// Explorer); the CLI re-attaches to the parent console in `platform`.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cli;
mod platform;
mod viewer;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let first = args.first().and_then(|a| a.to_str()).map(str::to_owned);
    match first.as_deref() {
        Some("convert") => {
            platform::attach_console();
            args.remove(0);
            cli::run_convert(args)
        }
        Some("passport") => {
            platform::attach_console();
            args.remove(0);
            cli::run_passport(args)
        }
        Some("info") => {
            platform::attach_console();
            args.remove(0);
            cli::run_info(args)
        }
        Some("-h" | "--help" | "help") => {
            platform::attach_console();
            print!("{}", cli::USAGE);
            ExitCode::SUCCESS
        }
        // Undocumented: plays a video silently to check the system player
        // (used by CI).
        Some("--play-test") => {
            platform::attach_console();
            let Some(path) = args.get(1) else {
                return ExitCode::from(2);
            };
            match viewer::play_test(std::path::Path::new(path)) {
                Ok(report) => {
                    println!("{report}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::from(1)
                }
            }
        }
        Some("-V" | "--version") => {
            platform::attach_console();
            println!("{}", cli::version());
            ExitCode::SUCCESS
        }
        _ => {
            let path = args.into_iter().next().map(PathBuf::from);
            match viewer::run(path) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    platform::attach_console();
                    platform::show_error(&format!("Chitra could not start: {e}"));
                    ExitCode::from(1)
                }
            }
        }
    }
}
