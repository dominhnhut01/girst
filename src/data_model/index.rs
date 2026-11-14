//! Git binary index (`.girst/index`) `DIRC` v2 codec and entry metadata representation.

use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use sha1::{Digest, Sha1};

use super::lockfile::Lockfile;

const DIRC_MAGIC: &[u8; 4] = b"DIRC";
const DIRC_VERSION: u32 = 2;
const HEADER_LEN: usize = 12;
const CHECKSUM_LEN: usize = 20;
const ENTRY_FIXED_LEN: usize = 62;
const REGULAR_MODE: u32 = 0o100644;
const EXECUTABLE_MODE: u32 = 0o100755;
const MAX_FLAG_PATH_LEN: usize = 0x0fff;

/// Single file entry stored in the Git `DIRC` v2 binary index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub ctime_sec: u32,
    pub ctime_nsec: u32,
    pub mtime_sec: u32,
    pub mtime_nsec: u32,
    pub dev: u32,
    pub ino: u32,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u32,
    pub oid: [u8; 20],
    pub flags: u16,
    pub path: String,
}

impl IndexEntry {
    /// Builds an `IndexEntry` from a workspace-relative UTF-8 `path`, raw 20-byte `oid`, and Unix `meta`.
    pub fn from_metadata(path: impl Into<String>, oid: [u8; 20], meta: &fs::Metadata) -> Self {
        let path = path.into();
        let mode = if meta.mode() & 0o111 != 0 {
            EXECUTABLE_MODE
        } else {
            REGULAR_MODE
        };
        let flags = path.len().min(MAX_FLAG_PATH_LEN) as u16;

        IndexEntry {
            ctime_sec: meta.ctime() as u32,
            ctime_nsec: meta.ctime_nsec() as u32,
            mtime_sec: meta.mtime() as u32,
            mtime_nsec: meta.mtime_nsec() as u32,
            dev: meta.dev() as u32,
            ino: meta.ino() as u32,
            mode,
            uid: meta.uid(),
            gid: meta.gid(),
            size: meta.size() as u32,
            oid,
            flags,
            path,
        }
    }

    /// Appends this entry's big-endian `DIRC` v2 wire bytes and 8-byte alignment NUL padding to `out`.
    fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.ctime_sec.to_be_bytes());
        out.extend_from_slice(&self.ctime_nsec.to_be_bytes());
        out.extend_from_slice(&self.mtime_sec.to_be_bytes());
        out.extend_from_slice(&self.mtime_nsec.to_be_bytes());
        out.extend_from_slice(&self.dev.to_be_bytes());
        out.extend_from_slice(&self.ino.to_be_bytes());
        out.extend_from_slice(&self.mode.to_be_bytes());
        out.extend_from_slice(&self.uid.to_be_bytes());
        out.extend_from_slice(&self.gid.to_be_bytes());
        out.extend_from_slice(&self.size.to_be_bytes());
        out.extend_from_slice(&self.oid);
        out.extend_from_slice(&self.flags.to_be_bytes());
        out.extend_from_slice(self.path.as_bytes());

        // Git DIRC v2 pads each entry with 1..=8 NUL bytes to an 8-byte boundary.
        let pad_len = entry_pad_len(self.path.len());
        out.resize(out.len() + pad_len, 0);
    }
}

/// Decodes a 40-character hexadecimal OID string into a raw 20-byte array.
pub fn hex_to_oid(hex_oid: &str) -> io::Result<[u8; 20]> {
    let raw = hex::decode(hex_oid)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("invalid hex oid: {e}")))?;
    raw.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid oid length: expected 20 bytes",
        )
    })
}

/// Manages reading and atomically writing the `.girst/index` binary file.
pub struct Index {
    path: PathBuf,
    entries: Vec<IndexEntry>,
    lockfile: Option<Lockfile>,
}

