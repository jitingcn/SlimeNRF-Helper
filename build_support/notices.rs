use std::{
    env,
    error::Error,
    path::{Path, PathBuf},
    process::Command,
};

const PYTHON_PROBE: &str = "import sys\nif sys.version_info < (3, 11):\n    sys.exit('Python 3.11 or newer is required')\nimport tomllib\n";

pub(super) fn build_notices() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=SLIMENRF_PYTHON");
    println!("cargo:rerun-if-env-changed=PATH");
    let root =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing CARGO_MANIFEST_DIR")?);
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?)
        .join("THIRD_PARTY_NOTICES.txt");
    let mut command = python_command()?;
    command
        .current_dir(&root)
        .env("PYTHONIOENCODING", "utf-8")
        .arg(root.join("scripts/generate_notices.py"))
        .arg("--root")
        .arg(&root)
        .arg("--output")
        .arg(&output)
        .arg("--refresh-inputs")
        .arg("--cargo-rerun");
    if env::var("PROFILE").as_deref() == Ok("debug") {
        command.arg("--development");
    }
    let status = command
        .status()
        .map_err(|error| format!("Cannot run the notices generator: {error}"))?;
    if !status.success() {
        return Err(format!(
            "Notices generation failed ({status}); review the generator diagnostics above. No source-root fallback is used."
        )
        .into());
    }
    Ok(())
}

fn python_command() -> Result<Command, String> {
    if let Some(executable) = env::var_os("SLIMENRF_PYTHON") {
        let command = Command::new(executable);
        probe_python(&command).map_err(|error| {
            format!(
                "SLIMENRF_PYTHON {:?} must name one Python 3.11+ executable with tomllib, not shell command text: {error}",
                command.get_program()
            )
        })?;
        return Ok(command);
    }
    let candidates: &[(&str, &[&str])] = if cfg!(windows) {
        &[("py", &["-3"]), ("python3", &[]), ("python", &[])]
    } else {
        &[("python3", &[]), ("python", &[])]
    };
    let mut failures = Vec::new();
    for (program, arguments) in candidates {
        let mut command = Command::new(program);
        command.args(*arguments);
        match probe_python(&command) {
            Ok(()) => return Ok(command),
            Err(error) => failures.push(format!("{program} {arguments:?}: {error}")),
        }
    }
    match uv_python_command() {
        Ok(command) => return Ok(command),
        Err(error) => failures.push(format!("uv python find: {error}")),
    }
    Err(format!(
        "Building SlimeNRF Helper requires Python 3.11+ with tomllib. Install it on PATH, make an installed interpreter available to uv, or set SLIMENRF_PYTHON to one interpreter executable/path. uv only searches installed interpreters and never downloads Python. Probes failed:\n{}",
        failures.join("\n")
    ))
}

fn uv_python_command() -> Result<Command, String> {
    let output = Command::new("uv")
        .args([
            "python",
            "find",
            "--no-project",
            "--system",
            "--no-python-downloads",
            "--offline",
            "--no-config",
            ">=3.11",
        ])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "{}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let output = std::str::from_utf8(&output.stdout)
        .map_err(|error| format!("interpreter path is not UTF-8: {error}"))?;
    let path = output
        .strip_suffix("\r\n")
        .or_else(|| output.strip_suffix('\n'))
        .unwrap_or(output);
    if path.is_empty()
        || path.chars().any(|ch| matches!(ch, '\r' | '\n' | '\0'))
        || !Path::new(path).is_absolute()
    {
        return Err("expected one absolute interpreter path from uv".to_owned());
    }
    let command = Command::new(path);
    probe_python(&command).map_err(|error| format!("{path:?} failed its Python probe: {error}"))?;
    Ok(command)
}

fn probe_python(command: &Command) -> Result<(), String> {
    let output = Command::new(command.get_program())
        .args(command.get_args())
        .args(["-c", PYTHON_PROBE])
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}
