//! Reading Microsoft Flight Simulator 2024's `.fsarchive` container.
//!
//! FS2020 ships its navigation data as a loose tree of `scenery/NNNN/*.bgl` files, which
//! is what the rest of this module walks. FS2024 packs the whole thing - about 1,750 BGLs,
//! 135 MB - into one file: `fs24-fs-base-nav/content/minimal.fsarchive`. Without this, that
//! data is unreachable and amdbgen cannot read FS2024's navigation data at all.
//!
//! Layout, worked out from the file itself:
//!
//! ```text
//! +0x00  magic "RASA" (4 bytes)
//! +0x04  version, u32 LE (2, in every copy seen)
//! +0x08  file count, u32 LE
//! +0x0C  JSON index length, u32 LE
//! +0x20  the JSON index itself, NUL-padded out to that length
//! ```
//!
//! The index is `{"encryptionSetup":{...},"fileInfoList":[{"path","byteOffset","byteSize",
//! "uncompressed_size","hash"}, ...]}`. `byteOffset` is relative to the start of the data
//! section, which begins right after the index (`32 + index length`). Every entry seen has
//! `byteSize == uncompressed_size` and the encryption scheme reads `"notEncrypted"`, so
//! nothing here decompresses or decrypts - it only seeks and reads the bytes a path names.
//!
//! Reading is by seek, never a full load: the archive is 135 MB and a caller after one
//! `nax` file has no reason to hold the other 1,749 in memory.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MAGIC: [u8; 4] = *b"RASA";
/// Header fields end at byte 16, but the index is documented (and observed) to start at
/// byte 32; the 16 bytes between are reserved and read as zero in every copy seen.
const INDEX_START: u64 = 32;

#[derive(Debug, Deserialize)]
struct Index {
    #[serde(rename = "fileInfoList")]
    files: Vec<Entry>,
}

#[derive(Debug, Clone, Deserialize)]
struct Entry {
    path: String,
    #[serde(rename = "byteOffset")]
    byte_offset: u64,
    #[serde(rename = "byteSize")]
    byte_size: u64,
}

/// One opened `.fsarchive`: where its data section begins, and the index of what is in
/// it. Opening only reads the header and the index - typically a few hundred kilobytes -
/// never the file data itself.
pub struct FsArchive {
    path: PathBuf,
    data_start: u64,
    entries: Vec<Entry>,
}

impl FsArchive {
    pub fn open(path: &Path) -> Result<FsArchive> {
        let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let mut head = [0u8; 16];
        f.read_exact(&mut head).with_context(|| format!("read header of {}", path.display()))?;
        if head[0..4] != MAGIC {
            bail!("{} is not an .fsarchive (bad magic)", path.display());
        }
        let index_len = u32::from_le_bytes([head[12], head[13], head[14], head[15]]) as usize;
        let mut index_bytes = vec![0u8; index_len];
        f.seek(SeekFrom::Start(INDEX_START)).with_context(|| format!("seek into {}", path.display()))?;
        f.read_exact(&mut index_bytes).with_context(|| format!("read index of {}", path.display()))?;
        // NUL-padded out to the declared length; parse only up to the first NUL.
        let end = index_bytes.iter().position(|&b| b == 0).unwrap_or(index_bytes.len());
        let index: Index = serde_json::from_slice(&index_bytes[..end]).with_context(|| format!("parse index of {}", path.display()))?;
        Ok(FsArchive { path: path.to_path_buf(), data_start: INDEX_START + index_len as u64, entries: index.files })
    }

