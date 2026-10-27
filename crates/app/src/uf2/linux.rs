use super::Uf2Volume;
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{FileTypeExt, MetadataExt},
    },
    path::{Path, PathBuf},
    sync::Arc,
};

const CHANGED: &str = "UF2 volume identity changed after selection; select the device again";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DeviceNumber {
    major: u32,
    minor: u32,
}
impl DeviceNumber {
    fn from_dev(dev: u64) -> Self {
        // Linux's dev_t encoding (including the extended major/minor bits).
        Self {
            major: (((dev >> 8) & 0xfff) | ((dev >> 32) & 0xffff_f000)) as u32,
            minor: ((dev & 0xff) | ((dev >> 12) & 0xffff_ff00)) as u32,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Mount {
    id: u64,
    parent: u64,
    device: DeviceNumber,
    root: PathBuf,
    point: PathBuf,
    filesystem: Vec<u8>,
    source: PathBuf,
    writable: bool,
}
impl Mount {
    fn eligible(&self) -> bool {
        self.writable
            && self.device.major != 0
            && self.root.is_absolute()
            && matches!(
                self.filesystem.as_slice(),
                b"vfat" | b"msdos" | b"fat" | b"exfat"
            )
            && self.source.is_absolute()
    }
}

fn decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

fn unescape_mount(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut result = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            let digits = bytes.get(i + 1..i + 4)?;
            if !digits.iter().all(|b| (b'0'..=b'7').contains(b)) {
                return None;
            }
            let value = (digits[0] - b'0') as u16 * 64
                + (digits[1] - b'0') as u16 * 8
                + (digits[2] - b'0') as u16;
            let value = u8::try_from(value).ok()?;
            if value == 0 {
                return None;
            }
            result.push(value);
            i += 4;
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    Some(result)
}

fn path_field(bytes: &[u8]) -> Option<PathBuf> {
    Some(PathBuf::from(OsString::from_vec(unescape_mount(bytes)?)))
}

fn writable_options(options: &[u8]) -> bool {
    let mut options = options.split(|b| *b == b',');
    options.clone().any(|option| option == b"rw") && !options.any(|option| option == b"ro")
}

fn parse_mount(line: &[u8]) -> Option<Mount> {
    let fields: Vec<_> = line
        .split(|b| b.is_ascii_whitespace())
        .filter(|f| !f.is_empty())
        .collect();
    let separator = fields.iter().position(|field| *field == b"-")?;
    if separator < 6 || fields.len() != separator + 4 {
        return None;
    }
    let mut device = fields[2].split(|b| *b == b':');
    let major = u32::try_from(decimal(device.next()?)?).ok()?;
    let minor = u32::try_from(decimal(device.next()?)?).ok()?;
    if device.next().is_some() {
        return None;
    }
    let root = path_field(fields[3])?;
    let point = path_field(fields[4])?;
    // Pseudo filesystems can expose non-path roots, e.g. nsfs net:[4026532232].
    // Native FAT eligibility validates its root separately.
    if !point.is_absolute() {
        return None;
    }
    Some(Mount {
        id: decimal(fields[0])?,
        parent: decimal(fields[1])?,
        device: DeviceNumber { major, minor },
        root,
        point,
        filesystem: unescape_mount(fields[separator + 1])?,
        source: path_field(fields[separator + 2])?,
        writable: writable_options(fields[5]) && writable_options(fields[separator + 3]),
    })
}

fn parse_mounts(bytes: &[u8]) -> Result<Vec<Mount>, String> {
    bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            parse_mount(line).ok_or_else(|| "Cannot parse Linux mount identity table".into())
        })
        .collect()
}

fn mounts() -> Result<Vec<Mount>, String> {
    let bytes = fs::read("/proc/self/mountinfo")
        .map_err(|e| format!("Cannot read Linux mount identity table: {e}"))?;
    parse_mounts(&bytes)
}

fn mount_at<'a>(mounts: &'a [Mount], path: &Path) -> Result<Option<&'a Mount>, String> {
    let mut matching = mounts.iter().filter(|mount| mount.point == path);
    let mount = matching.next();
    // Do not guess which of stacked mounts is visible, including same-device binds.
    if matching.next().is_some() {
        return Err("UF2 mountpoint has ambiguous stacked mounts".into());
    }
    Ok(mount)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BlockNode {
    device: u64,
    inode: u64,
    rdev: u64,
    changed: (i64, i64),
}
fn block_node(path: &Path, device: DeviceNumber) -> Result<BlockNode, String> {
    // stat follows /dev aliases, but never opens the device or reads raw sectors.
    let metadata =
        fs::metadata(path).map_err(|e| format!("Cannot identify UF2 block device: {e}"))?;
    if !metadata.file_type().is_block_device() || DeviceNumber::from_dev(metadata.rdev()) != device
    {
        return Err("UF2 mount source is not its native block device".into());
    }
    Ok(BlockNode {
        device: metadata.dev(),
        inode: metadata.ino(),
        rdev: metadata.rdev(),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    })
}

