//! Loose Git object database backed by `.girst/objects`.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read, Write},
    path::PathBuf,
};

use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use rand::Rng;
use sha1::{Digest, Sha1};

use super::{blob::Blob, commit::Commit, entry::Entry, object::GitObject, tree::Tree};

/// Decoded loose Git object loaded from the object database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedObject {
    /// Parsed `blob` object containing raw file bytes.
    Blob(Blob),
    /// Parsed `tree` object containing sorted directory entries.
    Tree(Tree),
    /// Parsed `commit` object containing tree, optional parent, author, committer, and message.
    Commit(Commit),
}

/// Content-addressable loose object store rooted at an `objects` directory.
pub struct Database {
    path: PathBuf,
}

impl Database {
    /// Creates a `Database` rooted at `path` (typically `.girst/objects`).
    pub fn new(path: PathBuf) -> Self {
        Database { path }
    }

    /// Hashes `object` with its Git header, assigns its OID, and writes it if not already present.
    ///
    /// Propagates any filesystem I/O error encountered while creating directories or writing the object.
    pub fn store<T: GitObject>(&self, object: &mut T) -> io::Result<()> {
        let payload = object.to_bytes();
        let header = format!("{} {}\0", object.kind(), payload.len());
        let mut content = Vec::with_capacity(header.len() + payload.len());
        content.extend_from_slice(header.as_bytes());
        content.extend_from_slice(&payload);

        let mut hasher = Sha1::new();
        hasher.update(&content);
        let oid = hex::encode(hasher.finalize());
        object.set_oid(oid.clone());

        self.write_object(&oid, &content)
    }

