//! Linux release packaging using Cargo's selected executable.

use std::ffi::OsString;

#[cfg(target_os = "linux")]
mod appimage;
#[cfg(target_os = "linux")]
mod flatpak;
#[cfg(all(test, target_os = "linux"))]
mod tests;

/// Package a Cargo-selected application artifact or build a Flatpak bundle.
///
/// # Errors
/// Returns an error if the metadata, executable, package tools or destination are invalid.
pub fn run(args: &[OsString]) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        package(args)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Err("Linux packaging requires a native Linux environment.".into())
    }
}

/// Check the pinned AppImage tooling and native client libraries.
///
/// # Errors
/// Returns an error when the host, tools, checksums or prerequisites are unsuitable.
pub fn check_appimage_tools(args: &[OsString]) -> Result<(), String> {
    if !args.is_empty() {
        return Err("Usage: slimenrf-tools appimage-check".into());
    }
    #[cfg(target_os = "linux")]
    {
        appimage::check_tools()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("AppImage packaging requires a native Linux environment.".into())
    }
}

/// Check the native Flatpak build prerequisites.
///
/// # Errors
/// Returns an error when arguments, required tools, runtime or SDK are unsuitable.
pub fn check_flatpak_tools(args: &[OsString]) -> Result<(), String> {
    let no_firmware = match args {
        [] => false,
        [option] if option == "--no-firmware" => true,
        _ => return Err("Usage: slimenrf-tools flatpak-check [--no-firmware]".into()),
    };
    #[cfg(target_os = "linux")]
    {
        flatpak::check_tools(&crate::workspace_root(), no_firmware).map(|_| ())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = no_firmware;
        Err("Flatpak packaging requires a native Linux environment.".into())
    }
}

#[cfg(target_os = "linux")]
use std::{
    collections::HashSet,
    ffi::CString,
    fs::{self, File, Metadata, OpenOptions, Permissions},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
};

#[cfg(target_os = "linux")]
use serde::Deserialize;

#[cfg(target_os = "linux")]
const APP: &str = "slimenrf-helper";

#[cfg(target_os = "linux")]
#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: Vec<String>,
}

#[cfg(target_os = "linux")]
#[derive(Deserialize)]
struct CargoPackage {
    name: String,
    version: String,
    id: String,
    manifest_path: PathBuf,
}

// Resolve existing components, while permitting Cargo metadata fixtures whose final
// manifest has not been created. This is equivalent to Path.resolve(strict=False).
#[cfg(target_os = "linux")]
fn resolve_weak(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("Cannot resolve current directory: {e}"))?
            .join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                resolved.push(name);
                match fs::canonicalize(&resolved) {
                    Ok(path) => resolved = path,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                        ) => {}
                    Err(error) => {
                        return Err(format!("Cannot resolve {}: {error}", resolved.display()));
                    }
                }
            }
        }
    }
    Ok(resolved)
}

