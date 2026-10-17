//! Console build of the converter. `chitra.exe` is a GUI-subsystem app on
//! Windows, so cmd and PowerShell don't wait for it; scripts should call this
//! binary instead. Same commands as `chitra`, without the viewer.

#[path = "../cli.rs"]
mod cli;

use std::ffi::OsString;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let first = args.first().and_then(|a| a.to_str()).map(str::to_owned);
    match first.as_deref() {
        Some("convert") => {
            args.remove(0);
            cli::run_convert(args)
        }
        Some("passport") => {
            args.remove(0);
            cli::run_passport(args)
        }
        Some("info") => {
            args.remove(0);
            cli::run_info(args)
        }
        Some("-V" | "--version") => {
            println!("{}", cli::version());
            ExitCode::SUCCESS
        }
        Some("-h" | "--help" | "help") => {
            print!("{}", cli::USAGE);
            ExitCode::SUCCESS
        }
        _ => {
            eprint!(
                "chitra-cli runs `convert`, `info` and `passport`; open photos with chitra.exe.\n\n{}",
                cli::USAGE
            );
            ExitCode::from(2)
        }
    }
}