    /// How many files the archive holds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every path in the archive, in index order.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.path.as_str())
    }

    /// One file's bytes, by its path exactly as the index spells it (backslashes and
    /// all). A single open-seek-read: cheap enough to call once per file wanted.
    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        let entry = self.entries.iter().find(|e| e.path.eq_ignore_ascii_case(path)).with_context(|| format!("{path} not found in {}", self.path.display()))?;
        let mut f = File::open(&self.path)?;
        Self::read_entry(&mut f, self.data_start, entry)
    }

    /// Every file whose name (the part after the last `\` or `/`) starts with one of
    /// `prefixes`, handed to `visit` one at a time as it is read. One `File` is kept open
    /// across the whole walk rather than reopened per entry, which matters when `nax`
    /// alone is some 1,500 files.
    pub fn for_each_with_prefix(&self, prefixes: &[&str], mut visit: impl FnMut(&str, Vec<u8>)) -> Result<()> {
        let mut f = File::open(&self.path).with_context(|| format!("open {}", self.path.display()))?;
        let lower: Vec<String> = prefixes.iter().map(|p| p.to_ascii_lowercase()).collect();
        for entry in &self.entries {
            let name = entry.path.rsplit(['\\', '/']).next().unwrap_or(&entry.path).to_ascii_lowercase();
            if !lower.iter().any(|p| name.starts_with(p.as_str())) {
                continue;
            }
            let data = Self::read_entry(&mut f, self.data_start, entry)?;
            visit(&entry.path, data);
        }
        Ok(())
    }

    fn read_entry(f: &mut File, data_start: u64, entry: &Entry) -> Result<Vec<u8>> {
        f.seek(SeekFrom::Start(data_start + entry.byte_offset)).with_context(|| format!("seek to {}", entry.path))?;
        let mut buf = vec![0u8; entry.byte_size as usize];
        f.read_exact(&mut buf).with_context(|| format!("read {}", entry.path))?;
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Builds a tiny, synthetic `.fsarchive` in memory: real navigation data is never
    /// committed, so the test fixture is two made-up files with made-up bytes.
    fn synthetic_archive() -> Vec<u8> {
        let files = [("scenery\\0000\\nax00000.bgl", b"hello bgl".to_vec()), ("scenery\\0000\\nvx00000.bgl", b"beacon bytes".to_vec())];
        let mut data = Vec::new();
        let mut file_info = Vec::new();
        for (path, bytes) in &files {
            file_info.push(format!(
                r#"{{"path":"{path}","byteOffset":{off},"byteSize":{size},"uncompressed_size":{size},"hash":0}}"#,
                path = path.replace('\\', "\\\\"),
                off = data.len(),
                size = bytes.len()
            ));
            data.extend_from_slice(bytes);
        }
        let index = format!(r#"{{"encryptionSetup":{{"scheme":"notEncrypted","version":0}},"fileInfoList":[{}]}}"#, file_info.join(","));
        let mut index_bytes = index.into_bytes();
        // Pad to a round length the way the real files are NUL-padded.
        while index_bytes.len() % 16 != 0 {
            index_bytes.push(0);
        }
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(files.len() as u32).to_le_bytes());
        out.extend_from_slice(&(index_bytes.len() as u32).to_le_bytes());
        out.resize(INDEX_START as usize, 0); // the 16 bytes after the header are reserved
        out.extend_from_slice(&index_bytes);
        out.extend_from_slice(&data);
        out
    }

    fn write_temp(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join("amdbgen-fsarchive-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::File::create(&path).unwrap().write_all(bytes).unwrap();
        path
    }

    #[test]
    fn reads_a_named_file_back_out() {
        let path = write_temp("roundtrip.fsarchive", &synthetic_archive());
        let archive = FsArchive::open(&path).unwrap();
        assert_eq!(archive.len(), 2);
        assert_eq!(archive.read("scenery\\0000\\nax00000.bgl").unwrap(), b"hello bgl");
        assert_eq!(archive.read("scenery\\0000\\nvx00000.bgl").unwrap(), b"beacon bytes");
    }

    #[test]
    fn walks_only_the_requested_prefix() {
        let path = write_temp("prefix.fsarchive", &synthetic_archive());
        let archive = FsArchive::open(&path).unwrap();
        let mut seen = Vec::new();
        archive.for_each_with_prefix(&["nax"], |p, data| seen.push((p.to_string(), data))).unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "scenery\\0000\\nax00000.bgl");
        assert_eq!(seen[0].1, b"hello bgl");
    }

    #[test]
    fn a_bad_magic_is_reported_rather_than_misread() {
        let mut bytes = synthetic_archive();
        bytes[0] = b'X';
        let path = write_temp("bad_magic.fsarchive", &bytes);
        assert!(FsArchive::open(&path).is_err());
    }

    #[test]
    fn an_unknown_path_is_an_error_not_a_panic() {
        let path = write_temp("missing.fsarchive", &synthetic_archive());
        let archive = FsArchive::open(&path).unwrap();
        assert!(archive.read("scenery\\0000\\nax99999.bgl").is_err());
    }
}