#[cfg(target_os = "linux")]
fn package(args: &[OsString]) -> Result<(), String> {
    if args.len() < 5 {
        return Err("Usage: slimenrf-tools package-artifact ROOT METADATA ARTIFACTS NO_FIRMWARE FORCE [elf|appimage|flatpak]".into());
    }
    let root = resolve_weak(Path::new(&args[0]))?;
    let metadata_file =
        File::open(&args[1]).map_err(|e| format!("Cannot open Cargo metadata: {e}"))?;
    let metadata: CargoMetadata = serde_json::from_reader(metadata_file)
        .map_err(|e| format!("Cannot read Cargo metadata: {e}"))?;
    let mut matches = Vec::new();
    for package in &metadata.packages {
        if package.name == APP
            && resolve_weak(&package.manifest_path)? == root.join("crates/app/Cargo.toml")
            && metadata.workspace_members.contains(&package.id)
        {
            matches.push(package);
        }
    }
    let [package] = matches.as_slice() else {
        return Err(
            "Cargo metadata must identify exactly one workspace slimenrf-helper application."
                .into(),
        );
    };
    let version = &package.version;
    let version_pattern = regex::Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)*$")
        .map_err(|e| format!("Invalid version pattern: {e}"))?;
    if !version_pattern.is_match(version) {
        return Err(format!("Unsafe package version: {version:?}"));
    }
    let format = match args.get(5) {
        Some(value) => value.to_str().ok_or("Package format must be UTF-8")?,
        None => "elf",
    };
    if !matches!(format, "elf" | "appimage" | "flatpak") {
        return Err(format!("Unknown package format: {format}"));
    }
    let no_firmware = args[3] == "1";
    let force = args[4] == "1";
    let variant = if no_firmware { "-no-firmware" } else { "" };
    if format == "flatpak" {
        let (_stage, bundle, architecture) = flatpak::build(&root, no_firmware, version)?;
        let name = format!("{APP}-{version}-linux-{architecture}{variant}.flatpak");
        let mut source = open_regular(&bundle)?;
        publish_package(&root, &mut source, &name, force, false)?;
        println!(
            "Flatpak: source-built inside the Freedesktop SDK; runtimes are not included in the bundle."
        );
        println!(
            "Full and no-firmware variants use the same app ID/branch and replace each other."
        );
        println!(
            "Compatibility permissions include host files and all devices; host ACLs, udev, BlueZ and real writable FAT/block UF2 mounts remain required."
        );
        return Ok(());
    }
    let executable = select_executable(Path::new(&args[2]), &package.id)?;
    let executable = if executable.is_absolute() {
        executable
    } else {
        root.join(executable)
    };
    let mut source = open_regular(&executable)?;
    let (architecture, dynamic) = elf_identity(&mut source)?;
    let mut name = format!("{APP}-{version}-linux-{architecture}{variant}");
    let (_stage, mut source) = if format == "appimage" {
        let (stage, image) = appimage::build(&root, &mut source, architecture, version)?;
        let mut image = open_regular(&image)?;
        let (image_architecture, _) = elf_identity(&mut image)?;
        if image_architecture != architecture {
            return Err("AppImage architecture differs from Cargo's executable.".into());
        }
        name.push_str(".AppImage");
        (Some(stage), image)
    } else {
        (None, source)
    };
    publish_package(&root, &mut source, &name, force, true)?;
    if format == "appimage" {
        println!(
            "AppImage: bundled desktop/client libraries; host libc, graphics drivers/loaders, fonts, libudev, and desktop/device services are required."
        );
        println!("Without FUSE, run the AppImage with --appimage-extract-and-run.");
    } else if dynamic {
        println!(
            "Dynamic Linux ELF: host system libraries/loader are required; no system libraries were copied."
        );
    } else {
        println!("Linux ELF has no dynamic program headers; no system libraries were copied.");
    }
    println!(
        "This package does not promise compatibility with every Linux distribution or desktop session."
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn select_executable(artifacts: &Path, package_id: &str) -> Result<PathBuf, String> {
    let stream = File::open(artifacts).map_err(|e| format!("Cannot open Cargo artifacts: {e}"))?;
    let mut executables = HashSet::new();
    for line in BufReader::new(stream).lines() {
        let line = line.map_err(|e| format!("Cannot read Cargo artifacts: {e}"))?;
        let value: serde_json::Value =
            serde_json::from_str(&line).map_err(|e| format!("Cannot parse Cargo artifact: {e}"))?;
        if value.get("reason").and_then(|v| v.as_str()) != Some("compiler-artifact")
            || value.get("package_id").and_then(|v| v.as_str()) != Some(package_id)
        {
            continue;
        }
        let target = value.get("target").ok_or("Cargo artifact has no target")?;
        if target.get("name").and_then(|v| v.as_str()) != Some(APP) {
            continue;
        }
        let kind = target
            .get("kind")
            .and_then(|v| v.as_array())
            .ok_or("Cargo artifact has no target kind")?;
        if !kind.iter().any(|item| item.as_str() == Some("bin")) {
            continue;
        }
        let test = value
            .get("profile")
            .and_then(|p| p.get("test"))
            .and_then(|v| v.as_bool())
            .ok_or("Cargo artifact has no profile test flag")?;
        if !test
            && let Some(path) = value
                .get("executable")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        {
            executables.insert(PathBuf::from(path));
        }
    }
    if executables.len() != 1 {
        return Err(
            "The successful Release build must report exactly one application executable.".into(),
        );
    }
    executables
        .into_iter()
        .next()
        .ok_or_else(|| "Missing Cargo executable".into())
}

#[cfg(target_os = "linux")]
fn open_regular(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
    if !file
        .metadata()
        .map_err(|e| format!("Cannot stat {}: {e}", path.display()))?
        .is_file()
    {
        return Err(format!("Not a regular file: {}", path.display()));
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
fn elf_identity(file: &mut File) -> Result<(&'static str, bool), String> {
    let mut header = [0_u8; 64];
    if file.read_exact(&mut header).is_err() || &header[..4] != b"\x7fELF" {
        return Err("Cargo's application executable is not an ELF file.".into());
    }
    let class = header[4];
    let encoding = header[5];
    if !matches!(class, 1 | 2) || !matches!(encoding, 1 | 2) || !matches!(header[7], 0 | 3) {
        return Err("Unsupported or non-Linux ELF identity.".into());
    }
    let u16_at = |offset: usize| {
        let bytes = [header[offset], header[offset + 1]];
        if encoding == 1 {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        }
    };
    let u32_at = |offset: usize| {
        let bytes: [u8; 4] = header[offset..offset + 4]
            .try_into()
            .expect("ELF header slice has fixed length");
        if encoding == 1 {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        }
    };
    let u64_at = |offset: usize| {
        let bytes: [u8; 8] = header[offset..offset + 8]
            .try_into()
            .expect("ELF header slice has fixed length");
        if encoding == 1 {
            u64::from_le_bytes(bytes)
        } else {
            u64::from_be_bytes(bytes)
        }
    };
    let elf_type = u16_at(16);
    let machine = u16_at(18);
    if !matches!(elf_type, 2 | 3) {
        return Err("Cargo's ELF is not an executable or position-independent executable.".into());
    }
    let architecture = match (machine, class, encoding) {
        (3, 1, _) => "x86",
        (62, 2, _) => "x64",
        (40, 1, _) => "arm",
        (183, 2, _) => "arm64",
        (20, 1, _) => "ppc",
        (21, 2, 1) => "ppc64le",
        (21, 2, 2) => "ppc64",
        (22, 2, _) => "s390x",
        (243, 1, _) => "riscv32",
        (243, 2, _) => "riscv64",
        (258, 2, _) => "loongarch64",
        _ => {
            return Err(format!(
                "Unsupported Linux ELF architecture: machine={machine}, class={class}"
            ));
        }
    };
    let (offset, entry_size, count, minimum) = if class == 2 {
        (u64_at(32), u64::from(u16_at(54)), u64::from(u16_at(56)), 56)
    } else {
        (
            u64::from(u32_at(28)),
            u64::from(u16_at(42)),
            u64::from(u16_at(44)),
            32,
        )
    };
    let size = file
        .metadata()
        .map_err(|e| format!("Cannot stat ELF: {e}"))?
        .len();
    if count == 0
        || entry_size < minimum
        || offset
            .checked_add(entry_size * count)
            .is_none_or(|end| end > size)
    {
        return Err("Invalid ELF program-header table.".into());
    }
    let mut dynamic = false;
    for index in 0..count {
        file.seek(SeekFrom::Start(offset + index * entry_size))
            .map_err(|e| format!("Cannot seek ELF: {e}"))?;
        let mut bytes = [0_u8; 4];
        file.read_exact(&mut bytes)
            .map_err(|e| format!("Cannot read ELF program header: {e}"))?;
        let kind = if encoding == 1 {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        };
        dynamic |= matches!(kind, 2 | 3);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|e| format!("Cannot rewind ELF: {e}"))?;
    Ok((architecture, dynamic))
}

#[cfg(target_os = "linux")]
fn same_bytes(left: &mut File, right: &mut File) -> io::Result<bool> {
    left.seek(SeekFrom::Start(0))?;
    right.seek(SeekFrom::Start(0))?;
    fn read_chunk(file: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
        let mut used = 0;
        while used < buffer.len() {
            match file.read(&mut buffer[used..]) {
                Ok(0) => break,
                Ok(count) => used += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(used)
    }
    let mut left_buffer = [0_u8; 64 * 1024];
    let mut right_buffer = [0_u8; 64 * 1024];
    loop {
        let left_count = read_chunk(left, &mut left_buffer)?;
        let right_count = read_chunk(right, &mut right_buffer)?;
        if left_count != right_count || left_buffer[..left_count] != right_buffer[..right_count] {
            return Ok(false);
        }
        if left_count == 0 {
            return Ok(true);
        }
    }
}

#[cfg(target_os = "linux")]
fn identity(metadata: &Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.size(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

#[cfg(target_os = "linux")]
fn open_directory(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| format!("Cannot open package directory {}: {e}", path.display()))
}

#[cfg(target_os = "linux")]
fn entry_name(name: &str) -> Result<CString, String> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(format!("Invalid package filename: {name:?}"));
    }
    CString::new(name).map_err(|_| format!("Invalid package filename: {name:?}"))
}

#[cfg(target_os = "linux")]
fn open_at(directory: &File, name: &CString, flags: i32, mode: libc::mode_t) -> io::Result<File> {
    // SAFETY: directory is an open directory descriptor and name is a terminated C string.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC,
            mode,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
fn regular_at(directory: &File, name: &CString) -> io::Result<File> {
    let file = open_at(
        directory,
        name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Not a regular package destination",
        ));
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
fn random_temporary_name(name: &str) -> Result<CString, String> {
    let mut random = [0_u8; 12];
    let mut filled = 0;
    while filled < random.len() {
        // SAFETY: the remaining byte slice is writable for its full length.
        let result = unsafe {
            libc::getrandom(
                random[filled..].as_mut_ptr().cast(),
                random.len() - filled,
                0,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!(
                "Cannot obtain temporary filename randomness: {error}"
            ));
        }
        if result == 0 {
            return Err("Cannot obtain temporary filename randomness".into());
        }
        filled += result as usize;
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut temporary = String::with_capacity(name.len() + 30);
    temporary.push('.');
    temporary.push_str(name);
    temporary.push('.');
    for byte in random {
        temporary.push(char::from(HEX[usize::from(byte >> 4)]));
        temporary.push(char::from(HEX[usize::from(byte & 15)]));
    }
    temporary.push_str(".tmp");
    entry_name(&temporary)
}

#[cfg(target_os = "linux")]
struct TemporaryEntry<'a> {
    directory: &'a File,
    name: CString,
}

#[cfg(target_os = "linux")]
impl Drop for TemporaryEntry<'_> {
    fn drop(&mut self) {
        // SAFETY: both the directory descriptor and the terminated name remain valid.
        unsafe {
            libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0);
        }
    }
}

#[cfg(target_os = "linux")]
fn create_temporary(directory: &File, name: CString) -> Result<(TemporaryEntry<'_>, File), String> {
    let output = open_at(
        directory,
        &name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
        0o600,
    )
    .map_err(|e| format!("Cannot create temporary package: {e}"))?;
    Ok((TemporaryEntry { directory, name }, output))
}

#[cfg(target_os = "linux")]
fn publish_impl<F>(
    source: &mut File,
    directory: &File,
    name: &str,
    force: bool,
    executable: bool,
    before_commit: F,
) -> Result<&'static str, String>
where
    F: FnOnce() -> Result<(), String>,
{
    let destination = entry_name(name)?;
    let existing = match regular_at(directory, &destination) {
        Ok(file) => Some(file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("Cannot open package destination {name}: {error}")),
    };
    let mut previous = None;
    if let Some(mut existing) = existing {
        let status = existing
            .metadata()
            .map_err(|e| format!("Cannot stat package destination: {e}"))?;
        previous = Some(identity(&status));
        if same_bytes(source, &mut existing)
            .map_err(|e| format!("Cannot compare package destination: {e}"))?
        {
            let current = existing
                .metadata()
                .map_err(|e| format!("Cannot stat package destination: {e}"))?;
            if executable && current.permissions().mode() & 0o111 == 0 {
                return Err(
                    "Identical package is not executable; fix its permissions before packaging."
                        .into(),
                );
            }
            return Ok("Identical package already exists");
        }
        if !force {
            return Err(format!(
                "Refusing to overwrite a different existing package: {name} (use --force)"
            ));
        }
    }
    let (temporary, mut output) = create_temporary(directory, random_temporary_name(name)?)?;
    source
        .seek(SeekFrom::Start(0))
        .map_err(|e| format!("Cannot rewind package source: {e}"))?;
    io::copy(&mut *source, &mut output).map_err(|e| format!("Cannot copy package source: {e}"))?;
    let source_mode = source
        .metadata()
        .map_err(|e| format!("Cannot stat package source: {e}"))?
        .permissions()
        .mode()
        & 0o777;
    let mode = if executable {
        source_mode | 0o111
    } else {
        source_mode & !0o111
    };
    output
        .set_permissions(Permissions::from_mode(mode))
        .map_err(|e| format!("Cannot set package permissions: {e}"))?;
    drop(output);
    before_commit()?;
    if let Some(previous) = previous {
        let current = regular_at(directory, &destination)
            .and_then(|file| file.metadata())
            .map_err(|_| format!("Package destination changed while publishing: {name}"))?;
        if identity(&current) != previous {
            return Err(format!(
                "Package destination changed while publishing: {name}"
            ));
        }
        // SAFETY: descriptors and C strings remain valid; renameat atomically replaces the directory entry.
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                temporary.name.as_ptr(),
                directory.as_raw_fd(),
                destination.as_ptr(),
            )
        } != 0
        {
            return Err(format!(
                "Cannot replace package {name}: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(if executable {
            "Replaced package executable"
        } else {
            "Replaced Flatpak bundle"
        })
    } else {
        // SAFETY: both names are relative to the open directory; linkat refuses an existing destination.
        if unsafe {
            libc::linkat(
                directory.as_raw_fd(),
                temporary.name.as_ptr(),
                directory.as_raw_fd(),
                destination.as_ptr(),
                0,
            )
        } != 0
        {
            return Err(format!(
                "Cannot publish package {name}: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(if executable {
            "Packaged single executable"
        } else {
            "Packaged Flatpak bundle"
        })
    }
}

#[cfg(target_os = "linux")]
fn publish_package(
    root: &Path,
    source: &mut File,
    name: &str,
    force: bool,
    executable: bool,
) -> Result<(), String> {
    let dist = root.join("dist");
    match fs::create_dir(&dist) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "Cannot create package directory {}: {error}",
                dist.display()
            ));
        }
    }
    let directory = open_directory(&dist)?;
    let result = publish_impl(source, &directory, name, force, executable, || Ok(()))?;
    println!("{result}: {}", dist.join(name).display());
    Ok(())
}
