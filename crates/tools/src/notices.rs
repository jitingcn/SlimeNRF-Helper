//! Frozen, reviewed third-party notice validation and rendering.

mod atomic;
mod model;
mod render;

#[cfg(test)]
mod tests;

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use atomic::{reject_input_output, write_atomic};
use model::{cargo_rerun_paths, fallback_rerun_paths, load_inputs};
use render::render;

#[cfg(test)]
use model::{Loaded, semantic_manifest_hash};

const INPUT: &str = "scripts/notices-inputs.json";
const REVIEW_GUIDE: &str = "docs/DEVELOPMENT.md#license-notices";
const MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "crates/app/Cargo.toml",
    "crates/firmware-image/Cargo.toml",
    "crates/recording/Cargo.toml",
    "crates/terminal-core/Cargo.toml",
    "crates/tools/Cargo.toml",
];
const VENDOR_LICENSES: &[&str] = &[
    "crates/recording/vendor/vqf/LICENSE",
    "crates/recording/vendor/vqf/LICENSE-SlimeVR",
    "crates/recording/vendor/vqf/cmsis/LICENSE",
];
const GENERATOR_INPUTS: &[&str] = &[
    "crates/tools/src/notices.rs",
    "crates/tools/src/notices/model.rs",
    "crates/tools/src/notices/render.rs",
    "crates/tools/src/notices/atomic.rs",
];

fn digest(data: &[u8]) -> String {
    crate::lower_hex(&Sha256::digest(data))
}

struct Options {
    root: PathBuf,
    output: Option<PathBuf>,
    check: bool,
    refresh_inputs: bool,
    development: bool,
    cargo_rerun: bool,
}

type NoticeWriter<'a> = dyn Fn(&Path, &[u8]) -> Result<(), String> + 'a;

fn warn(message: &str, cargo_rerun: bool) {
    eprintln!("Warning: {message}");
    if cargo_rerun {
        println!(
            "cargo:warning={}",
            message.lines().collect::<Vec<_>>().join(" ")
        );
    }
}

fn emit_watches(paths: &[PathBuf]) {
    for path in paths {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn development_pending(
    options: &Options,
    output: &Path,
    error: &str,
    write: &NoticeWriter<'_>,
) -> Result<(), String> {
    let message = format!(
        "DEVELOPMENT LICENSE REVIEW PENDING: {error}. Review dependencies and sources: \
         {REVIEW_GUIDE}. This development build is not redistribution-ready; the notice inventory is unavailable."
    );
    let paths = fallback_rerun_paths(&options.root);
    reject_input_output(output, &paths)?;
    write(output, format!("{message}\n").as_bytes())?;
    warn(&message, options.cargo_rerun);
    if options.cargo_rerun {
        emit_watches(&paths);
    }
    Ok(())
}

fn execute(options: &Options, write: &NoticeWriter<'_>) -> Result<(), String> {
    if !options.check && options.output.is_none() {
        return Err("--output is required unless --check is used".to_owned());
    }
    if options.check && options.development {
        return Err("--development cannot be combined with --check".to_owned());
    }

    let prepared = (|| {
        let loaded = load_inputs(&options.root, options.refresh_inputs)?;
        let paths = cargo_rerun_paths(&options.root, &loaded.manifest)?;
        let expected = render(&options.root, &loaded)?;
        Ok::<_, String>((loaded, paths, expected))
    })();
    let (loaded, paths, expected) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            if options.development
                && let Some(output) = &options.output
            {
                return development_pending(options, output, &error, write);
            }
            return Err(format!(
                "Notice generation failed: {error}. Review dependencies and sources: {REVIEW_GUIDE}"
            ));
        }
    };

    let mut previous_output = None;
    let mut output_existed = false;
    if options.check {
        if let Some(output) = &options.output
            && (!output.is_file() || fs::read(output).ok().as_deref() != Some(expected.as_slice()))
        {
            return Err(format!(
                "{} is missing or stale. Regenerate with --output PATH after reviewing the inputs.",
                output.display()
            ));
        }
    } else if let Some(output) = &options.output {
        reject_input_output(output, &paths)?;
        output_existed = output.is_file();
        if output_existed && loaded.candidate != loaded.original {
            previous_output = Some(fs::read(output).map_err(|error| {
                format!("Cannot read existing output {}: {error}", output.display())
            })?);
        }
        write(output, &expected)?;
    }

    if loaded.candidate != loaded.original {
        let input_path = options.root.join(INPUT);
        let commit = (|| {
            let current = fs::read(&input_path).map_err(|error| {
                format!(
                    "Cannot reread reviewed inputs {}: {error}",
                    input_path.display()
                )
            })?;
            if current != loaded.original {
                return Err(
                    "Reviewed inputs changed during generation; retry after review".to_owned(),
                );
            }
            write(&input_path, &loaded.candidate)
        })();
        if let Err(error) = commit {
            if !options.check
                && let Some(output) = &options.output
            {
                if output_existed {
                    if let Some(previous) = &previous_output {
                        write(output, previous).map_err(|restore_error| {
                            format!(
                                "Notice generation failed while restoring output: {restore_error}"
                            )
                        })?;
                    }
                } else {
                    fs::remove_file(output).map_err(|restore_error| {
                        format!("Notice generation failed while restoring output: {restore_error}")
                    })?;
                }
                if options.development {
                    return development_pending(options, output, &error, write);
                }
            }
            return Err(format!(
                "Notice generation failed: {error}. Review inputs: {REVIEW_GUIDE}"
            ));
        }
        warn(
            &format!(
                "Refreshed reviewed manifest hashes for profile/format-only changes; review and commit \
                 {INPUT}. Dependency or source changes still require review: {REVIEW_GUIDE}"
            ),
            options.cargo_rerun,
        );
    }

    if options.check {
        let package_count = loaded.manifest["packages"].as_array().map_or(0, Vec::len);
        println!(
            "Notices validated. {package_count} locked package versions, {} source texts.",
            loaded.sources.len()
        );
    } else if let Some(output) = &options.output {
        println!("Wrote {} ({} bytes).", output.display(), expected.len());
    }
    if options.cargo_rerun {
        emit_watches(&paths);
    }
    Ok(())
}

