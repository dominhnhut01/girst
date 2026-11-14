//! Safe tree-to-tree workspace migration and `HEAD`/index synchronization for `girst checkout`.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

use super::{
    database::{Database, ParsedObject},
    index::{hex_to_oid, Index, IndexEntry},
    refs::Refs,
    revision::resolve_revision,
    status::{hash_blob_oid, load_head_tree},
    workspace::Workspace,
};

/// Flattened repository tree mapping workspace-relative path to `(oid_hex, mode_str)`.
type TreeMap = BTreeMap<String, (String, String)>;

/// Checks out an existing branch or commit revision `target`, migrating workspace, index, and `HEAD`.
pub fn checkout(
    workspace: &Workspace,
    db: &Database,
    refs: &Refs,
    index: &mut Index,
    target: &str,
) -> io::Result<()> {
    let commit_oid = resolve_revision(db, refs, target)?;
    migrate_to_commit(workspace, db, refs, index, &commit_oid)?;
    refs.set_head(target, &commit_oid)
}

/// Creates `new_branch` (optionally at `start_rev`, defaulting to `HEAD`) and checks it out.
pub fn checkout_new_branch(
    workspace: &Workspace,
    db: &Database,
    refs: &Refs,
    index: &mut Index,
    new_branch: &str,
    start_rev: Option<&str>,
) -> io::Result<()> {
    refs.check_branch_available(new_branch)?;

    if start_rev.is_none() && refs.read_head()?.is_none() {
        return refs.set_symbolic_head(new_branch);
    }

    let rev = start_rev.unwrap_or("HEAD");
    let commit_oid = resolve_revision(db, refs, rev)?;
    migrate_to_commit(workspace, db, refs, index, &commit_oid)?;
    refs.create_branch(new_branch, &commit_oid)?;
    refs.set_head(new_branch, &commit_oid)
}

/// Validates conflicts and atomically migrates workspace and `.girst/index` from `HEAD` to `target_oid`.
fn migrate_to_commit(
    workspace: &Workspace,
    db: &Database,
    refs: &Refs,
    index: &mut Index,
    target_oid: &str,
) -> io::Result<()> {
    let old_tree = load_head_tree(db, refs)?;
    let new_tree = load_commit_tree(db, target_oid)?;

    index.load_for_update()?;
    if let Err(err) = validate_conflicts(workspace, &old_tree, &new_tree, index) {
        let _ = index.release_lock();
        return Err(err);
    }

    if let Err(err) = apply_migration(workspace, db, &old_tree, &new_tree, index) {
        let _ = index.release_lock();
        return Err(err);
    }

    index.write_updates()
}

/// Loads the flattened tree map for `commit_oid`.
fn load_commit_tree(db: &Database, commit_oid: &str) -> io::Result<TreeMap> {
    let ParsedObject::Commit(commit) = db.load(commit_oid)? else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("object {commit_oid} is not a commit"),
        ));
    };
    db.load_tree_map(commit.tree())
}

/// Runs all pre-flight conflict checks before any workspace, index, or ref mutation occurs.
fn validate_conflicts(
    workspace: &Workspace,
    old_tree: &TreeMap,
    new_tree: &TreeMap,
    index: &Index,
) -> io::Result<()> {
    let mut conflicts = BTreeSet::new();

    let all_paths: BTreeSet<&str> = old_tree
        .keys()
        .chain(new_tree.keys())
        .map(String::as_str)
        .collect();

    for path in all_paths {
        if old_tree.get(path) == new_tree.get(path) {
            continue;
        }
        if !index_matches_tree(old_tree.get(path), index.get(path)) {
            conflicts.insert(format!("{path} (staged changes would be overwritten)"));
            continue;
        }
        if let Some(idx_entry) = index.get(path) {
            if !workspace_matches_index(workspace, idx_entry)? {
                conflicts.insert(format!("{path} (local changes would be overwritten)"));
            }
        }
    }

    // Reject newly-staged entries (absent from HEAD) that collide file<->dir with target paths.
    for entry in index.entries() {
        if old_tree.contains_key(&entry.path) {
            continue;
        }
        if has_path_collision(&entry.path, new_tree) {
            conflicts.insert(format!(
                "{} (staged path collides with target tree)",
                entry.path
            ));
        }
    }

    for (path, target_entry) in new_tree {
        if old_tree.get(path) == Some(target_entry) {
            continue;
        }
        check_untracked_target_conflicts(workspace, old_tree, new_tree, index, path, &mut conflicts)?;
    }

    if conflicts.is_empty() {
        Ok(())
    } else {
        let details: Vec<String> = conflicts.into_iter().collect();
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "checkout aborted due to local conflicts:\n  {}",
                details.join("\n  ")
            ),
        ))
    }
}