    /// Reads, decompresses, verifies, and parses the loose object identified by 40-char hex `oid`.
    pub fn load(&self, oid: &str) -> io::Result<ParsedObject> {
        if !is_valid_hex(oid, 40..=40) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid object id: {oid}"),
            ));
        }
        let oid_lower = oid.to_ascii_lowercase();
        let object_path = self.path.join(&oid_lower[0..2]).join(&oid_lower[2..]);
        let compressed = fs::read(&object_path)?;

        let mut decoder = ZlibDecoder::new(compressed.as_slice());
        let mut raw = Vec::new();
        decoder.read_to_end(&mut raw).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("invalid zlib stream: {e}"))
        })?;

        let nul_pos = raw.iter().position(|&b| b == 0).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "missing object header NUL")
        })?;
        let header = std::str::from_utf8(&raw[..nul_pos]).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 object header")
        })?;
        let (kind, len_str) = header.split_once(' ').ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "malformed object header")
        })?;
        if !len_str.bytes().all(|b| b.is_ascii_digit()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid object length",
            ));
        }
        let byte_len: usize = len_str.parse().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid object length")
        })?;
        let payload = &raw[nul_pos + 1..];
        if payload.len() != byte_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "object byte length mismatch",
            ));
        }

        let mut hasher = Sha1::new();
        hasher.update(&raw);
        let actual_oid = hex::encode(hasher.finalize());
        if actual_oid != oid_lower {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("object hash mismatch: expected {oid_lower}, got {actual_oid}"),
            ));
        }

        match kind {
            "blob" => {
                let mut blob = Blob::new(payload.to_vec());
                blob.set_oid(oid_lower);
                Ok(ParsedObject::Blob(blob))
            }
            "tree" => {
                let entries = parse_tree_payload(payload)?;
                let mut tree = Tree::new(entries);
                tree.set_oid(oid_lower);
                Ok(ParsedObject::Tree(tree))
            }
            "commit" => {
                let mut commit = parse_commit_payload(payload)?;
                commit.set_oid(oid_lower);
                Ok(ParsedObject::Commit(commit))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown object kind: {kind}"),
            )),
        }
    }

    /// Resolves a 4..=40 hex `prefix` to sorted full 40-char lowercase OIDs present in `.girst/objects`.
    pub fn resolve_prefix(&self, prefix: &str) -> io::Result<Vec<String>> {
        if !is_valid_hex(prefix, 4..=40) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid object id prefix: {prefix}"),
            ));
        }
        let prefix_lower = prefix.to_ascii_lowercase();
        let (dir_prefix, file_prefix) = prefix_lower.split_at(2);
        let fanout_dir = self.path.join(dir_prefix);
        if !fanout_dir.exists() {
            return Ok(Vec::new());
        }

        let mut matches = Vec::new();
        for entry in fs::read_dir(&fanout_dir)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(name_str) = file_name.to_str() else {
                continue;
            };
            if is_valid_hex(name_str, 38..=38) {
                let name_lower = name_str.to_ascii_lowercase();
                if name_lower.starts_with(file_prefix) {
                    matches.push(format!("{dir_prefix}{name_lower}"));
                }
            }
        }
        matches.sort();
        Ok(matches)
    }

    /// Recursively flattens a root tree object into `rel_path -> (oid, normalized_mode)` leaf entries.
    pub fn load_tree_map(&self, root_tree_oid: &str) -> io::Result<BTreeMap<String, (String, String)>> {
        let mut out = BTreeMap::new();
        self.collect_tree_entries(root_tree_oid, "", &mut out)?;
        Ok(out)
    }

    /// Recursively walks `tree_oid` and inserts flattened leaf entries under `prefix` into `out`.
    fn collect_tree_entries(
        &self,
        tree_oid: &str,
        prefix: &str,
        out: &mut BTreeMap<String, (String, String)>,
    ) -> io::Result<()> {
        let ParsedObject::Tree(tree) = self.load(tree_oid)? else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("object {tree_oid} is not a tree"),
            ));
        };

        for entry in tree.entries() {
            let rel_path = if prefix.is_empty() {
                entry.name.clone()
            } else {
                format!("{prefix}/{}", entry.name)
            };
            if entry.is_dir() {
                self.collect_tree_entries(&entry.oid, &rel_path, out)?;
            } else {
                let norm_mode = normalize_file_mode(&entry.mode)?;
                out.insert(rel_path, (entry.oid.clone(), norm_mode));
            }
        }
        Ok(())
    }

    /// Writes `content` compressed with zlib to `<db>/<oid[0..2]>/<oid[2..]>` atomically via temp file.
    fn write_object(&self, oid: &str, content: &[u8]) -> io::Result<()> {
        let object_path = self.path.join(&oid[0..2]).join(&oid[2..]);
        if object_path.exists() {
            return Ok(());
        }

        let dirname = object_path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "Parent directory not found")
        })?;
        fs::create_dir_all(dirname)?;

        let temp_path = dirname.join(format!("tmp_obj_{}", generate_random_string(14)));
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(content)?;
        let compressed = encoder.finish()?;

        let mut file = File::create(&temp_path)?;
        file.write_all(&compressed)?;
        fs::rename(&temp_path, object_path)?;
        Ok(())
    }
}

/// Returns `true` if `s` has a byte length within `range` and contains only ASCII hex digits.
fn is_valid_hex(s: &str, range: std::ops::RangeInclusive<usize>) -> bool {
    range.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Normalizes a Git file mode string into `"100644"` or `"100755"`.
fn normalize_file_mode(mode: &str) -> io::Result<String> {
    let parsed = u32::from_str_radix(mode, 8).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, format!("invalid tree mode: {mode}"))
    })?;
    if parsed & 0o111 != 0 {
        Ok("100755".to_string())
    } else {
        Ok("100644".to_string())
    }
}

/// Parses raw Git tree payload bytes into `Entry` records.
fn parse_tree_payload(payload: &[u8]) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut pos = 0;

    while pos < payload.len() {
        // Advance by explicit byte offsets because raw 20-byte OIDs may contain 0x00 or 0x0a.
        let space_rel = payload[pos..].iter().position(|&b| b == b' ').ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "malformed tree entry mode")
        })?;
        let mode_end = pos + space_rel;
        let mode = std::str::from_utf8(&payload[pos..mode_end]).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 tree entry mode")
        })?;
        if mode.is_empty() || !mode.bytes().all(|b| matches!(b, b'0'..=b'7')) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid tree entry mode",
            ));
        }

        let name_start = mode_end + 1;
        let nul_rel = payload[name_start..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "missing NUL after tree entry name")
            })?;
        let name_end = name_start + nul_rel;
        let name = std::str::from_utf8(&payload[name_start..name_end]).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 tree entry name")
        })?;
        if name.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "empty tree entry name",
            ));
        }

        let oid_start = name_end + 1;
        let oid_end = oid_start + 20;
        if oid_end > payload.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated tree entry oid",
            ));
        }
        let oid = hex::encode(&payload[oid_start..oid_end]);
        entries.push(Entry::new(name, oid, mode));
        pos = oid_end;
    }

    Ok(entries)
}

