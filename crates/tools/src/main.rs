use std::{env, ffi::OsString, process::ExitCode};

use slimenrf_tools::{notices, packaging, prerelease, public, reference_oracle};

const USAGE: &str = "Usage: slimenrf-tools <command> [arguments]\n\
Commands: notices, check-public, prepare-prerelease, reference-oracle, package-artifact, appimage-check, flatpak-check";

fn main() -> ExitCode {
    let mut arguments = env::args_os().skip(1);
    let Some(command) = arguments.next() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let args: Vec<OsString> = arguments.collect();
    let result = match command.to_str() {
        Some("--help" | "-h") if args.is_empty() => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("check-public") => {
            return match public::run(&args) {
                Ok(code) => ExitCode::from(code),
                Err(error) => {
                    eprintln!("{error}");
                    ExitCode::from(2)
                }
            };
        }
        Some("notices") => notices::run(&args),
        Some("prepare-prerelease") => prerelease::run(&args),
        Some("reference-oracle") => reference_oracle::run(&args),
        Some("package-artifact") => packaging::run(&args),
        Some("appimage-check") => packaging::check_appimage_tools(&args),
        Some("flatpak-check") => packaging::check_flatpak_tools(&args),
        _ => {
            eprintln!("Unknown command {:?}.\n{USAGE}", command);
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
