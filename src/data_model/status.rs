//! Three-way working tree status scanner (`HEAD` vs `.girst/index` vs workspace).

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    io,
    os::unix::fs::MetadataExt,
    path::Path,
};

use sha1::{Digest, Sha1};

use super::{
    database::{Database, ParsedObject},
    index::Index,
    refs::Refs,
    workspace::Workspace,
};

/// Kind of change detected between two repository states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// Entry exists in the target state but is absent from the base state.
    Added,
    /// Entry exists in both states with a differing OID or file mode.
    Modified,
    /// Entry exists in the base state but is absent from the target state.
    Deleted,
}

/// Three-way status summary comparing `HEAD`, `.girst/index`, and the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatusReport {
    /// Staged changes between `HEAD` and `.girst/index`.
    pub staged: BTreeMap<String, ChangeKind>,
    /// Unstaged changes between `.girst/index` and the working tree.
    pub unstaged: BTreeMap<String, ChangeKind>,
    /// Untracked file paths or collapsed untracked directory prefixes (`dir/`).
    pub untracked: Vec<String>,
}

impl StatusReport {
    /// Scans `HEAD`, `index`, and `workspace` to produce a `StatusReport`.
    pub fn collect(
        workspace: &Workspace,
        db: &Database,
        index: &Index,
        refs: &Refs,
    ) -> io::Result<Self> {
        let head_tree = load_head_tree(db, refs)?;
        let staged = diff_head_vs_index(&head_tree, index);
        let unstaged = diff_index_vs_workspace(workspace, index)?;
        let untracked = collect_untracked(workspace, index)?;

        Ok(StatusReport {
            staged,
            unstaged,
            untracked,
        })
    }

    /// Formats the status report in Git `--porcelain` (`XY <path>`) format.
    pub fn format_porcelain(&self) -> String {
        let mut tracked_paths: BTreeSet<&String> = BTreeSet::new();
        tracked_paths.extend(self.staged.keys());
        tracked_paths.extend(self.unstaged.keys());

        let mut out = String::new();
        for path in tracked_paths {
            let x = self.staged.get(path).map_or(' ', change_code);
            let y = self.unstaged.get(path).map_or(' ', change_code);
            out.push_str(&format!("{x}{y} {path}\n"));
        }
        for path in &self.untracked {
            out.push_str(&format!("?? {path}\n"));
        }
        out
    }

    /// Formats the status report in human-readable Git sections.
    pub fn format_human(&self) -> String {
        if self.staged.is_empty() && self.unstaged.is_empty() && self.untracked.is_empty() {
            return "nothing to commit, working tree clean\n".to_string();
        }

        let mut out = String::new();
        if !self.staged.is_empty() {
            out.push_str("Changes to be committed:\n");
            for (path, kind) in &self.staged {
                out.push_str(&format!("\t{:<12}{path}\n", change_label(*kind)));
            }
        }
        if !self.unstaged.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("Changes not staged for commit:\n");
            for (path, kind) in &self.unstaged {
                out.push_str(&format!("\t{:<12}{path}\n", change_label(*kind)));
            }
        }
        if !self.untracked.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("Untracked files:\n");
            for path in &self.untracked {
                out.push_str(&format!("\t{path}\n"));
            }
        }
        out
    }
}

/// Loads the flattened `HEAD` tree map, or returns an empty map when `HEAD` is unborn.
fn load_head_tree(
    db: &Database,
    refs: &Refs,
) -> io::Result<BTreeMap<String, (String, String)>> {
    let Some(commit_oid) = refs.read_head()? else {
        return Ok(BTreeMap::new());
    };
    let ParsedObject::Commit(commit) = db.load(&commit_oid)? else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("HEAD object {commit_oid} is not a commit"),
        ));
    };
    db.load_tree_map(commit.tree())
}