fn decode_label(name: &OsStr) -> Option<String> {
    let bytes = name.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            if bytes.get(i + 1) != Some(&b'x') {
                return None;
            }
            let hex = std::str::from_utf8(bytes.get(i + 2..i + 4)?).ok()?;
            let value = u8::from_str_radix(hex, 16).ok()?;
            if value == 0 {
                return None;
            }
            result.push(value);
            i += 4;
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    // Catalog matching is UTF-8: reject, rather than conflate, invalid label bytes.
    String::from_utf8(result).ok()
}

#[derive(Clone, Debug)]
struct Label {
    text: String,
    link: PathBuf,
}
impl Label {
    fn revalidate(&self, device: DeviceNumber) -> Result<(), String> {
        if !fs::symlink_metadata(&self.link).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(CHANGED.into());
        }
        block_node(&self.link, device)
            .map(|_| ())
            .map_err(|_| CHANGED.into())
    }
}

fn filesystem_label(directory: &Path, device: DeviceNumber) -> Result<Option<Label>, String> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Cannot read Linux filesystem labels: {e}")),
    };
    let mut label: Option<Label> = None;
    for entry in entries {
        let entry = entry.map_err(|e| format!("Cannot read Linux filesystem label: {e}"))?;
        if !entry.file_type().is_ok_and(|kind| kind.is_symlink())
            || block_node(&entry.path(), device).is_err()
        {
            continue;
        }
        let text = decode_label(&entry.file_name())
            .ok_or("UF2 filesystem label cannot be decoded losslessly as UTF-8")?;
        if label.as_ref().is_some_and(|previous| previous.text != text) {
            return Err("UF2 block device has ambiguous filesystem labels".into());
        }
        label = Some(Label {
            text,
            link: entry.path(),
        });
    }
    Ok(label)
}

#[derive(Clone, Debug)]
pub(super) struct MountedIdentity {
    mount: Mount,
    node: BlockNode,
    inode: u64,
    label: Option<Label>,
}

#[derive(Clone, Debug)]
pub(super) enum Identity {
    Mounted(Arc<MountedIdentity>),
    // Only explicit inspect_volume calls can admit directory fixtures.
    Directory { device: u64, inode: u64 },
}

fn validate_mount(current: Option<&Mount>, frozen: &Mount) -> Result<(), String> {
    if current != Some(frozen) || !frozen.eligible() {
        return Err(CHANGED.into());
    }
    Ok(())
}

