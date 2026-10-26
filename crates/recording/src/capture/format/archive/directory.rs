use super::super::{MAX_INPUT_BYTES, MAX_TRACKS, cancelled, err};
use std::cell::Cell;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::AtomicBool;

const MAX_DIRECTORY_BYTES: usize = 4 * 1024 * 1024;
const EOCD: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];

pub(super) struct Directory {
    start: u64,
    bytes: Vec<u8>,
    pub(super) count: usize,
}

impl Directory {
    pub(super) fn read(file: &mut File, cancel: &AtomicBool) -> Result<Self, String> {
        cancelled(cancel)?;
        let metadata = file.metadata().map_err(err)?;
        let length = metadata.len();
        if !metadata.is_file() || !(22..=MAX_INPUT_BYTES).contains(&length) {
            return Err("Recording ZIP has an invalid or excessive length".into());
        }
        let tail_start = length.saturating_sub(22 + u64::from(u16::MAX));
        file.seek(SeekFrom::Start(tail_start)).map_err(err)?;
        let mut tail = vec![0; (length - tail_start) as usize];
        file.read_exact(&mut tail).map_err(err)?;
        let end = (0..=tail.len() - 22)
            .rev()
            .find(|&offset| {
                tail[offset..offset + 4] == EOCD
                    && offset + 22 + usize::from(u16_at(&tail, offset + 20)) == tail.len()
            })
            .ok_or("Recording ZIP has no valid end-of-directory record")?;
        let count = usize::from(u16_at(&tail, end + 10));
        if u16_at(&tail, end + 4) != 0
            || u16_at(&tail, end + 6) != 0
            || usize::from(u16_at(&tail, end + 8)) != count
            || count > MAX_TRACKS * 2
        {
            return Err("Recording ZIP has too many entries or uses split/ZIP64 storage".into());
        }
        let size = u32_at(&tail, end + 12);
        let start = u32_at(&tail, end + 16);
        if size == u32::MAX
            || start == u32::MAX
            || size as usize > MAX_DIRECTORY_BYTES
            || u64::from(start) + u64::from(size) != tail_start + end as u64
        {
            return Err("Recording ZIP central directory is invalid or excessive".into());
        }
        let start = u64::from(start);
        file.seek(SeekFrom::Start(start)).map_err(err)?;
        let mut bytes = vec![0; (length - start) as usize];
        file.read_exact(&mut bytes).map_err(err)?;
        let directory_end = size as usize;
        if bytes
            .windows(4)
            .enumerate()
            .any(|(offset, signature)| signature == EOCD && offset != directory_end)
        {
            return Err("Recording ZIP has competing end-of-directory records".into());
        }
        if directory_end >= 20
            && bytes[directory_end - 20..directory_end - 16] == [0x50, 0x4b, 0x06, 0x07]
        {
            return Err("Recording ZIP uses unsupported ZIP64 storage".into());
        }
        let mut offset = 0usize;
        for _ in 0..count {
            cancelled(cancel)?;
            if offset.checked_add(46).is_none_or(|end| end > directory_end)
                || bytes[offset..offset + 4] != [0x50, 0x4b, 0x01, 0x02]
            {
                return Err("Recording ZIP has an inconsistent central directory".into());
            }
            let name = usize::from(u16_at(&bytes, offset + 28));
            let extra = usize::from(u16_at(&bytes, offset + 30));
            let comment = usize::from(u16_at(&bytes, offset + 32));
            if name == 0 || name > 1024 || u16_at(&bytes, offset + 34) != 0 {
                return Err("Recording ZIP member name or disk is invalid".into());
            }
            if [20, 24, 42]
                .into_iter()
                .any(|field| u32_at(&bytes, offset + field) == u32::MAX)
                || u64::from(u32_at(&bytes, offset + 42)) >= start
            {
                return Err(
                    "Recording ZIP uses invalid offsets or unsupported ZIP64 storage".into(),
                );
            }
            let extra_start = offset + 46 + name;
            let extra_end = extra_start + extra;
            let next = extra_end + comment;
            if next > directory_end {
                return Err("Recording ZIP directory member exceeds its envelope".into());
            }
            let mut field = extra_start;
            while field < extra_end {
                if extra_end - field < 4 {
                    return Err("Recording ZIP has malformed extra fields".into());
                }
                if u16_at(&bytes, field) == 1 {
                    return Err("Recording ZIP uses unsupported ZIP64 storage".into());
                }
                field += 4 + usize::from(u16_at(&bytes, field + 2));
                if field > extra_end {
                    return Err("Recording ZIP has malformed extra fields".into());
                }
            }
            offset = next;
        }
        if offset != directory_end {
            return Err("Recording ZIP directory member count is inconsistent".into());
        }
        Ok(Self {
            start,
            bytes,
            count,
        })
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

// zip retries EOCD signatures throughout its reader on malformed metadata.
// Construction sees only the bounded snapshot; payloads become readable afterward.
pub(super) struct DirectoryReader<'a> {
    pub(super) file: &'a mut File,
    pub(super) directory: &'a Directory,
    pub(super) constructing: &'a Cell<bool>,
    pub(super) position: u64,
    file_position: Option<u64>,
}

impl<'a> DirectoryReader<'a> {
    pub(super) fn new(
        file: &'a mut File,
        directory: &'a Directory,
        constructing: &'a Cell<bool>,
    ) -> Self {
        Self {
            file,
            directory,
            constructing,
            position: 0,
            file_position: None,
        }
    }
}

impl Read for DirectoryReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if !self.constructing.get() {
            if self.file_position != Some(self.position) {
                self.file.seek(SeekFrom::Start(self.position))?;
            }
            let count = self.file.read(buffer)?;
            self.position += count as u64;
            self.file_position = Some(self.position);
            return Ok(count);
        }
        let length = self.directory.start + self.directory.bytes.len() as u64;
        if self.position >= length {
            return Ok(0);
        }
        let count = buffer.len().min((length - self.position) as usize);
        let prefix = self.directory.start.saturating_sub(self.position);
        if prefix >= count as u64 {
            return Err(io::Error::other(
                "ZIP constructor left its validated directory",
            ));
        }
        let prefix = prefix as usize;
        buffer[..prefix].fill(0);
        let offset = (self.position + prefix as u64 - self.directory.start) as usize;
        buffer[prefix..count]
            .copy_from_slice(&self.directory.bytes[offset..offset + count - prefix]);
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for DirectoryReader<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let length = self.directory.start + self.directory.bytes.len() as u64;
        self.position = match position {
            SeekFrom::Start(position) => Some(position),
            SeekFrom::End(offset) => length.checked_add_signed(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
        }
        .ok_or_else(|| io::Error::other("Invalid ZIP seek"))?;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests;