/// Computes staged differences between the `HEAD` tree and `.girst/index`.
fn diff_head_vs_index(
    head_tree: &BTreeMap<String, (String, String)>,
    index: &Index,
) -> BTreeMap<String, ChangeKind> {
    let mut staged = BTreeMap::new();

    for entry in index.entries() {
        let idx_oid = hex::encode(entry.oid);
        let idx_mode = if entry.mode & 0o111 != 0 {
            "100755"
        } else {
            "100644"
        };
        match head_tree.get(&entry.path) {
            Some((head_oid, head_mode)) => {
                if head_oid != &idx_oid || head_mode != idx_mode {
                    staged.insert(entry.path.clone(), ChangeKind::Modified);
                }
            }
            None => {
                staged.insert(entry.path.clone(), ChangeKind::Added);
            }
        }
    }

    for path in head_tree.keys() {
        if index.get(path).is_none() {
            staged.insert(path.clone(), ChangeKind::Deleted);
        }
    }

    staged
}

/// Computes unstaged differences between `.girst/index` and the working tree.
fn diff_index_vs_workspace(
    workspace: &Workspace,
    index: &Index,
) -> io::Result<BTreeMap<String, ChangeKind>> {
    let mut unstaged = BTreeMap::new();

    for entry in index.entries() {
        let meta = match workspace.stat_file(Path::new(&entry.path)) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                unstaged.insert(entry.path.clone(), ChangeKind::Deleted);
                continue;
            }
            Err(e) => return Err(e),
        };

        if !meta.is_file() {
            unstaged.insert(entry.path.clone(), ChangeKind::Deleted);
            continue;
        }

        let ws_mode = if meta.mode() & 0o111 != 0 {
            0o100755
        } else {
            0o100644
        };
        if ws_mode != entry.mode {
            unstaged.insert(entry.path.clone(), ChangeKind::Modified);
            continue;
        }

        let stat_matches = meta.size() as u32 == entry.size
            && meta.mtime() as u32 == entry.mtime_sec
            && meta.mtime_nsec() as u32 == entry.mtime_nsec;
        // Re-hash when mtime_sec == ctime_sec to guard against same-second racily-clean edits.
        let racily_clean = entry.mtime_sec == entry.ctime_sec
            || (meta.mtime() as u32) == (meta.ctime() as u32);

        if stat_matches && !racily_clean {
            continue;
        }

        let bytes = workspace.read_file(Path::new(&entry.path))?;
        let actual_oid = hash_blob_oid(&bytes);
        if actual_oid != entry.oid {
            unstaged.insert(entry.path.clone(), ChangeKind::Modified);
        }
    }

    Ok(unstaged)
}

/// Collects sorted untracked paths, collapsing directories that contain no tracked entries.
fn collect_untracked(workspace: &Workspace, index: &Index) -> io::Result<Vec<String>> {
    let mut tracked_dirs: HashSet<&str> = HashSet::new();
    for entry in index.entries() {
        for (idx, _) in entry.path.match_indices('/') {
            tracked_dirs.insert(&entry.path[..idx]);
        }
    }

    let mut untracked = BTreeSet::new();
    for rel_path in workspace.list_files()? {
        let rel_str = rel_path.to_string_lossy().replace('\\', "/");
        if index.get(&rel_str).is_some() {
            continue;
        }

        // Collapse to the shallowest ancestor directory that has no tracked descendants.
        let mut collapsed = None;
        for (idx, _) in rel_str.match_indices('/') {
            let dir_prefix = &rel_str[..idx];
            if !tracked_dirs.contains(dir_prefix) {
                collapsed = Some(format!("{dir_prefix}/"));
                break;
            }
        }
        untracked.insert(collapsed.unwrap_or(rel_str));
    }

    Ok(untracked.into_iter().collect())
}

/// Computes the raw 20-byte Git `blob` SHA-1 OID in memory without writing to the database.
fn hash_blob_oid(data: &[u8]) -> [u8; 20] {
    let header = format!("blob {}\0", data.len());
    let mut hasher = Sha1::new();
    hasher.update(header.as_bytes());
    hasher.update(data);
    hasher.finalize().into()
}

/// Maps a `ChangeKind` to its single-character `--porcelain` status code.
fn change_code(kind: &ChangeKind) -> char {
    match kind {
        ChangeKind::Added => 'A',
        ChangeKind::Modified => 'M',
        ChangeKind::Deleted => 'D',
    }
}

