use std::{
    fs::File,
    io::{self, Write as _},
    path::Path,
};

/// Replaces an export only after its same-directory temporary file is written and synced.
///
/// # Errors
/// Returns the underlying I/O error without truncating the destination on failure.
pub(crate) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_with(path, |file| {
        file.write_all(bytes)?;
        file.flush()
    })
}

fn write_with(path: &Path, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    write(temporary.as_file_mut())?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{write, write_with};
    use std::{
        fs::{self, File},
        io::{self, Write},
        path::Path,
    };

    #[derive(Clone, Copy)]
    enum Failure {
        AfterBytes(usize),
        Flush,
    }

    struct FailingWriter<'a> {
        file: &'a mut File,
        failure: Failure,
    }

    impl Write for FailingWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            match &mut self.failure {
                Failure::AfterBytes(remaining) => {
                    if *remaining == 0 {
                        return Err(io::Error::other("injected write failure"));
                    }
                    let written = self.file.write(&bytes[..bytes.len().min(*remaining)])?;
                    *remaining -= written;
                    Ok(written)
                }
                Failure::Flush => self.file.write(bytes),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            match self.failure {
                Failure::AfterBytes(_) => self.file.flush(),
                Failure::Flush => Err(io::Error::other("injected flush failure")),
            }
        }
    }

    fn assert_entry_count(directory: &Path, count: usize) {
        let entries = fs::read_dir(directory)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            entries.len(),
            count,
            "temporary export files must be removed"
        );
    }

    #[test]
    fn write_and_flush_failures_preserve_destination_and_remove_temporary() {
        for (failure, message) in [
            (Failure::AfterBytes(5), "injected write failure"),
            (Failure::Flush, "injected flush failure"),
        ] {
            for original in [None, Some(b"existing destination".as_slice())] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("export.txt");
                if let Some(original) = original {
                    fs::write(&path, original).unwrap();
                }
                let error = write_with(&path, |file| {
                    let mut writer = FailingWriter { file, failure };
                    let result = writer
                        .write_all(b"partially written replacement")
                        .and_then(|()| writer.flush());
                    assert!(writer.file.metadata().unwrap().len() > 0);
                    assert_eq!(fs::read(&path).ok().as_deref(), original);
                    result
                })
                .unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::Other);
                assert_eq!(error.to_string(), message);
                assert_eq!(fs::read(&path).ok().as_deref(), original);
                assert_entry_count(directory.path(), usize::from(original.is_some()));
            }
        }
    }

    #[test]
    fn successful_write_replaces_existing_bytes_and_removes_temporary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("export.txt");
        fs::write(&path, b"a longer original export that must disappear").unwrap();

        write(&path, b"new\0bytes\n").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new\0bytes\n");
        assert_entry_count(directory.path(), 1);
    }

    #[test]
    fn successful_write_creates_nonexistent_destination() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("new-export.txt");

        write(&path, b"new export\n").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new export\n");
        assert_entry_count(directory.path(), 1);
    }

    #[test]
    fn bare_relative_destination_uses_current_directory_without_changing_it() {
        let destination = tempfile::NamedTempFile::new_in(".")
            .unwrap()
            .into_temp_path();
        let relative = Path::new(destination.file_name().unwrap());

        write(relative, b"relative export\n").unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"relative export\n");
    }

    #[test]
    fn missing_parent_is_reported_without_creating_directories() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing").join("export.txt");

        let error = write(&path, b"new export").unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!path.exists());
        assert_entry_count(directory.path(), 0);
    }

    #[test]
    fn failed_publish_preserves_directory_contents_and_removes_temporary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("export.txt");
        fs::create_dir(&path).unwrap();
        let child = path.join("keep.txt");
        fs::write(&child, b"existing contents").unwrap();

        assert!(write(&path, b"replacement export").is_err());

        assert_eq!(fs::read(child).unwrap(), b"existing contents");
        assert_entry_count(&path, 1);
        assert_entry_count(directory.path(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn failed_publish_preserves_locked_file_and_removes_temporary() {
        use std::os::windows::fs::OpenOptionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("export.txt");
        fs::write(&path, b"existing export").unwrap();
        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();

        assert!(write(&path, b"replacement export").is_err());

        assert_entry_count(directory.path(), 1);
        drop(lock);
        assert_eq!(fs::read(path).unwrap(), b"existing export");
    }
}