impl Index {
    /// Creates an empty `Index` handle pointing at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Index {
            path: path.into(),
            entries: Vec::new(),
            lockfile: None,
        }
    }

    /// Returns the index file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns a slice of currently loaded or staged entries.
    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries
    }

    /// Returns a mutable reference to the entries list.
    pub fn entries_mut(&mut self) -> &mut Vec<IndexEntry> {
        &mut self.entries
    }

    /// Loads entries from `.girst/index`, clearing entries if the file does not exist yet.
    pub fn load(&mut self) -> io::Result<()> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.entries.clear();
                return Ok(());
            }
            Err(e) => return Err(e),
        };

        self.entries = decode_dirc_v2(&bytes)?;
        Ok(())
    }

    /// Acquires `<index>.lock` and loads the existing index contents.
    pub fn load_for_update(&mut self) -> io::Result<()> {
        let lock = Lockfile::hold_for_update(&self.path)?;
        self.lockfile = Some(lock);
        if let Err(e) = self.load() {
            self.lockfile = None;
            return Err(e);
        }
        Ok(())
    }

    /// Encodes the current entries as `DIRC` v2 and commits them through the held lockfile.
    pub fn write_updates(&mut self) -> io::Result<()> {
        let mut lock = self
            .lockfile
            .take()
            .ok_or_else(|| io::Error::other("index lockfile is not held"))?;

        let encoded = encode_dirc_v2(&self.entries)?;
        lock.write(&encoded)?;
        lock.commit()
    }

    /// Releases the held lockfile without modifying `.girst/index`.
    pub fn release_lock(&mut self) -> io::Result<()> {
        if let Some(lock) = self.lockfile.take() {
            lock.rollback()?;
        }
        Ok(())
    }
}

/// Computes the `1..=8` NUL padding length required to align an entry to an 8-byte boundary.
fn entry_pad_len(path_len: usize) -> usize {
    8 - ((ENTRY_FIXED_LEN + path_len) % 8)
}

/// Encodes `entries` into a complete `DIRC` v2 binary payload with trailing 20-byte SHA-1 checksum.
fn encode_dirc_v2(entries: &[IndexEntry]) -> io::Result<Vec<u8>> {
    let count = u32::try_from(entries.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many index entries"))?;

    let mut out = Vec::with_capacity(HEADER_LEN + entries.len() * (ENTRY_FIXED_LEN + 16) + CHECKSUM_LEN);
    out.extend_from_slice(DIRC_MAGIC);
    out.extend_from_slice(&DIRC_VERSION.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());

    for entry in entries {
        entry.write_to(&mut out);
    }

    let digest = Sha1::digest(&out);
    out.extend_from_slice(&digest);
    Ok(out)
}

/// Validates and decodes a `DIRC` v2 binary index payload.
fn decode_dirc_v2(bytes: &[u8]) -> io::Result<Vec<IndexEntry>> {
    if bytes.len() < HEADER_LEN + CHECKSUM_LEN {
        return Err(invalid_data("index file too short"));
    }

    let (payload, trailer) = bytes.split_at(bytes.len() - CHECKSUM_LEN);
    let expected_digest = Sha1::digest(payload);
    if expected_digest.as_slice() != trailer {
        return Err(invalid_data("index checksum mismatch"));
    }

    if &payload[0..4] != DIRC_MAGIC {
        return Err(invalid_data("invalid index magic header"));
    }

    let mut pos = 4;
    let version = read_u32_be(payload, &mut pos)?;
    if version != DIRC_VERSION {
        return Err(invalid_data(format!("unsupported index version: {version}")));
    }
    let count = read_u32_be(payload, &mut pos)? as usize;

    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let entry = parse_entry(payload, &mut pos)?;
        entries.push(entry);
    }

    if pos != payload.len() {
        return Err(invalid_data("trailing bytes before index checksum"));
    }

    Ok(entries)
}