/// Maps a `ChangeKind` to its human-readable status section label.
fn change_label(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Added => "new file:",
        ChangeKind::Modified => "modified:",
        ChangeKind::Deleted => "deleted:",
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use uuid::Uuid;

    use super::*;
    use crate::data_model::{
        author::Author, blob::Blob, commit::Commit, index::IndexEntry, object::GitObject,
        tree::Tree,
    };

    fn temp_repo_dir() -> PathBuf {
        std::env::temp_dir().join(format!("girst_status_test_{}", Uuid::new_v4()))
    }

    fn store_blob(db: &Database, content: &[u8]) -> [u8; 20] {
        let mut blob = Blob::new(content.to_vec());
        db.store(&mut blob).expect("store blob");
        let oid_hex = blob.oid().expect("blob oid");
        let raw = hex::decode(oid_hex).expect("hex decode");
        raw.try_into().expect("20 bytes")
    }

    #[test]
    fn three_way_status_matrix_and_racy_clean_and_collapse() {
        let root = temp_repo_dir();
        let girst = root.join(".girst");
        fs::create_dir_all(girst.join("objects")).expect("create objects");
        fs::create_dir_all(root.join("tracked_dir")).expect("create tracked_dir");
        fs::create_dir_all(root.join("untracked_dir/sub")).expect("create untracked_dir");

        let db = Database::new(girst.join("objects"));
        let refs = Refs::new(&girst);
        let ws = Workspace::new(&root);

        fs::write(root.join("clean.txt"), b"same").expect("write clean");
        fs::write(root.join("mod_head.txt"), b"v1").expect("write mod_head");
        fs::write(root.join("del_head.txt"), b"gone_from_idx").expect("write del_head");
        fs::write(root.join("tracked_dir/a.txt"), b"a").expect("write tracked_dir/a");

        let clean_oid = store_blob(&db, b"same");
        let mod_v1_oid = store_blob(&db, b"v1");
        let del_head_oid = store_blob(&db, b"gone_from_idx");
        let tracked_a_oid = store_blob(&db, b"a");

        let tree = Tree::build(
            vec![
                (
                    PathBuf::from("clean.txt"),
                    hex::encode(clean_oid),
                    "100644".to_string(),
                ),
                (
                    PathBuf::from("mod_head.txt"),
                    hex::encode(mod_v1_oid),
                    "100644".to_string(),
                ),
                (
                    PathBuf::from("del_head.txt"),
                    hex::encode(del_head_oid),
                    "100644".to_string(),
                ),
                (
                    PathBuf::from("tracked_dir/a.txt"),
                    hex::encode(tracked_a_oid),
                    "100644".to_string(),
                ),
            ],
            &db,
        )
        .expect("build tree");

        let author = Author::new("Ada", "ada@example.com", 1700000000, "+0000");
        let mut commit = Commit::new(tree.oid().expect("tree oid"), None, &author, "init");
        db.store(&mut commit).expect("store commit");
        refs.update_head(commit.oid().expect("commit oid"))
            .expect("update HEAD");

        // Stage: clean.txt (unchanged), mod_head.txt (v2), added.txt (new), tracked_dir/a.txt (unchanged); del_head.txt deleted.
        let mut index = Index::new(girst.join("index"));
        index.load_for_update().expect("lock index");

        let mut clean_entry = IndexEntry::from_metadata(
            "clean.txt",
            clean_oid,
            &ws.stat_file(Path::new("clean.txt")).expect("stat clean"),
        );
        // Simulate non-racy clean cache with mtime_sec != ctime_sec.
        clean_entry.ctime_sec = clean_entry.mtime_sec.saturating_sub(1);
        index.add(clean_entry);

        fs::write(root.join("mod_head.txt"), b"v2").expect("write mod_head v2");
        let mod_v2_oid = store_blob(&db, b"v2");
        let mut mod_entry = IndexEntry::from_metadata(
            "mod_head.txt",
            mod_v2_oid,
            &ws.stat_file(Path::new("mod_head.txt")).expect("stat mod"),
        );
        // Same-size edit with identical mtime/ctime triggers racily-clean re-hash.
        mod_entry.ctime_sec = mod_entry.mtime_sec;
        index.add(mod_entry);

        fs::write(root.join("added.txt"), b"new").expect("write added");
        let added_oid = store_blob(&db, b"new");
        let mut added_entry = IndexEntry::from_metadata(
            "added.txt",
            added_oid,
            &ws.stat_file(Path::new("added.txt")).expect("stat added"),
        );
        added_entry.ctime_sec = added_entry.mtime_sec.saturating_sub(1);
        index.add(added_entry);

        let mut tracked_a_entry = IndexEntry::from_metadata(
            "tracked_dir/a.txt",
            tracked_a_oid,
            &ws.stat_file(Path::new("tracked_dir/a.txt")).expect("stat a"),
        );
        tracked_a_entry.ctime_sec = tracked_a_entry.mtime_sec.saturating_sub(1);
        index.add(tracked_a_entry);

        index.write_updates().expect("write index");

        // Workspace mutations after staging:
        // 1) Same-size edit on racily-clean mod_head.txt ("v2" -> "v3") -> unstaged Modified.
        fs::write(root.join("mod_head.txt"), b"v3").expect("write mod_head v3");
        // 2) Delete tracked_dir/a.txt from disk -> unstaged Deleted.
        fs::remove_file(root.join("tracked_dir/a.txt")).expect("remove tracked_dir/a");
        // 3) Rewrite clean.txt with identical content ("same") and bump mtime/ctime -> still clean!
        fs::write(root.join("clean.txt"), b"same").expect("rewrite clean");
        // 4) Untracked files: partially-tracked dir vs fully-untracked dir.
        fs::write(root.join("tracked_dir/untracked.txt"), b"u").expect("write partial untracked");
        fs::write(root.join("untracked_dir/sub/x.txt"), b"x").expect("write collapsed 1");
        fs::write(root.join("untracked_dir/y.txt"), b"y").expect("write collapsed 2");

        let mut loaded_index = Index::new(girst.join("index"));
        loaded_index.load().expect("reload index");
        let report = StatusReport::collect(&ws, &db, &loaded_index, &refs).expect("collect status");

        assert_eq!(report.staged.get("added.txt"), Some(&ChangeKind::Added));
        assert_eq!(
            report.staged.get("mod_head.txt"),
            Some(&ChangeKind::Modified)
        );
        assert_eq!(
            report.staged.get("del_head.txt"),
            Some(&ChangeKind::Deleted)
        );
        assert!(!report.staged.contains_key("clean.txt"));

        assert_eq!(
            report.unstaged.get("mod_head.txt"),
            Some(&ChangeKind::Modified)
        );
        assert_eq!(
            report.unstaged.get("tracked_dir/a.txt"),
            Some(&ChangeKind::Deleted)
        );
        assert!(!report.unstaged.contains_key("clean.txt"));

        assert_eq!(
            report.untracked,
            vec![
                "del_head.txt".to_string(),
                "tracked_dir/untracked.txt".to_string(),
                "untracked_dir/".to_string()
            ]
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn porcelain_and_clean_human_formatting() {
        let clean = StatusReport::default();
        assert_eq!(clean.format_porcelain(), "");
        assert_eq!(
            clean.format_human(),
            "nothing to commit, working tree clean\n"
        );

        let mut report = StatusReport::default();
        report.staged.insert("a.txt".to_string(), ChangeKind::Added);
        report
            .staged
            .insert("b.txt".to_string(), ChangeKind::Modified);
        report
            .unstaged
            .insert("b.txt".to_string(), ChangeKind::Modified);
        report
            .unstaged
            .insert("c.txt".to_string(), ChangeKind::Deleted);
        report.untracked.push("u/".to_string());

        assert_eq!(
            report.format_porcelain(),
            "A  a.txt\nMM b.txt\n D c.txt\n?? u/\n"
        );
    }
}
