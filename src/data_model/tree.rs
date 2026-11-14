//! Hierarchical Git tree object and binary serialization.

use std::{collections::BTreeMap, io, path::PathBuf};

use super::{database::Database, entry::Entry, object::GitObject};

/// In-memory representation of a Git `tree` object.
pub struct Tree {
    oid: Option<String>,
    entries: Vec<Entry>,
}

impl Tree {
    /// Creates a `Tree` from a list of direct child `entries` with no assigned OID.
    pub fn new(entries: Vec<Entry>) -> Self {
        Tree { oid: None, entries }
    }

    /// Returns a slice of the direct entries in this tree.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Recursively builds and stores a hierarchy of `Tree` objects from `(rel_path, oid, mode)` items.
    ///
    /// Child subtrees are stored bottom-up with mode `"40000"` before storing and returning the root `Tree`.
    pub fn build(items: Vec<(PathBuf, String, String)>, db: &Database) -> io::Result<Tree> {
        let mut entries = Vec::new();
        let mut subdirs: BTreeMap<String, Vec<(PathBuf, String, String)>> = BTreeMap::new();

        for (rel_path, oid, mode) in items {
            let mut components = rel_path.components();
            let first = components
                .next()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Empty entry path"))?
                .as_os_str()
                .to_string_lossy()
                .into_owned();
            let rest: PathBuf = components.collect();

            if rest.as_os_str().is_empty() {
                entries.push(Entry::new(first, oid, mode));
            } else {
                subdirs.entry(first).or_default().push((rest, oid, mode));
            }
        }

        for (dir_name, sub_items) in subdirs {
            let subtree = Tree::build(sub_items, db)?;
            let sub_oid = subtree
                .oid()
                .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Subtree OID missing"))?
                .to_string();
            entries.push(Entry::new(dir_name, sub_oid, "40000"));
        }

        let mut tree = Tree::new(entries);
        db.store(&mut tree)?;
        Ok(tree)
    }
}

/// Computes Git's byte-comparison sort key for a tree entry.
fn sort_key(entry: &Entry) -> Vec<u8> {
    let mut key = entry.name.as_bytes().to_vec();
    if entry.is_dir() {
        // Git sorts directory entries as if their name had a trailing '/'.
        key.push(b'/');
    }
    key
}

impl GitObject for Tree {
    /// Returns `"tree"`.
    fn kind(&self) -> &str {
        "tree"
    }

    /// Serializes sorted tree entries into Git's binary `<mode> <name>\0<20-byte SHA-1>` format.
    fn to_bytes(&self) -> Vec<u8> {
        let mut sorted = self.entries.clone();
        sorted.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));

        let mut out = Vec::new();
        for entry in &sorted {
            // Binary entry wire layout: "<mode> <name>\0<20-byte raw SHA-1>".
            let raw_oid = hex::decode(&entry.oid).expect("entry oid must be valid 40-char hex");
            out.extend_from_slice(entry.mode.as_bytes());
            out.push(b' ');
            out.extend_from_slice(entry.name.as_bytes());
            out.push(0);
            out.extend_from_slice(&raw_oid);
        }
        out
    }

    /// Returns the tree's 40-character hex SHA-1 OID if computed.
    fn oid(&self) -> Option<&str> {
        self.oid.as_deref()
    }

    /// Sets the tree's 40-character hex SHA-1 OID.
    fn set_oid(&mut self, oid: String) {
        self.oid = Some(oid);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use uuid::Uuid;

    use super::*;

    fn temp_db_dir() -> PathBuf {
        std::env::temp_dir().join(format!("girst_tree_test_{}", Uuid::new_v4()))
    }

    #[test]
    fn tree_sort_order_and_binary_wire_layout() {
        let oid_file = "1111111111111111111111111111111111111111";
        let oid_dir = "2222222222222222222222222222222222222222";
        let oid_dash = "3333333333333333333333333333333333333333";

        let tree = Tree::new(vec![
            Entry::new("a.txt", oid_file, "100644"),
            Entry::new("a", oid_dir, "40000"),
            Entry::new("a-b", oid_dash, "100755"),
        ]);

        let mut expected = Vec::new();
        expected.extend_from_slice(b"100755 a-b\0");
        expected.extend_from_slice(&hex::decode(oid_dash).unwrap());
        expected.extend_from_slice(b"100644 a.txt\0");
        expected.extend_from_slice(&hex::decode(oid_file).unwrap());
        expected.extend_from_slice(b"40000 a\0");
        expected.extend_from_slice(&hex::decode(oid_dir).unwrap());

        assert_eq!(tree.to_bytes(), expected);
    }

    #[test]
    fn tree_build_nested_directories_deterministic_oids() {
        let oid_a = "ce013625030ba8dba906f756967f9e9ca394464a".to_string();
        let oid_b = "1111111111111111111111111111111111111111".to_string();

        let items = vec![
            (PathBuf::from("a.txt"), oid_a.clone(), "100644".to_string()),
            (
                PathBuf::from("dir/sub/b.txt"),
                oid_b.clone(),
                "100755".to_string(),
            ),
        ];

        let db1_path = temp_db_dir();
        let db2_path = temp_db_dir();
        let db1 = Database::new(db1_path.clone());
        let db2 = Database::new(db2_path.clone());

        let tree1 = Tree::build(items.clone(), &db1).expect("build tree1");
        let tree2 = Tree::build(items, &db2).expect("build tree2");

        assert_eq!(tree1.oid(), tree2.oid());
        let root_oid = tree1.oid().expect("root oid");
        assert!(db1_path.join(&root_oid[0..2]).join(&root_oid[2..]).exists());

        let dir_entry = tree1
            .entries()
            .iter()
            .find(|e| e.name == "dir")
            .expect("dir entry");
        assert_eq!(dir_entry.mode, "40000");
        assert!(db1_path
            .join(&dir_entry.oid[0..2])
            .join(&dir_entry.oid[2..])
            .exists());

        let _ = fs::remove_dir_all(&db1_path);
        let _ = fs::remove_dir_all(&db2_path);
    }
}