impl Identity {
    pub(super) fn inspect(
        path: &Path,
        metadata: &fs::Metadata,
        expected: Option<&Self>,
    ) -> Result<Self, String> {
        let table = mounts()?;
        let current = mount_at(&table, path)?;
        if let Some(frozen @ Self::Mounted(native)) = expected {
            let MountedIdentity {
                mount,
                node,
                inode,
                label,
            } = native.as_ref();
            // A disappeared mount must never be reinterpreted as a directory fixture.
            validate_mount(current, mount)?;
            if DeviceNumber::from_dev(metadata.dev()) != mount.device
                || metadata.ino() != *inode
                || block_node(&mount.source, mount.device)? != *node
            {
                return Err(CHANGED.into());
            }
            if let Some(label) = label {
                label.revalidate(mount.device)?;
            }
            return Ok(frozen.clone());
        }
        if let Some(mount) = current {
            if expected.is_some()
                || !mount.eligible()
                || DeviceNumber::from_dev(metadata.dev()) != mount.device
            {
                return Err("UF2 volume is not a writable native FAT block mount".into());
            }
            return Ok(Self::Mounted(Arc::new(MountedIdentity {
                node: block_node(&mount.source, mount.device)?,
                inode: metadata.ino(),
                label: filesystem_label(Path::new("/dev/disk/by-label"), mount.device)?,
                mount: mount.clone(),
            })));
        }
        let identity = Self::Directory {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        if let Some(Self::Directory { device, inode }) = expected
            && (*device != metadata.dev() || *inode != metadata.ino())
        {
            return Err(CHANGED.into());
        }
        Ok(identity)
    }

    pub(super) fn volume_identity(&self) -> (String, String) {
        match self {
            Self::Mounted(native) => {
                let MountedIdentity {
                    mount,
                    node,
                    inode,
                    label,
                } = native.as_ref();
                (
                    label
                        .as_ref()
                        .map(|label| label.text.clone())
                        .unwrap_or_default(),
                    // Debug formatting preserves non-UTF-8 path bytes without display collisions.
                    format!("linux-mount:{mount:?}:{node:?}:{inode}"),
                )
            }
            Self::Directory { device, inode } => {
                (String::new(), format!("linux-directory:{device}:{inode}"))
            }
        }
    }
}

pub(super) fn scan(info_filename: &str) -> Result<Vec<Uf2Volume>, String> {
    let table = mounts()?;
    let mut seen = BTreeSet::new();
    let mut volumes = Vec::new();
    for mount in table.iter().filter(|mount| mount.eligible()) {
        if !seen.insert(mount.point.clone())
            || !(mount.point.join(info_filename).is_file()
                || mount.point.join("CURRENT.UF2").is_file())
        {
            continue;
        }
        if let Ok(volume) = super::inspect_volume(&mount.point, info_filename)
            && let Identity::Mounted(native) = &volume.linux_identity
            && native.mount == *mount
        {
            volumes.push(volume);
        }
    }
    Ok(volumes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount() -> Mount {
        parse_mount(
            b"42 21 8:17 / /media/custom rw,nosuid shared:7 master:2 - vfat /dev/sdb1 rw,uid=1000",
        )
        .unwrap()
    }

    #[test]
    fn parses_optional_fields_and_lossless_escaped_paths_once() {
        let parsed = parse_mount(b"42 21 8:17 /root\\040dir /media/My\\040Drive\\134040\\377 rw shared:7 - vfat /dev/disk/by-id/a\\134b rw").unwrap();
        assert_eq!(parsed.id, 42);
        assert_eq!(parsed.parent, 21);
        assert_eq!(
            parsed.device,
            DeviceNumber {
                major: 8,
                minor: 17
            }
        );
        assert_eq!(parsed.root.as_os_str().as_bytes(), b"/root dir");
        assert_eq!(
            parsed.point.as_os_str().as_bytes(),
            b"/media/My Drive\\040\xff"
        );
        assert_eq!(
            parsed.source.as_os_str().as_bytes(),
            b"/dev/disk/by-id/a\\b"
        );
        assert!(parsed.eligible());
        assert!(parse_mount(b"7 1 8:1 / /tmp rw - vfat /dev/sda1 rw").is_some());
    }

    #[test]
    fn parses_native_fat_alongside_namespace_pseudo_filesystems() {
        let table = parse_mounts(
            b"42 21 8:17 / /media/custom rw - vfat /dev/sdb1 rw\n864 122 0:4 net:[4026532232] /run/docker/netns/1746098dfba2 rw shared:337 - nsfs nsfs rw\n",
        )
        .unwrap();
        assert_eq!(table.len(), 2);
        assert!(table[0].eligible());
        assert_eq!(table[1].root, Path::new("net:[4026532232]"));
        assert!(!table[1].eligible());
    }

    #[test]
    fn rejects_malformed_mount_tables() {
        for line in [
            b"42 21 8:17 / /media rw vfat /dev/sdb1 rw".as_slice(),
            b"42 21 8:17 / /media\\xyz rw - vfat /dev/sdb1 rw",
            b"42 21 8:17 / /media\\777 rw - vfat /dev/sdb1 rw",
            b"42 21 8:17 / /media\\000 rw - vfat /dev/sdb1 rw",
            b"42 21 8:17:1 / /media rw - vfat /dev/sdb1 rw",
            b"42 21 8:17 / relative rw - vfat /dev/sdb1 rw",
            b"42 21 8:17 / /media rw - vfat /dev/sdb1",
        ] {
            assert!(parse_mount(line).is_none(), "{line:?}");
            assert!(parse_mounts(line).is_err());
        }
    }

    #[test]
    fn excludes_readonly_wsl_and_nonfat_mounts() {
        for line in [
            b"42 21 8:17 / /media ro - vfat /dev/sdb1 rw".as_slice(),
            b"42 21 8:17 / /media rw - vfat /dev/sdb1 ro",
            b"42 21 0:17 / /mnt/c rw - 9p C:\\134 rw,aname=drvfs",
            b"42 21 0:17 / /mnt/c rw - drvfs C:\\134 rw",
            b"42 21 8:17 / /media rw - ext4 /dev/sdb1 rw",
            b"42 21 0:17 / /media rw - vfat /dev/sdb1 rw",
            b"42 21 8:17 / /media rw - vfat none rw",
            b"42 21 8:17 relative /media rw - vfat /dev/sdb1 rw",
        ] {
            assert!(!parse_mount(line).unwrap().eligible(), "{line:?}");
        }
    }

    #[test]
    fn mount_incarnation_device_root_source_and_readonly_changes_revoke_selection() {
        let frozen = mount();
        validate_mount(Some(&frozen), &frozen).unwrap();
        assert!(validate_mount(None, &frozen).is_err());
        let mut replacements = Vec::new();
        let mut changed = frozen.clone();
        changed.id += 1;
        replacements.push(changed);
        let mut changed = frozen.clone();
        changed.device.minor += 1;
        replacements.push(changed);
        let mut changed = frozen.clone();
        changed.root = "/other".into();
        replacements.push(changed);
        let mut changed = frozen.clone();
        changed.source = "/dev/sdc1".into();
        replacements.push(changed);
        let mut changed = frozen.clone();
        changed.writable = false;
        replacements.push(changed);
        let mut changed = frozen.clone();
        changed.filesystem = b"exfat".to_vec();
        replacements.push(changed);
        for replacement in replacements {
            assert!(validate_mount(Some(&replacement), &frozen).is_err());
        }
    }

    #[test]
    fn label_decoding_is_lossless_and_never_uses_mount_folder() {
        assert_eq!(
            decode_label(OsStr::new(r"SLIME\x20NRF\x5c040")),
            Some("SLIME NRF\\040".into())
        );
        assert_eq!(decode_label(OsStr::new(r"\xc3\xa9")), Some("é".into()));
        assert_eq!(decode_label(OsStr::new(r"\xff")), None);
        assert_eq!(decode_label(OsStr::new(r"bad\x00")), None);
        assert_eq!(decode_label(OsStr::new(r"bad\xz0")), None);
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("device"), b"not a block device").unwrap();
        let labels = dir.path().join("labels");
        fs::create_dir(&labels).unwrap();
        std::os::unix::fs::symlink(dir.path().join("device"), labels.join("SLIMENRF1")).unwrap();
        assert!(
            filesystem_label(
                &labels,
                DeviceNumber {
                    major: 8,
                    minor: 17
                }
            )
            .unwrap()
            .is_none()
        );
        assert!(
            block_node(
                &dir.path().join("device"),
                DeviceNumber {
                    major: 8,
                    minor: 17
                }
            )
            .is_err()
        );
    }

    #[test]
    fn native_label_is_independent_of_mount_directory_and_unknown_stays_empty() {
        let mount = mount();
        assert_eq!(mount.point, Path::new("/media/custom"));
        let mut identity = Identity::Mounted(Arc::new(MountedIdentity {
            mount,
            node: BlockNode {
                device: 0,
                inode: 0,
                rdev: 0,
                changed: (0, 0),
            },
            inode: 2,
            label: Some(Label {
                text: decode_label(OsStr::new("SLIMENRF1")).unwrap(),
                link: "/dev/disk/by-label/SLIMENRF1".into(),
            }),
        }));
        assert_eq!(identity.volume_identity().0, "SLIMENRF1");
        if let Identity::Mounted(native) = &mut identity {
            Arc::make_mut(native).label = None;
        }
        assert_eq!(identity.volume_identity().0, "");
        let directory = Identity::Directory {
            device: 0,
            inode: 2,
        };
        assert_ne!(identity.volume_identity().1, directory.volume_identity().1);
    }

    #[test]
    fn rejects_stacked_mounts_and_nested_marker_directories() {
        let first = mount();
        let mut second = first.clone();
        second.id += 1;
        assert!(mount_at(&[first.clone(), second], &first.point).is_err());
        assert!(
            mount_at(std::slice::from_ref(&first), &first.point.join("nested"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn frozen_mount_cannot_fall_back_to_directory_fixture() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("INFO_UF2.TXT"), "Board-ID: nRF52840-test").unwrap();
        let mut volume = super::super::inspect_volume(dir.path(), "INFO_UF2.TXT").unwrap();
        assert!(matches!(volume.linux_identity, Identity::Directory { .. }));
        let mut missing = mount();
        missing.point = fs::canonicalize(dir.path()).unwrap();
        volume.linux_identity = Identity::Mounted(Arc::new(MountedIdentity {
            mount: missing,
            node: BlockNode {
                device: 0,
                inode: 0,
                rdev: 0,
                changed: (0, 0),
            },
            inode: fs::metadata(dir.path()).unwrap().ino(),
            label: None,
        }));
        assert!(super::super::revalidate(&volume).is_err());
        let cancel = std::sync::atomic::AtomicBool::new(false);
        assert!(super::super::reboot(&volume, &cancel, &mut |_| {}).is_err());
        assert!(!dir.path().join("REBOOT.UF2").exists());
    }

    #[test]
    fn replacement_directory_with_identical_marker_revokes_selection() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("volume");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("INFO_UF2.TXT"), "Board-ID: nRF52840-test").unwrap();
        let volume = super::super::inspect_volume(&path, "INFO_UF2.TXT").unwrap();
        fs::rename(&path, parent.path().join("old-volume")).unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("INFO_UF2.TXT"), "Board-ID: nRF52840-test").unwrap();
        assert!(super::super::revalidate(&volume).is_err());
    }

    #[test]
    fn linux_device_numbers_include_extended_bits() {
        assert_eq!(
            DeviceNumber::from_dev(0x811),
            DeviceNumber {
                major: 8,
                minor: 17
            }
        );
        let major = 0x12345u64;
        let minor = 0x67890u64;
        let encoded = (minor & 0xff)
            | ((major & 0xfff) << 8)
            | ((minor & !0xff) << 12)
            | ((major & !0xfff) << 32);
        assert_eq!(
            DeviceNumber::from_dev(encoded),
            DeviceNumber {
                major: major as u32,
                minor: minor as u32
            }
        );
    }
}
