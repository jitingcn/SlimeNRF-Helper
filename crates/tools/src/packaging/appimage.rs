#[cfg(target_os = "linux")]
pub(super) use linux::{build, check_tools, home_directory};

#[cfg(not(target_os = "linux"))]
pub(super) fn check_tools() -> Result<(), String> {
    Err("AppImage packaging requires a native Linux environment.".to_owned())
}

#[cfg(not(target_os = "linux"))]
pub(super) fn build(
    _root: &std::path::Path,
    _source: &mut std::fs::File,
    _architecture: &str,
    _version: &str,
) -> Result<(tempfile::TempDir, std::path::PathBuf), String> {
    Err("AppImage packaging requires a native Linux environment.".to_owned())
}

#[cfg(target_os = "linux")]
mod linux {
    use sha2::{Digest, Sha256};
    use std::env;
    use std::ffi::{CStr, CString, OsString};
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Read, Seek, SeekFrom, Write};
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};

    // Release API SHA256 values; changing a pin requires an explicit source review.
    // https://api.github.com/repos/linuxdeploy/linuxdeploy/releases/tags/1-alpha-20251107-1
    // https://api.github.com/repos/AppImage/appimagetool/releases/tags/1.9.1
    pub(super) struct Tool {
        name: &'static str,
        repository: &'static str,
        release: &'static str,
        environment: &'static str,
        assets: &'static [(&'static str, &'static str, &'static str)],
    }

    pub(super) const LINUXDEPLOY: Tool = Tool {
        name: "linuxdeploy",
        repository: "linuxdeploy/linuxdeploy",
        release: "1-alpha-20251107-1",
        environment: "SLIMENRF_LINUXDEPLOY",
        assets: &[
            (
                "x64",
                "x86_64",
                "c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d",
            ),
            (
                "arm64",
                "aarch64",
                "620095110d693282b8ebeb244a95b5e911cf8f65f76c88b4b47d16ae6346fcff",
            ),
            (
                "x86",
                "i386",
                "a2a88d142aac42db779483ca07c10dbf318b27f514691107fc88a202faae17b5",
            ),
            (
                "arm",
                "armhf",
                "e359161979fa4bee50b92ce7102fb510299caebf34f711d983fba7a8f4bb1c2e",
            ),
        ],
    };
    pub(super) const APPIMAGETOOL: Tool = Tool {
        name: "appimagetool",
        repository: "AppImage/appimagetool",
        release: "1.9.1",
        environment: "SLIMENRF_APPIMAGETOOL",
        assets: &[
            (
                "x64",
                "x86_64",
                "ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0",
            ),
            (
                "arm64",
                "aarch64",
                "f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158",
            ),
            (
                "x86",
                "i686",
                "7ad9ff47c203aae0149b18f6df9e3018b2e2f470ea644a0413e3ded39e9e3bdb",
            ),
            (
                "arm",
                "armhf",
                "42b61cba5495d8aaf418a5c9a015a49b85ad92efabcbd3c341f1540440e4e23d",
            ),
        ],
    };

    // These dlopen dependencies need explicit deployment even if not DT_NEEDED.
    pub(super) const CLIENT_LIBRARIES: &[&str] = &[
        "libxkbcommon.so.0",
        "libxkbcommon-x11.so.0",
        "libfontconfig.so.1",
        "libfreetype.so.6",
        "libdbus-1.so.3",
    ];

    // The loader, libc, Wayland, udev and host graphics stack must remain coherent.
    const HOST_LIBRARIES: &[&str] = &[
        "ld-linux*.so*",
        "ld-musl*.so*",
        "libc.so*",
        "libm.so*",
        "libpthread.so*",
        "libdl.so*",
        "librt.so*",
        "libresolv.so*",
        "libnss_*.so*",
        "libutil.so*",
        "libanl.so*",
        "libBrokenLocale.so*",
        "libudev.so*",
        "libsystemd.so*",
        "libEGL.so*",
        "libGL.so*",
        "libGLX*.so*",
        "libGLES*.so*",
        "libOpenGL.so*",
        "libwayland*.so*",
        "libGLdispatch.so*",
        "libgbm.so*",
        "libdrm*.so*",
        "libvulkan*.so*",
        "libnvidia*.so*",
        "libcuda*.so*",
        "*_dri.so",
    ];

    pub(super) struct ToolSpec {
        pub(super) tool: &'static Tool,
        pub(super) path: PathBuf,
        pub(super) digest: &'static str,
        pub(super) url: String,
    }

    fn host_architecture() -> Result<&'static str, String> {
        let mut name = MaybeUninit::<libc::utsname>::uninit();
        // SAFETY: uname initializes the entire utsname on success.
        if unsafe { libc::uname(name.as_mut_ptr()) } != 0 {
            return Err(format!(
                "Could not determine host architecture: {}",
                io::Error::last_os_error()
            ));
        }
        // SAFETY: the successful uname call initialized the structure and its strings.
        let name = unsafe { name.assume_init() };
        // SAFETY: uname guarantees that machine is NUL-terminated.
        let machine = unsafe { CStr::from_ptr(name.machine.as_ptr()) }
            .to_string_lossy()
            .to_ascii_lowercase();
        match machine.as_str() {
            "x86_64" | "amd64" => Ok("x64"),
            "aarch64" | "arm64" => Ok("arm64"),
            "i386" | "i486" | "i586" | "i686" => Ok("x86"),
            "armv7l" | "armv8l" => Ok("arm"),
            _ => Err(format!(
                "No pinned AppImage build tools for this host architecture: {machine}"
            )),
        }
    }

    pub(super) fn asset_for(
        tool: &Tool,
        architecture: &str,
    ) -> Result<(&'static str, &'static str), String> {
        tool.assets
            .iter()
            .find(|(name, _, _)| *name == architecture)
            .map(|(_, asset, digest)| (*asset, *digest))
            .ok_or_else(|| {
                format!(
                    "No pinned {} AppImage for architecture: {architecture}",
                    tool.name
                )
            })
    }

    pub(crate) fn home_directory() -> Result<PathBuf, String> {
        if let Some(home) = env::var_os("HOME") {
            return Ok(if home.is_empty() {
                PathBuf::from("/")
            } else {
                PathBuf::from(home)
            });
        }
        // Match Path.home's passwd fallback when HOME is absent.
        let mut capacity = 1024usize;
        loop {
            let mut storage = vec![0u8; capacity];
            let mut entry = MaybeUninit::<libc::passwd>::uninit();
            let mut result = std::ptr::null_mut();
            // SAFETY: all output pointers refer to live writable storage of the supplied sizes;
            // getpwuid_r returns pointers into storage, which remains alive until copied below.
            let error = unsafe {
                libc::getpwuid_r(
                    libc::getuid(),
                    entry.as_mut_ptr(),
                    storage.as_mut_ptr().cast(),
                    storage.len(),
                    &mut result,
                )
            };
            if error == libc::ERANGE {
                capacity = capacity.checked_mul(2).ok_or_else(|| {
                    "Home-directory lookup exceeded addressable memory.".to_owned()
                })?;
                continue;
            }
            if error != 0 {
                return Err(format!(
                    "Could not determine home directory: {}",
                    io::Error::from_raw_os_error(error)
                ));
            }
            if result.is_null() {
                return Err("Could not determine home directory for the current user.".to_owned());
            }
            // SAFETY: a non-null result means getpwuid_r initialized entry and its pw_dir.
            let entry = unsafe { entry.assume_init() };
            if entry.pw_dir.is_null() {
                return Err("The current user has no home directory.".to_owned());
            }
            // SAFETY: pw_dir is a NUL-terminated string in the still-live lookup storage.
            let bytes = unsafe { CStr::from_ptr(entry.pw_dir) }.to_bytes().to_vec();
            return Ok(PathBuf::from(OsString::from_vec(bytes)));
        }
    }

    fn cache_home() -> Result<PathBuf, String> {
        match env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
            Some(path) => Ok(PathBuf::from(path)),
            None => Ok(home_directory()?.join(".cache")),
        }
    }

    pub(super) fn tool_spec_from(
        tool: &'static Tool,
        architecture: &str,
        cache: &Path,
        override_path: Option<&Path>,
    ) -> Result<ToolSpec, String> {
        let (asset, digest) = asset_for(tool, architecture)?;
        let filename = format!("{}-{asset}.AppImage", tool.name);
        if !cache.is_absolute() {
            return Err("XDG_CACHE_HOME must be absolute for AppImage tool discovery.".to_owned());
        }
        let path = override_path
            .filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| {
                cache
                    .join("slimenrf-helper")
                    .join("appimage-tools")
                    .join(tool.release)
                    .join(&filename)
            });
        if !path.is_absolute() {
            return Err(format!(
                "{} must name an absolute regular-file path.",
                tool.environment
            ));
        }
        let url = format!(
            "https://github.com/{}/releases/download/{}/{}",
            tool.repository, tool.release, filename
        );
        Ok(ToolSpec {
            tool,
            path,
            digest,
            url,
        })
    }

    fn tool_spec(tool: &'static Tool, architecture: &str) -> Result<ToolSpec, String> {
        let cache = cache_home()?;
        let override_path = env::var_os(tool.environment).map(PathBuf::from);
        tool_spec_from(tool, architecture, &cache, override_path.as_deref())
    }

    fn shell_quote(value: &str) -> String {
        if !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte))
        {
            value.to_owned()
        } else {
            format!("'{}'", value.replace('\'', "'\\''"))
        }
    }

    fn setup_message(spec: &ToolSpec) -> String {
        let tool = spec.tool;
        format!(
            "{} requires the pinned official {} AppImage.\n  Download: {}\n  SHA256: {}\n  \
             Save as: {}\n  Or set {} to an absolute regular-file path.\n  \
             No automatic download or system installation is performed.\n  \
             Example: curl --fail --location --proto '=https' --tlsv1.2 {} --output {}\n  \
             Create the parent directory first; packaging checks SHA256 before executing anything.",
            tool.name,
            tool.release,
            spec.url,
            spec.digest,
            spec.path.display(),
            tool.environment,
            shell_quote(&spec.url),
            shell_quote(&spec.path.to_string_lossy()),
        )
    }

    pub(super) fn copy_verified_tool(
        spec: &ToolSpec,
        destination: Option<&Path>,
    ) -> Result<(), String> {
        let copy = || -> Result<(), String> {
            let mut source = super::super::open_regular(&spec.path)?;
            let mut output = destination
                .map(|path| {
                    OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(path)
                        .map_err(|error| {
                            format!("Could not create staged tool {}: {error}", path.display())
                        })
                })
                .transpose()?;
            let mut digest = Sha256::new();
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let count = match source.read(&mut buffer) {
                    Ok(count) => count,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        return Err(format!(
                            "Could not read tool {}: {error}",
                            spec.path.display()
                        ));
                    }
                };
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count]);
                if let Some(output) = output.as_mut() {
                    output.write_all(&buffer[..count]).map_err(|error| {
                        format!("Could not stage tool {}: {error}", spec.path.display())
                    })?;
                }
            }
            if crate::lower_hex(&digest.finalize()) != spec.digest {
                return Err(format!(
                    "SHA256 mismatch for {}; refusing to execute unpinned bytes.",
                    spec.path.display()
                ));
            }
            if let Some(output) = output {
                output
                    .set_permissions(fs::Permissions::from_mode(0o755))
                    .map_err(|error| format!("Could not make staged tool executable: {error}"))?;
            }
            Ok(())
        };
        copy().map_err(|error| format!("{error}\n{}", setup_message(spec)))
    }

    fn executable(path: &Path) -> bool {
        if !path.is_file() {
            return false;
        }
        let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: path is a valid NUL-terminated pathname and access does not retain it.
        unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
    }

    fn which(program: &str) -> Option<PathBuf> {
        let search = env::var_os("PATH").unwrap_or_else(|| OsString::from("/bin:/usr/bin"));
        if search.is_empty() {
            return None;
        }
        env::split_paths(&search)
            .map(|directory| {
                if directory.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    directory
                }
            })
            .map(|directory| directory.join(program))
            .find(|path| executable(path))
    }

    fn checked_status(command: &mut Command) -> Result<(), String> {
        let status = command
            .status()
            .map_err(|error| format!("Could not run {command:?}: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("Command {command:?} failed with {status}"))
        }
    }

    fn checked_output(command: &mut Command) -> Result<Output, String> {
        let output = command
            .output()
            .map_err(|error| format!("Could not run {command:?}: {error}"))?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(format!(
                "Command {command:?} failed with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    fn read_header(path: &Path) -> Result<([u8; 20], usize), String> {
        let mut file = File::open(path)
            .map_err(|error| format!("Could not open {}: {error}", path.display()))?;
        let mut header = [0u8; 20];
        let mut length = 0;
        while length < header.len() {
            match file.read(&mut header[length..]) {
                Ok(0) => break,
                Ok(count) => length += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(format!("Could not read {}: {error}", path.display())),
            }
        }
        Ok((header, length))
    }

    pub(super) fn collect_client_libraries(
        listing: &str,
        native: &[u8],
    ) -> Result<Vec<PathBuf>, String> {
        let mut candidates: [Vec<&str>; CLIENT_LIBRARIES.len()] =
            std::array::from_fn(|_| Vec::new());
        for line in listing.lines() {
            let mut fields = line.split_whitespace();
            let Some(name) = fields.next() else { continue };
            let Some(index) = CLIENT_LIBRARIES
                .iter()
                .position(|candidate| *candidate == name)
            else {
                continue;
            };
            if let (Some(path), Some("=>"), Some(_)) =
                (fields.next_back(), fields.next_back(), fields.next())
            {
                candidates[index].push(path);
            }
        }
        let mut libraries = Vec::with_capacity(CLIENT_LIBRARIES.len());
        let mut missing = Vec::new();
        for (name, paths) in CLIENT_LIBRARIES.iter().zip(candidates) {
            let mut selected = None;
            for path in paths {
                let (header, length) = read_header(Path::new(path))?;
                let header = &header[..length];
                if header.starts_with(b"\x7fELF")
                    && header.get(4..6) == native.get(4..6)
                    && header.get(18..20) == native.get(18..20)
                {
                    selected = Some(PathBuf::from(path));
                    break;
                }
            }
            if let Some(path) = selected {
                libraries.push(path);
            } else {
                missing.push(*name);
            }
        }
        if !missing.is_empty() {
            return Err(format!(
                "AppImage client-library prerequisites missing from the native ldconfig cache: {}. \
                 Install the corresponding Wayland, xkbcommon, fontconfig, FreeType and D-Bus \
                 packages using your distribution's package manager.",
                missing.join(", ")
            ));
        }
        Ok(libraries)
    }

    fn client_libraries() -> Result<Vec<PathBuf>, String> {
        let ldconfig = which("ldconfig")
            .or_else(|| {
                ["/sbin/ldconfig", "/usr/sbin/ldconfig"]
                    .into_iter()
                    .map(PathBuf::from)
                    .find(|path| path.is_file())
            })
            .ok_or_else(|| {
                "AppImage dependency discovery requires ldconfig (from the host libc tools)."
                    .to_owned()
            })?;
        let output = checked_output(Command::new(ldconfig).arg("-p"))?;
        let listing = String::from_utf8(output.stdout)
            .map_err(|error| format!("ldconfig returned invalid text: {error}"))?;
        let (native, length) = read_header(Path::new("/proc/self/exe"))?;
        collect_client_libraries(&listing, &native[..length])
    }

    pub(crate) fn check_tools() -> Result<(), String> {
        let architecture = host_architecture()?;
        let mut errors = Vec::new();
        for tool in [&LINUXDEPLOY, &APPIMAGETOOL] {
            if let Err(error) =
                tool_spec(tool, architecture).and_then(|spec| copy_verified_tool(&spec, None))
            {
                errors.push(error);
            }
        }
        if which("desktop-file-validate").is_none() {
            errors.push(
                "desktop-file-validate is required (the desktop-file-utils package).".to_owned(),
            );
        }
        if which("file").is_none() {
            errors.push("file is required by appimagetool (the file package).".to_owned());
        }
        if let Err(error) = client_libraries() {
            errors.push(error);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n\n"))
        }
    }

    pub(super) fn deployment_environment(command: &mut Command, architecture: &str, version: &str) {
        // Older bundled strip cannot parse modern DT_RELR; inherited exclusions
        // and copyright settings must not override the declared deployment policy.
        command
            .env("APPIMAGE_EXTRACT_AND_RUN", "1")
            .env("ARCH", architecture)
            .env("VERSION", version)
            .env("LC_ALL", "C")
            .env("NO_STRIP", "1")
            .env_remove("LINUXDEPLOY_EXCLUDED_LIBRARIES")
            .env_remove("DISABLE_COPYRIGHT_FILES_DEPLOYMENT");
    }

    pub(super) fn copy_runtime(image: &Path, offset: &str, runtime: &Path) -> Result<(), String> {
        let offset = offset.trim();
        let invalid =
            || "Pinned appimagetool did not report a valid type-2 runtime offset.".to_owned();
        if offset.is_empty() || !offset.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
        let offset: u64 = offset.parse().map_err(|_| invalid())?;
        let image = super::super::open_regular(image)?;
        let size = image
            .metadata()
            .map_err(|error| format!("Could not inspect pinned appimagetool: {error}"))?
            .len();
        if offset == 0 || offset >= size {
            return Err(invalid());
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(runtime)
            .map_err(|error| {
                format!(
                    "Could not create AppImage runtime {}: {error}",
                    runtime.display()
                )
            })?;
        let copied = io::copy(&mut image.take(offset), &mut output)
            .map_err(|error| format!("Could not copy pinned AppImage runtime: {error}"))?;
        if copied != offset {
            return Err("Truncated pinned AppImage runtime.".to_owned());
        }
        Ok(())
    }

    pub(super) fn stage_executable(source: &mut File, destination: &Path) -> Result<(), String> {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .map_err(|error| {
                format!(
                    "Could not create staged executable {}: {error}",
                    destination.display()
                )
            })?;
        source
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("Could not seek Cargo executable: {error}"))?;
        io::copy(source, &mut output)
            .map_err(|error| format!("Could not stage Cargo executable: {error}"))?;
        output
            .set_permissions(fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("Could not make staged executable runnable: {error}"))?;
        Ok(())
    }

    pub(super) fn linuxdeploy_command(
        tool: &Path,
        appdir: &Path,
        executable: &Path,
        assets: &Path,
        libraries: &[PathBuf],
    ) -> Command {
        let mut command = Command::new(tool);
        command
            .arg("--appdir")
            .arg(appdir)
            .arg("--executable")
            .arg(executable)
            .arg("--desktop-file")
            .arg(assets.join("slimenrf-helper.desktop"))
            .arg("--icon-file")
            .arg(assets.join("slimenrf-helper.svg"))
            .arg("--custom-apprun")
            .arg(assets.join("AppRun"));
        for pattern in HOST_LIBRARIES {
            command.arg("--exclude-library").arg(pattern);
        }
        for library in libraries {
            command.arg("--library").arg(library);
        }
        command
    }

    pub(crate) fn build(
        root: &Path,
        source: &mut File,
        architecture: &str,
        version: &str,
    ) -> Result<(tempfile::TempDir, PathBuf), String> {
        let host = host_architecture()?;
        if architecture != host {
            return Err(format!(
                "AppImage dependency bundling requires a native build: Cargo ELF is {architecture}, host is {host}."
            ));
        }
        check_tools()?;
        let root = fs::canonicalize(root)
            .map_err(|error| format!("Could not resolve workspace {}: {error}", root.display()))?;
        let assets = root.join("scripts/appimage");
        let (appimage_architecture, _) = asset_for(&APPIMAGETOOL, architecture)?;
        let temporary = tempfile::Builder::new()
            .prefix("slimenrf-appimage-")
            .tempdir()
            .map_err(|error| format!("Could not create AppImage staging directory: {error}"))?;
        let stage = fs::canonicalize(temporary.path())
            .map_err(|error| format!("Could not resolve AppImage staging directory: {error}"))?;
        let mut images = Vec::new();
        let mut tools = Vec::new();
        for tool in [&LINUXDEPLOY, &APPIMAGETOOL] {
            let directory = stage.join(tool.name);
            fs::create_dir(&directory)
                .map_err(|error| format!("Could not create {}: {error}", directory.display()))?;
            let image = directory.join(format!("{}.AppImage", tool.name));
            copy_verified_tool(&tool_spec(tool, architecture)?, Some(&image))?;
            let mut extraction = Command::new(&image);
            extraction
                .arg("--appimage-extract")
                .current_dir(&directory)
                .stdout(Stdio::null());
            deployment_environment(&mut extraction, appimage_architecture, version);
            checked_status(&mut extraction)?;
            images.push(image);
            tools.push(directory.join("squashfs-root/AppRun"));
        }
        // Reuse the pinned tool's own type-2 runtime; appimagetool otherwise downloads one.
        let mut offset_command = Command::new(&images[1]);
        offset_command.arg("--appimage-offset");
        deployment_environment(&mut offset_command, appimage_architecture, version);
        let offset = checked_output(&mut offset_command)?;
        let offset = String::from_utf8(offset.stdout)
            .map_err(|error| format!("Invalid AppImage runtime offset text: {error}"))?;
        let runtime = stage.join("runtime");
        copy_runtime(&images[1], &offset, &runtime)?;

        let appdir = stage.join("SlimeNRF.AppDir");
        let executable = stage.join("slimenrf-helper");
        stage_executable(source, &executable)?;
        let libraries = client_libraries()?;
        let mut deploy = linuxdeploy_command(&tools[0], &appdir, &executable, &assets, &libraries);
        deploy.current_dir(&stage);
        deployment_environment(&mut deploy, appimage_architecture, version);
        checked_status(&mut deploy)?;
        for library in CLIENT_LIBRARIES {
            if !appdir.join("usr/lib").join(library).is_file() {
                return Err(format!(
                    "linuxdeploy omitted required client library: {library}"
                ));
            }
        }
        let licenses = appdir.join("usr/share/licenses/slimenrf-helper");
        fs::create_dir_all(&licenses).map_err(|error| {
            format!(
                "Could not create license directory {}: {error}",
                licenses.display()
            )
        })?;
        for name in ["LICENSE-MIT", "LICENSE-APACHE"] {
            fs::copy(root.join(name), licenses.join(name))
                .map_err(|error| format!("Could not stage {name}: {error}"))?;
        }
        crate::notices::write_notices(&root, &licenses.join("THIRD_PARTY_NOTICES.txt"))?;
        let mut validate = Command::new("desktop-file-validate");
        validate.arg(appdir.join("slimenrf-helper.desktop"));
        deployment_environment(&mut validate, appimage_architecture, version);
        checked_status(&mut validate)?;
        let image = stage.join("application.AppImage");
        let mut package = Command::new(&tools[1]);
        package
            .arg("--runtime-file")
            .arg(&runtime)
            .arg("--no-appstream")
            .arg(&appdir)
            .arg(&image)
            .current_dir(&stage);
        deployment_environment(&mut package, appimage_architecture, version);
        checked_status(&mut package)?;
        Ok((temporary, image))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::linux::{
        APPIMAGETOOL, CLIENT_LIBRARIES, LINUXDEPLOY, ToolSpec, asset_for, collect_client_libraries,
        copy_runtime, copy_verified_tool, deployment_environment, linuxdeploy_command,
        stage_executable, tool_spec_from,
    };
    use std::ffi::CString;
    use std::fs::{self, File};
    use std::io::{Seek, SeekFrom};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::Duration;

    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn fixture_spec(path: PathBuf, digest: &'static str) -> ToolSpec {
        ToolSpec {
            tool: &LINUXDEPLOY,
            path,
            digest,
            url: "https://example.invalid/linuxdeploy.AppImage".to_owned(),
        }
    }

    fn fixture_tool(root: &Path, name: &str, contents: &[u8]) -> PathBuf {
        let path = root.join(name);
        fs::write(&path, contents).expect("write tool fixture");
        path
    }

    #[test]
    fn pinned_tool_copy_verifies_actual_bytes_and_is_independent_of_cache_changes() {
        let dir = tempfile::tempdir().expect("create fixture directory");
        let source = fixture_tool(dir.path(), "cached.AppImage", b"abc");
        let staged = dir.path().join("staged.AppImage");
        let spec = fixture_spec(source.clone(), ABC_SHA256);

        copy_verified_tool(&spec, None).expect("verify pinned cache bytes");
        copy_verified_tool(&spec, Some(&staged)).expect("stage pinned cache bytes");
        fs::write(&source, b"replacement").expect("replace cached bytes");

        assert_eq!(
            fs::read(&staged).expect("read staged bytes").as_slice(),
            b"abc"
        );
        assert_eq!(
            fs::metadata(&staged)
                .expect("staged metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert!(
            copy_verified_tool(&spec, None)
                .expect_err("changed cache must fail")
                .contains("SHA256 mismatch")
        );
    }

    #[test]
    fn pinned_tool_hashes_and_stages_multiple_read_chunks() {
        let dir = tempfile::tempdir().expect("create fixture directory");
        let bytes = vec![b'a'; 1_000_000];
        let source = fixture_tool(dir.path(), "large.AppImage", &bytes);
        let staged = dir.path().join("staged.AppImage");
        let digest = "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0";
        copy_verified_tool(&fixture_spec(source, digest), Some(&staged))
            .expect("stage and hash all chunks");
        assert_eq!(fs::read(staged).expect("read staged bytes"), bytes);
    }

    #[test]
    fn tool_refuses_wrong_hash_and_does_not_overwrite_staging_file() {
        let dir = tempfile::tempdir().expect("create fixture directory");
        let source = fixture_tool(dir.path(), "cached.AppImage", b"wrong payload");
        let spec = fixture_spec(source, ABC_SHA256);
        let staged = fixture_tool(dir.path(), "staged.AppImage", b"existing");

        let error = copy_verified_tool(&spec, None).expect_err("unpinned tool must fail");
        assert!(error.contains("SHA256 mismatch"), "{error}");
        assert!(error.contains("No automatic download"), "{error}");
        let untrusted = dir.path().join("untrusted.AppImage");
        assert!(
            copy_verified_tool(&spec, Some(&untrusted)).is_err(),
            "unpinned bytes must not be accepted"
        );
        assert_eq!(
            fs::metadata(&untrusted)
                .expect("untrusted staging metadata")
                .permissions()
                .mode()
                & 0o111,
            0,
            "unverified staged bytes must not be made executable"
        );
        assert!(
            copy_verified_tool(&spec, Some(&staged)).is_err(),
            "staging collisions must fail"
        );
        assert_eq!(
            fs::read(staged).expect("read existing stage").as_slice(),
            b"existing"
        );
    }

    #[test]
    fn tool_rejects_symlink_directory_and_fifo_without_waiting_for_writer() {
        let dir = tempfile::tempdir().expect("create fixture directory");
        let target = fixture_tool(dir.path(), "actual.AppImage", b"abc");
        let link = dir.path().join("link.AppImage");
        symlink(&target, &link).expect("create symlink");
        assert!(copy_verified_tool(&fixture_spec(link, ABC_SHA256), None).is_err());
        assert!(
            copy_verified_tool(&fixture_spec(dir.path().to_path_buf(), ABC_SHA256), None).is_err()
        );

        let fifo = dir.path().join("tool.fifo");
        let name = CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path has no NUL");
        // SAFETY: name is a valid, NUL-terminated path and mkfifo does not retain it.
        assert_eq!(
            unsafe { libc::mkfifo(name.as_ptr(), 0o600) },
            0,
            "create FIFO fixture"
        );
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = copy_verified_tool(&fixture_spec(fifo, ABC_SHA256), None);
            let _ = sender.send(result);
        });
        let result = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("FIFO check must not block");
        assert!(result.is_err(), "FIFO is not a regular tool file");
    }

    #[test]
    fn tool_paths_and_architecture_assets_keep_pinned_release_selection() {
        let dir = tempfile::tempdir().expect("create fixture directory");
        let cache = dir.path().join("cache");
        let spec = tool_spec_from(&LINUXDEPLOY, "x64", &cache, None).expect("default spec");
        assert_eq!(
            spec.path,
            cache.join(
                "slimenrf-helper/appimage-tools/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage"
            )
        );
        assert_eq!(
            spec.digest,
            "c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d"
        );
        assert_eq!(
            spec.url,
            "https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage"
        );
        assert_eq!(
            asset_for(&APPIMAGETOOL, "x86").expect("x86 asset").0,
            "i686"
        );
        assert_eq!(asset_for(&LINUXDEPLOY, "x86").expect("x86 asset").0, "i386");
        assert!(asset_for(&APPIMAGETOOL, "riscv64").is_err());

        let override_path = dir.path().join("override.AppImage");
        let override_spec = tool_spec_from(&APPIMAGETOOL, "arm64", &cache, Some(&override_path))
            .expect("override spec");
        assert_eq!(override_spec.path, override_path);
        assert!(
            tool_spec_from(
                &APPIMAGETOOL,
                "arm64",
                Path::new("relative-cache"),
                Some(&override_path)
            )
            .is_err()
        );
        assert!(
            tool_spec_from(
                &APPIMAGETOOL,
                "arm64",
                &cache,
                Some(Path::new("relative-tool"))
            )
            .is_err()
        );
    }

    fn elf(path: &Path, class: u8, encoding: u8, machine: [u8; 2]) {
        let mut header = [0; 20];
        header[..4].copy_from_slice(b"\x7fELF");
        header[4] = class;
        header[5] = encoding;
        header[18..20].copy_from_slice(&machine);
        fs::write(path, header).expect("write ELF fixture");
    }

    #[test]
    fn client_library_discovery_uses_native_elf_class_and_machine() {
        let dir = tempfile::tempdir().expect("create fixture directory");
        let native = dir.path().join("native");
        let wrong = dir.path().join("wrong");
        fs::create_dir(&native).expect("create native directory");
        fs::create_dir(&wrong).expect("create other-architecture directory");
        let mut listing = String::new();
        let mut expected = Vec::new();
        let directory = File::open(dir.path()).expect("open fixture directory");
        let cache_root = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
        for (index, name) in CLIENT_LIBRARIES.iter().enumerate() {
            let other_path = wrong.join(name);
            let native_path = native.join(name);
            match index % 3 {
                0 => elf(&other_path, 1, 1, [62, 0]),
                1 => elf(&other_path, 2, 1, [183, 0]),
                _ => elf(&other_path, 2, 2, [62, 0]),
            }
            elf(&native_path, 2, 1, [62, 0]);
            let cache_other = cache_root.join("wrong").join(name);
            let cache_native = cache_root.join("native").join(name);
            listing.push_str(&format!("\t{name} (libc6) => {}\n", cache_other.display()));
            listing.push_str(&format!(
                "\t{name} (libc6,x86-64) => {}\n",
                cache_native.display()
            ));
            expected.push(cache_native);
        }
        let mut native_header = [0; 20];
        native_header[4] = 2;
        native_header[5] = 1;
        native_header[18..20].copy_from_slice(&[62, 0]);
        assert_eq!(
            collect_client_libraries(&listing, &native_header).expect("find native libraries"),
            expected
        );

        fs::write(&expected[0], b"not an ELF").expect("invalidate first native library");
        let error = collect_client_libraries(&listing, &native_header)
            .expect_err("wrong architecture must not satisfy dependency");
        assert!(error.contains(CLIENT_LIBRARIES[0]), "{error}");
    }

    #[test]
    fn runtime_extraction_checks_offset_and_copies_exact_original_prefix() {
        let dir = tempfile::tempdir().expect("create fixture directory");
        let image = fixture_tool(dir.path(), "appimagetool.AppImage", b"runtime-squashfs");
        let output = dir.path().join("runtime");
        copy_runtime(&image, " 7\n", &output).expect("copy pinned runtime");
        assert_eq!(
            fs::read(&output).expect("read runtime").as_slice(),
            b"runtime"
        );
        assert!(
            copy_runtime(&image, "7", &output).is_err(),
            "must not overwrite runtime"
        );
        for bad in [
            "",
            "0",
            "-1",
            "+1",
            "16",
            "17",
            "99999999999999999999999999999",
            "1.0",
        ] {
            let absent = dir.path().join("invalid-runtime");
            assert!(
                copy_runtime(&image, bad, &absent).is_err(),
                "offset {bad:?} must fail"
            );
            assert!(!absent.exists(), "offset {bad:?} must not create runtime");
        }
        let link = dir.path().join("linked-tool.AppImage");
        symlink(&image, &link).expect("create tool symlink");
        assert!(copy_runtime(&link, "7", &dir.path().join("linked-runtime")).is_err());
    }

    #[test]
    fn executable_staging_uses_the_already_open_file_after_path_replacement() {
        let dir = tempfile::tempdir().expect("create fixture directory");
        let original = fixture_tool(dir.path(), "cargo-elf", b"validated ELF bytes");
        let mut pinned = File::open(&original).expect("open validated source");
        pinned
            .seek(SeekFrom::End(0))
            .expect("advance inspected source");
        fs::rename(&original, dir.path().join("old-elf")).expect("move original source");
        fs::write(&original, b"replacement bytes").expect("replace source path");
        let staged = dir.path().join("staged-executable");
        stage_executable(&mut pinned, &staged).expect("stage original handle");
        assert_eq!(
            fs::read(&staged)
                .expect("read staged executable")
                .as_slice(),
            b"validated ELF bytes"
        );
        assert_eq!(
            fs::metadata(staged)
                .expect("staged metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }

    #[test]
    fn deployment_filters_real_host_library_files_and_forces_safe_environment() {
        let dir = tempfile::tempdir().expect("create fixture directory");
        let fixture = dir.path().join("available");
        fs::create_dir(&fixture).expect("create library directory");
        let host = [
            "ld-linux-x86-64.so.2",
            "ld-musl-x86_64.so.1",
            "libc.so.6",
            "libm.so.6",
            "libpthread.so.0",
            "libdl.so.2",
            "librt.so.1",
            "libresolv.so.2",
            "libnss_files.so.2",
            "libutil.so.1",
            "libanl.so.1",
            "libBrokenLocale.so.1",
            "libudev.so.1",
            "libsystemd.so.0",
            "libwayland-client.so.0",
            "libwayland-cursor.so.0",
            "libwayland-egl.so.1",
            "libEGL.so.1",
            "libGL.so.1",
            "libGLX_mesa.so.0",
            "libGLESv2.so.2",
            "libOpenGL.so.0",
            "libGLdispatch.so.0",
            "libgbm.so.1",
            "libdrm_amdgpu.so.1",
            "libvulkan.so.1",
            "libnvidia-glcore.so.1",
            "libcuda.so.1",
            "radeonsi_dri.so",
        ];
        for name in host.iter().copied().chain(CLIENT_LIBRARIES.iter().copied()) {
            fs::write(fixture.join(name), name).expect("write library fixture");
        }
        let deployer = fixture_tool(
            dir.path(),
            "linuxdeploy",
            br##"#!/bin/sh
set -eu
[ "$APPIMAGE_EXTRACT_AND_RUN" = 1 ]
[ "$ARCH" = x86_64 ]
[ "$VERSION" = 1.2.3 ]
[ "$LC_ALL" = C ]
[ "$NO_STRIP" = 1 ]
[ "${LINUXDEPLOY_EXCLUDED_LIBRARIES+x}" != x ]
[ "${DISABLE_COPYRIGHT_FILES_DEPLOYMENT+x}" != x ]
[ "$FONTCONFIG_PATH" = preserve-fonts ]
[ "$XKB_CONFIG_ROOT" = preserve-xkb ]
appdir=
patterns=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --appdir) appdir=$2 ;;
        --exclude-library) patterns="$patterns $2" ;;
        --executable|--desktop-file|--icon-file|--custom-apprun|--library) [ -f "$2" ] ;;
        *) exit 10 ;;
    esac
    shift 2
done
[ -n "$appdir" ]
mkdir -p "$appdir/usr/lib"
for item in "$FIXTURE_LIBRARIES"/*; do
    name=${item##*/}
    excluded=0
    set -f
    for pattern in $patterns; do
        case "$name" in $pattern) excluded=1 ;; esac
    done
    set +f
    if [ "$excluded" = 0 ]; then cp "$item" "$appdir/usr/lib/$name"; fi
done
"##,
        );
        fs::set_permissions(&deployer, fs::Permissions::from_mode(0o755))
            .expect("make fixture deployer executable");
        let assets = dir.path().join("assets");
        fs::create_dir(&assets).expect("create assets directory");
        for name in ["slimenrf-helper.desktop", "slimenrf-helper.svg", "AppRun"] {
            fs::write(assets.join(name), b"asset").expect("write asset");
        }
        let executable = fixture_tool(dir.path(), "app", b"elf");
        let clients: Vec<PathBuf> = CLIENT_LIBRARIES
            .iter()
            .map(|name| fixture.join(name))
            .collect();
        let appdir = dir.path().join("AppDir");
        let mut command = linuxdeploy_command(&deployer, &appdir, &executable, &assets, &clients);
        command
            .env("FIXTURE_LIBRARIES", &fixture)
            .env("LINUXDEPLOY_EXCLUDED_LIBRARIES", "libxkbcommon.so.0")
            .env("DISABLE_COPYRIGHT_FILES_DEPLOYMENT", "1")
            .env("FONTCONFIG_PATH", "preserve-fonts")
            .env("XKB_CONFIG_ROOT", "preserve-xkb");
        deployment_environment(&mut command, "x86_64", "1.2.3");
        let status = command
            .status()
            .expect("execute functional deployer fixture");
        assert!(status.success(), "deployer fixture failed: {status}");
        for name in host {
            assert!(
                !appdir.join("usr/lib").join(name).exists(),
                "host library {name} leaked into bundle"
            );
        }
        for name in CLIENT_LIBRARIES {
            assert_eq!(
                fs::read(appdir.join("usr/lib").join(name))
                    .expect("bundled client library")
                    .as_slice(),
                name.as_bytes()
            );
        }
    }
}