/// Parses raw Git commit payload bytes into a `Commit`.
fn parse_commit_payload(payload: &[u8]) -> io::Result<Commit> {
    let text = std::str::from_utf8(payload).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 commit payload")
    })?;
    let (header_block, message) = text.split_once("\n\n").ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing commit header separator")
    })?;

    let mut tree: Option<String> = None;
    let mut parent: Option<String> = None;
    let mut author: Option<String> = None;
    let mut committer: Option<String> = None;

    for line in header_block.lines() {
        let (key, value) = line.split_once(' ').ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "malformed commit header line")
        })?;
        if value.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "empty commit header value",
            ));
        }
        match key {
            "tree" => {
                if tree.is_some() || !is_valid_hex(value, 40..=40) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid commit tree header",
                    ));
                }
                tree = Some(value.to_ascii_lowercase());
            }
            "parent" => {
                if !is_valid_hex(value, 40..=40) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid commit parent header",
                    ));
                }
                parent = Some(value.to_ascii_lowercase());
            }
            "author" => author = Some(value.to_string()),
            "committer" => committer = Some(value.to_string()),
            _ => {}
        }
    }

    let tree = tree.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing commit tree header")
    })?;
    let author = author.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing commit author header")
    })?;
    let committer = committer.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing commit committer header")
    })?;

    Ok(Commit::from_parts(
        tree,
        parent,
        author,
        committer,
        message.to_string(),
    ))
}

