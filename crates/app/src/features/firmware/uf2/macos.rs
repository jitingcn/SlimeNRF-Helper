use super::{Marker, Uf2Volume};
use std::{
    ffi::{CStr, CString, OsStr},
    fs::{self, File, OpenOptions},
    io,
    mem::{MaybeUninit, size_of},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
    ptr, slice,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

mod disk;

const CHANGED: &str = "UF2 volume identity changed after selection; select the device again";

fn mount_table() -> io::Result<Vec<libc::statfs>> {
    // SAFETY: a null buffer requests only the number of mounted filesystems.
    let count = unsafe { libc::getfsstat(ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    let mut count = usize::try_from(count).map_err(|_| io::Error::last_os_error())?;
    loop {
        let mut mounts = Vec::<libc::statfs>::with_capacity(count + 8);
        let size = i32::try_from(mounts.capacity() * size_of::<libc::statfs>())
            .map_err(|_| io::Error::other("macOS mount table is too large"))?;
        // SAFETY: the vector has writable storage for `size` bytes, with statfs alignment.
        let found = unsafe { libc::getfsstat(mounts.as_mut_ptr(), size, libc::MNT_NOWAIT) };
        let found = usize::try_from(found).map_err(|_| io::Error::last_os_error())?;
        if found < mounts.capacity() {
            // SAFETY: getfsstat initialized one entry per returned mount.
            unsafe { mounts.set_len(found) };
            return Ok(mounts);
        }
        count = found;
    }
}

fn field(bytes: &[libc::c_char]) -> Option<&[u8]> {
    // SAFETY: the c_char array is initialized byte storage; its length does not change.
    let bytes = unsafe { slice::from_raw_parts(bytes.as_ptr().cast(), bytes.len()) };
    Some(CStr::from_bytes_until_nul(bytes).ok()?.to_bytes())
}

fn mounted_path(stat: &libc::statfs) -> Option<&Path> {
    let flags = stat.f_flags;
    if flags & libc::MNT_LOCAL as u32 == 0 || flags & libc::MNT_RDONLY as u32 != 0 {
        return None;
    }
    // LiveFiles mounts can report `lifs` instead of `msdos` or `exfat`.
    // Disk Arbitration must also confirm the actual writable FAT media and path.
    match field(&stat.f_fstypename)? {
        b"msdos" | b"exfat" | b"lifs" | b"msdos_fskit" | b"exfat_fskit" | b"fskit" => (),
        _ => return None,
    }
    let point = Path::new(OsStr::from_bytes(field(&stat.f_mntonname)?));
    point.is_absolute().then_some(point)
}

#[derive(Debug)]
pub(super) struct Root(File);

impl Root {
    fn open(path: &Path) -> io::Result<Self> {
        // An event-only directory descriptor distinguishes a remount even if its
        // path and BSD device name are reused, without preventing ordinary eject.
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_EVTONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)
            .map(Self)
    }

    fn is_at(&self, path: &Path) -> bool {
        self.0
            .metadata()
            .and_then(|open| {
                fs::symlink_metadata(path).map(|current| {
                    current.is_dir() && open.dev() == current.dev() && open.ino() == current.ino()
                })
            })
            .unwrap_or(false)
    }

    fn is_mounted_at(&self, path: &Path) -> bool {
        let mut stat = MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: the root descriptor is live and stat points to writable storage.
        if unsafe { libc::fstatfs(self.0.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return false;
        }
        // SAFETY: a successful fstatfs initialized the record.
        mounted_path(unsafe { &*stat.as_ptr() }) == Some(path) && self.is_at(path)
    }

    fn openat(&self, name: &CStr, flags: libc::c_int) -> io::Result<File> {
        // SAFETY: the directory is owned by self; name is nul-terminated, and the
        // mode argument has the promoted type required by openat's C varargs.
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o666 as libc::c_int,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: this is a fresh, owned descriptor returned by openat.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn open_regular(&self, name: &CStr) -> io::Result<File> {
        let file = self.openat(name, libc::O_RDONLY)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a regular file",
            ));
        }
        Ok(file)
    }

    fn marker(&self, name: &CStr) -> Result<Marker, String> {
        let filename = name.to_string_lossy();
        match self.open_regular(name) {
            Ok(file) => Marker::from_info(file, filename),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Marker::from_current(self.open_regular(c"CURRENT.UF2"), &filename)
            }
            Err(error) => Err(format!("Cannot open {filename}: {error}")),
        }
    }
}

#[derive(Debug)]
pub(super) struct Mount {
    path: PathBuf,
    root: Root,
    media: disk::Media,
    generation: u64,
}

#[derive(Clone, Debug)]
pub(super) enum Identity {
    Mounted(Arc<Mount>),
    // Directory fixtures have no constructor in production builds.
    #[cfg(test)]
    Fixture(Arc<Root>),
}

impl Identity {
    fn root(&self) -> &Root {
        match self {
            Self::Mounted(mount) => &mount.root,
            #[cfg(test)]
            Self::Fixture(root) => root,
        }
    }
}