fn absolute_root(path: PathBuf) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| format!("Cannot resolve workspace root: {error}"))?
            .join(path)
    };
    Ok(fs::canonicalize(&absolute).unwrap_or(absolute))
}

/// Writes a notice artifact from strictly validated, frozen inputs without refreshing them.
///
/// # Errors
/// Returns an error for stale or invalid reviewed inputs, an unsafe output path, or a failed write.
pub fn write_notices(root: &Path, output: &Path) -> Result<(), String> {
    execute(
        &Options {
            root: absolute_root(root.to_path_buf())?,
            output: Some(output.to_path_buf()),
            check: false,
            refresh_inputs: false,
            development: false,
            cargo_rerun: false,
        },
        &write_atomic,
    )
}

/// Generates build notices, permitting only reviewed profile/format-only hash refreshes.
///
/// The `debug` profile can emit a non-redistributable review-pending diagnostic;
/// all other profiles reject invalid notice inputs. Cargo input watches are emitted.
///
/// # Errors
/// Returns an error for failed writes or, outside debug, any invalid reviewed input.
pub fn write_build_notices(root: &Path, output: &Path, profile: &str) -> Result<(), String> {
    execute(
        &Options {
            root: absolute_root(root.to_path_buf())?,
            output: Some(output.to_path_buf()),
            check: false,
            refresh_inputs: true,
            development: profile == "debug",
            cargo_rerun: true,
        },
        &write_atomic,
    )
}

/// Runs the notices maintenance command using the supplied arguments.
///
/// # Errors
/// Returns an error for invalid options, stale or malformed reviewed data, unsafe
/// destinations, or failed output and restricted-refresh transactions.
pub fn run(args: &[OsString]) -> Result<(), String> {
    let mut options = Options {
        root: crate::workspace_root(),
        output: None,
        check: false,
        refresh_inputs: false,
        development: false,
        cargo_rerun: false,
    };
    let mut arguments = args.iter();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--check") => options.check = true,
            Some("--refresh-inputs") => options.refresh_inputs = true,
            Some("--development") => options.development = true,
            Some("--cargo-rerun") => options.cargo_rerun = true,
            Some("--root") => {
                options.root = PathBuf::from(arguments.next().ok_or("--root requires a path")?);
            }
            Some("--output") => {
                options.output = Some(PathBuf::from(
                    arguments.next().ok_or("--output requires a path")?,
                ));
            }
            Some("--help" | "-h") => {
                println!(
                    "Usage: slimenrf-tools notices [--root PATH] [--check] [--output PATH] \
                     [--refresh-inputs] [--development] [--cargo-rerun]\n\
                     --check validates inputs; with --output it also compares the artifact.\n\
                     --output is required unless --check is used.\n\
                     --refresh-inputs only updates manifest hashes for reviewed profile/format changes.\n\
                     --development emits a non-redistributable pending diagnostic on validation failure."
                );
                return Ok(());
            }
            Some(value) if value.starts_with("--root=") => {
                options.root = PathBuf::from(&value[7..]);
            }
            Some(value) if value.starts_with("--output=") => {
                options.output = Some(PathBuf::from(&value[9..]));
            }
            _ => return Err(format!("Unknown notices argument: {argument:?}")),
        }
    }
    options.root = absolute_root(options.root)?;
    execute(&options, &write_atomic)
}