/// Returns `true` if `idx_entry` matches `tree_entry` in presence, OID, and executable mode.
fn index_matches_tree(
    tree_entry: Option<&(String, String)>,
    idx_entry: Option<&IndexEntry>,
) -> bool {
    match (tree_entry, idx_entry) {
        (None, None) => true,
        (Some((oid, mode)), Some(entry)) => {
            hex::encode(entry.oid) == *oid && entry_mode_str(entry) == mode
        }
        _ => false,
    }
}

/// Returns `true` if the workspace file at `entry.path` matches `entry` on mode and content.
fn workspace_matches_index(workspace: &Workspace, entry: &IndexEntry) -> io::Result<bool> {
    let disk_path = workspace.root().join(&entry.path);
    let meta = match fs::symlink_metadata(&disk_path) {
        Ok(m) => m,
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => {
            return Ok(false);
        }
        Err(e) => return Err(e),
    };

    if !meta.is_file() {
        return Ok(false);
    }

    let ws_mode = if meta.mode() & 0o111 != 0 {
        0o100755
    } else {
        0o100644
    };
    if ws_mode != entry.mode {
        return Ok(false);
    }

    let stat_matches = meta.size() as u32 == entry.size
        && meta.mtime() as u32 == entry.mtime_sec
        && meta.mtime_nsec() as u32 == entry.mtime_nsec;
    // Re-hash when mtime_sec == ctime_sec to guard against same-second racily-clean edits.
    let racily_clean = entry.mtime_sec == entry.ctime_sec
        || (meta.mtime() as u32) == (meta.ctime() as u32);
    if stat_matches && !racily_clean {
        return Ok(true);
    }

    let bytes = workspace.read_file(Path::new(&entry.path))?;
    Ok(hash_blob_oid(&bytes) == entry.oid)
}

/// Checks whether `staged_path` has a file<->directory prefix collision with any path in `new_tree`.
fn has_path_collision(staged_path: &str, new_tree: &TreeMap) -> bool {
    for (idx, _) in staged_path.match_indices('/') {
        if new_tree.contains_key(&staged_path[..idx]) {
            return true;
        }
    }
    let prefix = format!("{staged_path}/");
    new_tree
        .range(prefix.clone()..)
        .take_while(|(k, _)| k.starts_with(&prefix))
        .next()
        .is_some()
}

/// Checks untracked overwrites, untracked ancestor files, and untracked files in replaced directories.
fn check_untracked_target_conflicts(
    workspace: &Workspace,
    old_tree: &TreeMap,
    new_tree: &TreeMap,
    index: &Index,
    target_path: &str,
    conflicts: &mut BTreeSet<String>,
) -> io::Result<()> {
    // 1. Ancestor file/symlink on disk blocking a target parent directory.
    for (idx, _) in target_path.match_indices('/') {
        let prefix = &target_path[..idx];
        let prefix_disk = workspace.root().join(prefix);
        if let Ok(meta) = fs::symlink_metadata(&prefix_disk) {
            if meta.is_file() || meta.file_type().is_symlink() {
                let is_deleted_tracked =
                    old_tree.contains_key(prefix) && !new_tree.contains_key(prefix);
                if !is_deleted_tracked {
                    conflicts.insert(format!("{prefix} (file blocks target directory)"));
                }
            }
        }
    }

    let disk_path = workspace.root().join(target_path);
    let meta = match fs::symlink_metadata(&disk_path) {
        Ok(m) => m,
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => {
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    if meta.is_dir() {
        // 2. Target file replaces a directory: fail if any descendant file/symlink is untracked.
        collect_untracked_in_dir(workspace.root(), &disk_path, old_tree, conflicts)?;
    } else if !old_tree.contains_key(target_path) && index.get(target_path).is_none() {
        // 3. Target file would overwrite an untracked file/symlink.
        conflicts.insert(format!("{target_path} (untracked file would be overwritten)"));
    }

    Ok(())
}

/// Recursively checks `dir` for any non-directory entry absent from `old_tree`.
fn collect_untracked_in_dir(
    root: &Path,
    dir: &Path,
    old_tree: &TreeMap,
    conflicts: &mut BTreeSet<String>,
) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            collect_untracked_in_dir(root, &path, old_tree, conflicts)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if !old_tree.contains_key(&rel_str) {
                conflicts.insert(format!(
                    "{rel_str} (untracked file inside replaced directory)"
                ));
            }
        }
    }
    Ok(())
}