fn pin_mount(path: &Path, root: Root, media: disk::Media) -> Arc<Mount> {
    // The same pin keeps selection stable across scans. A revoked descriptor or
    // a new I/O Registry entry receives a new generation, even at the same path.
    static PINS: Mutex<Vec<Weak<Mount>>> = Mutex::new(Vec::new());
    static GENERATION: AtomicU64 = AtomicU64::new(1);

    let mut pins = PINS.lock().expect("macOS UF2 mount-pin registry poisoned");
    pins.retain(|old| old.strong_count() != 0);
    if let Some(old) = pins
        .iter()
        .filter_map(Weak::upgrade)
        .find(|old| old.path == path && old.media == media && old.root.is_mounted_at(path))
    {
        return old;
    }
    let pin = Arc::new(Mount {
        path: path.to_owned(),
        root,
        media,
        generation: GENERATION.fetch_add(1, Ordering::Relaxed),
    });
    pins.push(Arc::downgrade(&pin));
    pin
}

pub(super) fn scan(info_filename: &str) -> Result<Vec<Uf2Volume>, String> {
    let table = mount_table().map_err(|e| format!("Cannot read macOS mount table: {e}"))?;
    let mut paths = table.iter().filter_map(mounted_path).peekable();
    if paths.peek().is_none() {
        return Ok(Vec::new());
    }
    let session = disk::Session::new().ok_or("Cannot open macOS Disk Arbitration session")?;
    let name = CString::new(info_filename).map_err(|_| "Invalid UF2 information filename")?;
    let mut volumes: Vec<Uf2Volume> = Vec::new();
    for path in paths {
        if volumes.iter().any(|volume| volume.mount == path) {
            continue;
        }
        let Ok(root) = Root::open(path) else {
            continue;
        };
        if !root.is_mounted_at(path) {
            continue;
        }
        let Some(media) = session.media_at(path) else {
            continue;
        };
        let Ok(marker) = root.marker(&name) else {
            continue;
        };
        if !root.is_mounted_at(path) {
            continue;
        }
        let pin = pin_mount(path, root, media);
        volumes.push(Uf2Volume {
            mount: path.to_owned(),
            label: pin.media.label.clone(),
            board_id: marker.board_id,
            identity: format!("macos-mount:{}:{:08x}", pin.generation, marker.checksum),
            info_filename: name.clone(),
            macos_identity: Identity::Mounted(pin),
            marker_checksum: marker.checksum,
        });
    }
    Ok(volumes)
}

pub(super) fn revalidate(volume: &Uf2Volume) -> Result<(), String> {
    let root = volume.macos_identity.root();
    match &volume.macos_identity {
        Identity::Mounted(pin) => {
            if pin.path != volume.mount
                || pin.media.label != volume.label
                || !root.is_mounted_at(&volume.mount)
                || !disk::Session::new()
                    .is_some_and(|session| session.is_current(&volume.mount, &pin.media))
            {
                return Err(CHANGED.into());
            }
        }
        #[cfg(test)]
        Identity::Fixture(_) => {
            if !root.is_at(&volume.mount) {
                return Err(CHANGED.into());
            }
        }
    }
    let marker = root.marker(&volume.info_filename)?;
    if marker.checksum != volume.marker_checksum || marker.board_id != volume.board_id {
        return Err(CHANGED.into());
    }
    Ok(())
}

pub(super) fn create_destination(volume: &Uf2Volume, name: &str) -> io::Result<File> {
    // Never resolve the write path relative to a potentially reused mount pathname.
    volume.macos_identity.root().openat(
        &CString::new(name)?,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
    )
}

pub(super) fn open_current(volume: &Uf2Volume) -> Result<File, String> {
    volume
        .macos_identity
        .root()
        .open_regular(c"CURRENT.UF2")
        .map_err(|e| format!("CURRENT.UF2 must be a regular file on the selected UF2 volume: {e}"))
}

#[cfg(test)]
pub(super) fn inspect_fixture(path: &Path, info_filename: &str) -> Result<Uf2Volume, String> {
    if !super::valid_filename(info_filename) {
        return Err("Invalid UF2 information filename".into());
    }
    let name = CString::new(info_filename).map_err(|_| "Invalid UF2 information filename")?;
    let mount = fs::canonicalize(path).map_err(|e| format!("UF2 volume is unavailable: {e}"))?;
    let root = Root::open(&mount).map_err(|e| format!("Cannot inspect UF2 volume: {e}"))?;
    let marker = root.marker(&name)?;
    let metadata = root
        .0
        .metadata()
        .map_err(|e| format!("Cannot stat UF2 volume: {e}"))?;
    Ok(Uf2Volume {
        identity: format!(
            "macos-fixture:{}:{}:{:08x}",
            metadata.dev(),
            metadata.ino(),
            marker.checksum
        ),
        label: String::new(),
        board_id: marker.board_id,
        mount,
        info_filename: name,
        macos_identity: Identity::Fixture(Arc::new(root)),
        marker_checksum: marker.checksum,
    })
}

#[cfg(test)]
mod tests;