/// Parses a single `DIRC` v2 entry starting at `*pos` and advances `*pos` past its NUL padding.
fn parse_entry(payload: &[u8], pos: &mut usize) -> io::Result<IndexEntry> {
    if *pos + ENTRY_FIXED_LEN > payload.len() {
        return Err(invalid_data("truncated index entry header"));
    }

    let ctime_sec = read_u32_be(payload, pos)?;
    let ctime_nsec = read_u32_be(payload, pos)?;
    let mtime_sec = read_u32_be(payload, pos)?;
    let mtime_nsec = read_u32_be(payload, pos)?;
    let dev = read_u32_be(payload, pos)?;
    let ino = read_u32_be(payload, pos)?;
    let mode = read_u32_be(payload, pos)?;
    let uid = read_u32_be(payload, pos)?;
    let gid = read_u32_be(payload, pos)?;
    let size = read_u32_be(payload, pos)?;

    let oid: [u8; 20] = payload[*pos..*pos + 20]
        .try_into()
        .map_err(|_| invalid_data("truncated index entry oid"))?;
    *pos += 20;

    let flags = u16::from_be_bytes([payload[*pos], payload[*pos + 1]]);
    *pos += 2;

    let path_start = *pos;
    let nul_rel = payload[path_start..]
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| invalid_data("unterminated index entry path"))?;
    if nul_rel == 0 {
        return Err(invalid_data("empty index entry path"));
    }

    let path_bytes = &payload[path_start..path_start + nul_rel];
    let path = std::str::from_utf8(path_bytes)
        .map_err(|_| invalid_data("non-utf8 index entry path"))?
        .to_string();

    let pad_len = entry_pad_len(path_bytes.len());
    let next_pos = path_start + path_bytes.len() + pad_len;
    if next_pos > payload.len() {
        return Err(invalid_data("truncated index entry padding"));
    }
    if payload[path_start + path_bytes.len()..next_pos]
        .iter()
        .any(|&b| b != 0)
    {
        return Err(invalid_data("non-zero index entry padding"));
    }

    *pos = next_pos;
    Ok(IndexEntry {
        ctime_sec,
        ctime_nsec,
        mtime_sec,
        mtime_nsec,
        dev,
        ino,
        mode,
        uid,
        gid,
        size,
        oid,
        flags,
        path,
    })
}

/// Reads a big-endian `u32` from `buf` at `*pos` and advances `*pos` by 4.
fn read_u32_be(buf: &[u8], pos: &mut usize) -> io::Result<u32> {
    let bytes: [u8; 4] = buf
        .get(*pos..*pos + 4)
        .ok_or_else(|| invalid_data("unexpected end of index payload"))?
        .try_into()
        .map_err(|_| invalid_data("unexpected end of index payload"))?;
    *pos += 4;
    Ok(u32::from_be_bytes(bytes))
}