/// Executes deletions (with empty parent pruning), blob writes/permissions, and index updates.
fn apply_migration(
    workspace: &Workspace,
    db: &Database,
    old_tree: &TreeMap,
    new_tree: &TreeMap,
    index: &mut Index,
) -> io::Result<()> {
    // Delete all removed paths and prune empty directories first to allow file<->dir transitions.
    for path in old_tree.keys() {
        if new_tree.contains_key(path) {
            continue;
        }
        let disk_path = workspace.root().join(path);
        match fs::remove_file(&disk_path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        prune_empty_parents(workspace.root(), disk_path.parent());
        index.remove(path);
    }

    for (path, (oid_hex, mode_str)) in new_tree {
        if old_tree.get(path).map(|(o, m)| (o, m)) == Some((oid_hex, mode_str)) {
            continue;
        }

        let ParsedObject::Blob(blob) = db.load(oid_hex)? else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("object {oid_hex} is not a blob"),
            ));
        };

        let disk_path = workspace.root().join(path);
        if let Ok(meta) = fs::symlink_metadata(&disk_path) {
            if meta.is_dir() {
                remove_empty_dir_tree(&disk_path)?;
            }
        }
        if let Some(parent) = disk_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&disk_path, blob.data())?;
        let unix_mode = if mode_str == "100755" { 0o755 } else { 0o644 };
        fs::set_permissions(&disk_path, fs::Permissions::from_mode(unix_mode))?;

        let oid = hex_to_oid(oid_hex)?;
        let meta = workspace.stat_file(Path::new(path))?;
        index.add(IndexEntry::from_metadata(path, oid, &meta));
    }

    Ok(())
}

/// Recursively removes an empty directory tree bottom-up.
fn remove_empty_dir_tree(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if fs::symlink_metadata(&path)?.is_dir() {
            remove_empty_dir_tree(&path)?;
        }
    }
    fs::remove_dir(dir)
}

/// Removes empty directories upward from `start` up to (excluding) `root`.
fn prune_empty_parents(root: &Path, mut cur: Option<&Path>) {
    while let Some(dir) = cur {
        if dir == root {
            break;
        }
        if fs::remove_dir(dir).is_err() {
            break;
        }
        cur = dir.parent();
    }
}

