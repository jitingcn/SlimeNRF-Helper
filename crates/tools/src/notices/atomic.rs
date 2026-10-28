use std::{
    ffi::OsString,
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

// Resolve an existing ancestor as well as the missing suffix: a pending build must
// not recreate a missing reviewed input under the guise of a notice artifact.
fn resolved_path(path: &Path) -> Result<PathBuf, String> {
    let mut ancestor = path.to_path_buf();
    let mut suffix: Vec<OsString> = Vec::new();
    loop {
        match fs::canonicalize(&ancestor) {
            Ok(mut resolved) => {
                for component in suffix.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let Some(name) = ancestor.file_name() else {
                    return Err(format!("Cannot resolve {}: {error}", path.display()));
                };
                suffix.push(name.to_owned());
                if !ancestor.pop() {
                    return Err(format!("Cannot resolve {}: {error}", path.display()));
                }
                if ancestor.as_os_str().is_empty() {
                    ancestor.push(".");
                }
            }
            Err(error) => return Err(format!("Cannot resolve {}: {error}", path.display())),
        }
    }
}

fn same_resolved_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

pub(super) fn reject_input_output(output: &Path, inputs: &[PathBuf]) -> Result<(), String> {
    let output_resolved = resolved_path(output)?;
    let output_handle = if output.is_file() {
        Some(
            same_file::Handle::from_path(output)
                .map_err(|error| format!("Cannot identify output {}: {error}", output.display()))?,
        )
    } else {
        None
    };
    for input in inputs {
        let input_resolved = resolved_path(input)?;
        let same_path = same_resolved_path(&output_resolved, &input_resolved);
        let same_file = if !same_path && input.is_file() {
            if let Some(output_handle) = &output_handle {
                let handle = same_file::Handle::from_path(input).map_err(|error| {
                    format!(
                        "Cannot identify reviewed input {}: {error}",
                        input.display()
                    )
                })?;
                *output_handle == handle
            } else {
                false
            }
        } else {
            false
        };
        if same_path || same_file {
            return Err(format!(
                "Output must not overwrite a reviewed input: {}",
                input.display()
            ));
        }
    }
    Ok(())
}

pub(super) fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let previous_permissions = match fs::metadata(path) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(format!("Cannot inspect {}: {error}", path.display())),
    };
    let mut builder = tempfile::Builder::new();
    builder.prefix(".notices-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Creating with 0666 lets the OS apply the process umask to new artifacts.
        builder.permissions(fs::Permissions::from_mode(0o666));
    }
    let mut temporary = builder
        .tempfile_in(directory)
        .map_err(|error| format!("Cannot stage {}: {error}", path.display()))?;
    temporary
        .write_all(data)
        .map_err(|error| format!("Cannot write {}: {error}", path.display()))?;
    if let Some(permissions) = previous_permissions {
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(|error| {
                format!(
                    "Cannot preserve permissions for {}: {error}",
                    path.display()
                )
            })?;
    }
    temporary
        .persist(path)
        .map_err(|error| format!("Cannot replace {}: {}", path.display(), error.error))?;
    Ok(())
}