/// Builds an `io::Error` with `InvalidData`.
fn invalid_data(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use uuid::Uuid;

    use super::*;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("girst_index_test_{}", Uuid::new_v4()))
    }

    #[test]
    fn index_dirc_v2_round_trip_and_missing_loads_empty() {
        let root = temp_dir();
        let girst_dir = root.join(".girst");
        fs::create_dir_all(root.join("dir")).unwrap();
        fs::create_dir_all(&girst_dir).unwrap();

        let index_path = girst_dir.join("index");
        let mut index = Index::new(&index_path);
        index.load().expect("missing index should load empty");
        assert!(index.entries().is_empty());

        fs::write(root.join("a"), b"1").unwrap();
        fs::write(root.join("ab"), b"12").unwrap();
        let exec_path = root.join("dir/run.sh");
        fs::write(&exec_path, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&exec_path, fs::Permissions::from_mode(0o755)).unwrap();

        let oid1 = hex_to_oid("1111111111111111111111111111111111111111").unwrap();
        let oid2 = hex_to_oid("2222222222222222222222222222222222222222").unwrap();
        let oid3 = hex_to_oid("3333333333333333333333333333333333333333").unwrap();

        let e1 = IndexEntry::from_metadata("a", oid1, &fs::metadata(root.join("a")).unwrap());
        let e2 = IndexEntry::from_metadata("ab", oid2, &fs::metadata(root.join("ab")).unwrap());
        let e3 = IndexEntry::from_metadata("dir/run.sh", oid3, &fs::metadata(&exec_path).unwrap());

        assert_eq!(e1.mode, REGULAR_MODE);
        assert_eq!(e3.mode, EXECUTABLE_MODE);

        index.load_for_update().expect("lock index");
        index.entries_mut().extend([e1.clone(), e2.clone(), e3.clone()]);
        index.write_updates().expect("write index");
        assert!(!girst_dir.join("index.lock").exists());

        let raw = fs::read(&index_path).unwrap();
        assert_eq!(&raw[0..4], b"DIRC");
        assert_eq!(u32::from_be_bytes(raw[4..8].try_into().unwrap()), 2);
        assert_eq!(u32::from_be_bytes(raw[8..12].try_into().unwrap()), 3);

        // Verify exact 8-byte entry alignment and NUL padding lengths:
        // "a" (62+1=63 -> pad 1 -> 64), "ab" (62+2=64 -> pad 8 -> 72), "dir/run.sh" (62+10=72 -> pad 8 -> 80).
        let expected_len = HEADER_LEN + 64 + 72 + 80 + CHECKSUM_LEN;
        assert_eq!(raw.len(), expected_len);
        assert_eq!(&raw[12 + 63..12 + 64], &[0u8; 1]);
        assert_eq!(&raw[12 + 64 + 64..12 + 64 + 72], &[0u8; 8]);
        assert_eq!(&raw[12 + 64 + 72 + 72..12 + 64 + 72 + 80], &[0u8; 8]);

        let mut reloaded = Index::new(&index_path);
        reloaded.load().expect("reload index");
        assert_eq!(reloaded.entries(), &[e1, e2, e3]);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn index_load_rejects_corrupted_checksum_and_invalid_header() {
        let root = temp_dir();
        fs::create_dir_all(&root).unwrap();
        let index_path = root.join("index");

        let file_path = root.join("f.txt");
        fs::write(&file_path, b"hello").unwrap();
        let oid = hex_to_oid("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let entry = IndexEntry::from_metadata("f.txt", oid, &fs::metadata(&file_path).unwrap());

        let mut index = Index::new(&index_path);
        index.load_for_update().unwrap();
        index.entries_mut().push(entry);
        index.write_updates().unwrap();

        let valid = fs::read(&index_path).unwrap();

        // 1. Corrupt SHA-1 trailer.
        let mut bad_sha = valid.clone();
        let last = bad_sha.len() - 1;
        bad_sha[last] ^= 0xff;
        fs::write(&index_path, &bad_sha).unwrap();
        assert_eq!(
            Index::new(&index_path).load().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        // 2. Corrupt magic header with valid SHA-1 trailer.
        let mut bad_magic = valid[..valid.len() - CHECKSUM_LEN].to_vec();
        bad_magic[0..4].copy_from_slice(b"NOPE");
        bad_magic.extend_from_slice(&Sha1::digest(&bad_magic));
        fs::write(&index_path, &bad_magic).unwrap();
        assert_eq!(
            Index::new(&index_path).load().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        // 3. Corrupt version with valid SHA-1 trailer.
        let mut bad_ver = valid[..valid.len() - CHECKSUM_LEN].to_vec();
        bad_ver[4..8].copy_from_slice(&3u32.to_be_bytes());
        bad_ver.extend_from_slice(&Sha1::digest(&bad_ver));
        fs::write(&index_path, &bad_ver).unwrap();
        assert_eq!(
            Index::new(&index_path).load().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        let _ = fs::remove_dir_all(&root);
    }
}