/// Generates an alphanumeric suffix of `length` characters for temporary object filenames.
fn generate_random_string(length: usize) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();

    (0..length)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use uuid::Uuid;

    use super::*;
    use crate::data_model::blob::Blob;

    fn temp_db_dir() -> PathBuf {
        std::env::temp_dir().join(format!("girst_db_test_{}", Uuid::new_v4()))
    }

    #[test]
    fn blob_hello_hashes_to_git_oid() {
        let db_path = temp_db_dir();
        let db = Database::new(db_path.clone());
        let mut blob = Blob::new(b"hello\n".to_vec());

        db.store(&mut blob).expect("store should succeed");
        assert_eq!(blob.oid(), Some("ce013625030ba8dba906f756967f9e9ca394464a"));

        let _ = fs::remove_dir_all(&db_path);
    }

    #[test]
    fn store_writes_zlib_object_and_skips_existing() {
        let db_path = temp_db_dir();
        let db = Database::new(db_path.clone());
        let mut blob = Blob::new(b"hello\n".to_vec());

        db.store(&mut blob).expect("initial store should succeed");
        let obj_path = db_path
            .join("ce")
            .join("013625030ba8dba906f756967f9e9ca394464a");
        assert!(obj_path.exists(), "loose object must exist at 2/38 path");

        let output = Command::new("python3")
            .arg("-c")
            .arg("import sys, zlib; sys.stdout.buffer.write(zlib.decompress(open(sys.argv[1], 'rb').read()))")
            .arg(&obj_path)
            .output()
            .expect("python3 should run");
        assert!(output.status.success(), "python3 zlib.decompress failed");
        assert_eq!(output.stdout, b"blob 6\0hello\n");

        // Overwrite with sentinel bytes to verify subsequent store skips writing when file exists.
        fs::write(&obj_path, b"sentinel").expect("write sentinel");
        let mut duplicate = Blob::new(b"hello\n".to_vec());
        db.store(&mut duplicate)
            .expect("duplicate store should succeed");
        assert_eq!(
            fs::read(&obj_path).expect("read sentinel"),
            b"sentinel",
            "store should skip rewriting existing object"
        );

        let _ = fs::remove_dir_all(&db_path);
    }

    fn write_raw_loose_object(db_path: &PathBuf, raw: &[u8]) -> String {
        let mut hasher = Sha1::new();
        hasher.update(raw);
        let oid = hex::encode(hasher.finalize());
        let dir = db_path.join(&oid[0..2]);
        fs::create_dir_all(&dir).expect("create fanout dir");
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(raw).expect("compress");
        let compressed = encoder.finish().expect("finish compress");
        fs::write(dir.join(&oid[2..]), compressed).expect("write raw object");
        oid
    }

    #[test]
    fn load_round_trip_blob_nested_tree_commit_and_prefix() {
        use crate::data_model::author::Author;

        let db_path = temp_db_dir();
        let db = Database::new(db_path.clone());

        let mut bin_blob = Blob::new(vec![0x00, 0xff, 0x0a, 0x42]);
        db.store(&mut bin_blob).expect("store binary blob");
        let bin_oid = bin_blob.oid().expect("blob oid").to_string();
        assert_eq!(
            db.load(&bin_oid).expect("load blob"),
            ParsedObject::Blob(bin_blob)
        );

        let oid_with_nul_lf = "000a112233445566778899aabbccddeeff000a11";
        let mut sub_tree = Tree::new(vec![Entry::new("raw.bin", oid_with_nul_lf, "100755")]);
        db.store(&mut sub_tree).expect("store sub_tree");
        let sub_oid = sub_tree.oid().expect("sub_tree oid").to_string();

        let mut root_tree = Tree::new(vec![
            Entry::new("sub", &sub_oid, "40000"),
            Entry::new("top.bin", &bin_oid, "100644"),
        ]);
        db.store(&mut root_tree).expect("store root_tree");
        let root_oid = root_tree.oid().expect("root_tree oid").to_string();

        assert_eq!(
            db.load(&root_oid).expect("load root_tree"),
            ParsedObject::Tree(root_tree)
        );

        let tree_map = db.load_tree_map(&root_oid).expect("load_tree_map");
        assert_eq!(
            tree_map.get("top.bin"),
            Some(&(bin_oid.clone(), "100644".to_string()))
        );
        assert_eq!(
            tree_map.get("sub/raw.bin"),
            Some(&(oid_with_nul_lf.to_string(), "100755".to_string()))
        );
        assert_eq!(tree_map.len(), 2);

        let author = Author::new("Ada", "ada@example.com", 1700000000, "+0000");
        let mut commit = Commit::new(&root_oid, None, &author, "initial commit\n");
        db.store(&mut commit).expect("store commit");
        let commit_oid = commit.oid().expect("commit oid").to_string();
        assert_eq!(
            db.load(&commit_oid).expect("load commit"),
            ParsedObject::Commit(commit)
        );

        let matches = db
            .resolve_prefix(&commit_oid[..6])
            .expect("resolve_prefix");
        assert_eq!(matches, vec![commit_oid]);
        assert!(db.resolve_prefix("ffff").expect("missing prefix").is_empty());

        let _ = fs::remove_dir_all(&db_path);
    }

    #[test]
    fn load_rejects_malformed_header_length_and_truncated_tree() {
        let db_path = temp_db_dir();
        let db = Database::new(db_path.clone());

        let bad_header_oid = write_raw_loose_object(&db_path, b"blob_no_space\0abc");
        let err = db.load(&bad_header_oid).expect_err("bad header must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        let len_mismatch_oid = write_raw_loose_object(&db_path, b"blob 99\0abc");
        let err = db
            .load(&len_mismatch_oid)
            .expect_err("length mismatch must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        let truncated_tree = b"tree 19\0100644 a.txt\0\x01\x02\x03\x04\x05";
        let truncated_oid = write_raw_loose_object(&db_path, truncated_tree);
        let err = db
            .load(&truncated_oid)
            .expect_err("truncated tree must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        let _ = fs::remove_dir_all(&db_path);
    }
}