/// Formats an [`IndexEntry`] Unix mode as `"100755"` or `"100644"`.
fn entry_mode_str(entry: &IndexEntry) -> &'static str {
    if entry.mode & 0o111 != 0 {
        "100755"
    } else {
        "100644"
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use uuid::Uuid;

    use super::*;
    use crate::data_model::{
        author::Author, blob::Blob, commit::Commit, object::GitObject, tree::Tree,
    };

    fn temp_repo_dir() -> PathBuf {
        std::env::temp_dir().join(format!("girst_checkout_test_{}", Uuid::new_v4()))
    }

    fn store_blob(db: &Database, data: &[u8]) -> String {
        let mut blob = Blob::new(data.to_vec());
        db.store(&mut blob).unwrap();
        blob.oid().unwrap().to_string()
    }

    fn commit_tree_state(
        db: &Database,
        parent: Option<String>,
        items: Vec<(&str, &str, &str)>,
        msg: &str,
    ) -> String {
        let tree_items = items
            .into_iter()
            .map(|(p, oid, mode)| (PathBuf::from(p), oid.to_string(), mode.to_string()))
            .collect();
        let tree = Tree::build(tree_items, db).unwrap();
        let author = Author::new("Ada", "ada@example.com", 1700000000, "+0000");
        let mut commit = Commit::new(tree.oid().unwrap(), parent, &author, msg);
        db.store(&mut commit).unwrap();
        commit.oid().unwrap().to_string()
    }

    fn stage_from_disk(workspace: &Workspace, db: &Database, index: &mut Index, paths: &[&str]) {
        index.load_for_update().unwrap();
        for &p in paths {
            let data = workspace.read_file(Path::new(p)).unwrap();
            let oid_hex = store_blob(db, &data);
            let oid = hex_to_oid(&oid_hex).unwrap();
            let meta = workspace.stat_file(Path::new(p)).unwrap();
            index.add(IndexEntry::from_metadata(p, oid, &meta));
        }
        index.write_updates().unwrap();
    }

    #[test]
    fn preflight_conflict_refusal_leaves_workspace_index_and_refs_untouched() {
        let root = temp_repo_dir();
        let girst = root.join(".girst");
        fs::create_dir_all(girst.join("objects")).unwrap();
        fs::create_dir_all(girst.join("refs/heads")).unwrap();
        fs::write(girst.join("HEAD"), b"ref: refs/heads/main\n").unwrap();

        let db = Database::new(girst.join("objects"));
        let refs = Refs::new(&girst);
        let ws = Workspace::new(&root);

        let b_v1 = store_blob(&db, b"v1\n");
        let b_v2 = store_blob(&db, b"v2\n");
        let c1 = commit_tree_state(&db, None, vec![("a.txt", &b_v1, "100644")], "c1\n");
        let c2 = commit_tree_state(
            &db,
            Some(c1.clone()),
            vec![
                ("a.txt", &b_v2, "100644"),
                ("untracked.txt", &b_v2, "100644"),
                ("block/sub.txt", &b_v2, "100644"),
                ("dir_to_file", &b_v2, "100644"),
            ],
            "c2\n",
        );

        refs.update_head(&c1).unwrap();
        refs.create_branch("other", &c2).unwrap();
        fs::write(root.join("a.txt"), b"v1\n").unwrap();
        let mut index = Index::new(girst.join("index"));
        stage_from_disk(&ws, &db, &mut index, &["a.txt"]);
        let index_snapshot = fs::read(girst.join("index")).unwrap();

        // 1) Dirty workspace edit on affected file a.txt.
        fs::write(root.join("a.txt"), b"dirty\n").unwrap();
        assert_eq!(
            checkout(&ws, &db, &refs, &mut index, "other")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(fs::read(root.join("a.txt")).unwrap(), b"dirty\n");
        assert_eq!(fs::read(girst.join("index")).unwrap(), index_snapshot);
        assert_eq!(refs.current_branch().unwrap(), Some("main".to_string()));
        assert!(!girst.join("index.lock").exists());

        // Restore clean a.txt, then test 2) staged edit on a.txt.
        fs::write(root.join("a.txt"), b"v1\n").unwrap();
        fs::write(root.join("a.txt"), b"staged\n").unwrap();
        stage_from_disk(&ws, &db, &mut index, &["a.txt"]);
        let staged_snapshot = fs::read(girst.join("index")).unwrap();
        assert!(checkout(&ws, &db, &refs, &mut index, "other").is_err());
        assert_eq!(fs::read(girst.join("index")).unwrap(), staged_snapshot);
        assert_eq!(refs.read_head().unwrap(), Some(c1.clone()));

        // Reset index & disk to clean c1, then test 3) untracked overwrite, 4) untracked ancestor file,
        // and 5) untracked file inside directory replaced by target file.
        fs::write(root.join("a.txt"), b"v1\n").unwrap();
        stage_from_disk(&ws, &db, &mut index, &["a.txt"]);
        fs::write(root.join("untracked.txt"), b"keep\n").unwrap();
        fs::write(root.join("block"), b"blocker\n").unwrap();
        fs::create_dir_all(root.join("dir_to_file")).unwrap();
        fs::write(root.join("dir_to_file/keep.txt"), b"keep\n").unwrap();

        let err = checkout(&ws, &db, &refs, &mut index, "other")
            .unwrap_err()
            .to_string();
        assert!(err.contains("untracked.txt"));
        assert!(err.contains("block"));
        assert!(err.contains("dir_to_file/keep.txt"));
        assert_eq!(fs::read(root.join("untracked.txt")).unwrap(), b"keep\n");
        assert_eq!(fs::read(root.join("block")).unwrap(), b"blocker\n");
        assert!(root.join("dir_to_file/keep.txt").is_file());
        assert_eq!(refs.current_branch().unwrap(), Some("main".to_string()));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clean_migration_modes_file_dir_transitions_pruning_index_and_head() {
        let root = temp_repo_dir();
        let girst = root.join(".girst");
        fs::create_dir_all(girst.join("objects")).unwrap();
        fs::create_dir_all(girst.join("refs/heads")).unwrap();
        fs::write(girst.join("HEAD"), b"ref: refs/heads/main\n").unwrap();

        let db = Database::new(girst.join("objects"));
        let refs = Refs::new(&girst);
        let ws = Workspace::new(&root);

        let b_script = store_blob(&db, b"#!/bin/sh\necho hi\n");
        let b_nested = store_blob(&db, b"nested\n");
        let b_file = store_blob(&db, b"now a file\n");

        let c1 = commit_tree_state(
            &db,
            None,
            vec![
                ("bin/run.sh", &b_script, "100755"),
                ("old/deep/gone.txt", &b_nested, "100644"),
                ("morph", &b_file, "100644"),
            ],
            "c1\n",
        );
        let c2 = commit_tree_state(
            &db,
            Some(c1.clone()),
            vec![
                ("bin/run.sh", &b_script, "100644"),
                ("morph/inner.txt", &b_nested, "100755"),
            ],
            "c2\n",
        );

        refs.update_head(&c1).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("old/deep")).unwrap();
        fs::write(root.join("bin/run.sh"), b"#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(root.join("bin/run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(root.join("old/deep/gone.txt"), b"nested\n").unwrap();
        fs::write(root.join("morph"), b"now a file\n").unwrap();

        let mut index = Index::new(girst.join("index"));
        stage_from_disk(
            &ws,
            &db,
            &mut index,
            &["bin/run.sh", "old/deep/gone.txt", "morph"],
        );

        // checkout -b topic c2: transitions file->dir (morph), mode 755->644, and prunes old/deep.
        checkout_new_branch(&ws, &db, &refs, &mut index, "topic", Some(&c2)).unwrap();
        assert_eq!(refs.current_branch().unwrap(), Some("topic".to_string()));
        assert_eq!(ws.stat_mode(Path::new("bin/run.sh")).unwrap(), "100644");
        assert_eq!(ws.stat_mode(Path::new("morph/inner.txt")).unwrap(), "100755");
        assert!(!root.join("old").exists());
        assert!(index.get("old/deep/gone.txt").is_none());
        assert!(index.get("morph/inner.txt").is_some());

        // Switch back to main (dir->file morph, mode 644->755, recreates old/deep/gone.txt).
        checkout(&ws, &db, &refs, &mut index, "main").unwrap();
        assert_eq!(refs.current_branch().unwrap(), Some("main".to_string()));
        assert_eq!(ws.stat_mode(Path::new("bin/run.sh")).unwrap(), "100755");
        assert!(root.join("morph").is_file());
        assert_eq!(fs::read(root.join("old/deep/gone.txt")).unwrap(), b"nested\n");

        // Detached checkout via revision expression.
        checkout(&ws, &db, &refs, &mut index, "topic~0").unwrap();
        assert_eq!(refs.current_branch().unwrap(), None);
        assert_eq!(refs.read_head().unwrap(), Some(c2));

        let _ = fs::remove_dir_all(&root);
    }
}
