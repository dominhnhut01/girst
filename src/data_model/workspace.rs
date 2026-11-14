//! Workspace file discovery, raw byte reading, and Unix file mode inspection.

use std::{
    collections::HashSet,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// Represents a working tree directory and its ignored subdirectories.
pub struct Workspace {
    ignore_dir: HashSet<PathBuf>,
    path: PathBuf,
}

impl Workspace {
    /// Creates a `Workspace` rooted at `path`, ignoring `.git`, `.girst`, and `target` under it.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut ignore_dir = HashSet::new();
        ignore_dir.insert(path.join(".girst"));
        ignore_dir.insert(path.join(".git"));
        ignore_dir.insert(path.join("target"));

        Workspace { ignore_dir, path }
    }

    /// Recursively lists all non-ignored files as paths relative to the workspace root.
    pub fn list_files(&self) -> io::Result<Vec<PathBuf>> {
        let mut entries = Vec::new();
        self.list_files_recursive(&mut entries, &self.path)?;
        Ok(entries)
    }

    /// Walks `cur_dir` and appends workspace-relative file paths to `entries`, skipping ignored dirs.
    fn list_files_recursive(&self, entries: &mut Vec<PathBuf>, cur_dir: &Path) -> io::Result<()> {
        if self.ignore_dir.contains(cur_dir) {
            return Ok(());
        }
        if cur_dir.is_dir() {
            for entry in fs::read_dir(cur_dir)? {
                let entry = entry?;
                self.list_files_recursive(entries, &entry.path())?;
            }
        } else {
            let rel_path = cur_dir
                .strip_prefix(&self.path)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            entries.push(rel_path.to_path_buf());
        }
        Ok(())
    }

    /// Reads the raw bytes of `path` (interpreted relative to the workspace root).
    pub fn read_file(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(self.path.join(path))
    }

    /// Returns the `fs::Metadata` for `path` (interpreted relative to the workspace root).
    pub fn stat_file(&self, path: &Path) -> io::Result<fs::Metadata> {
        fs::metadata(self.path.join(path))
    }

    /// Expands `target` (`.`/root, file, or directory) into normalized `/` workspace-relative paths.
    ///
    /// Rejects missing paths, `.girst`/`.git` metadata paths, and paths outside the workspace root.
    pub fn expand_path(&self, target: &Path) -> io::Result<Vec<String>> {
        let canon_root = fs::canonicalize(&self.path)?;
        let candidate = if target.is_absolute() {
            target.to_path_buf()
        } else {
            self.path.join(target)
        };
        let canon_target = fs::canonicalize(&candidate)?;

        let rel = canon_target.strip_prefix(&canon_root).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("path '{}' is outside workspace", target.display()),
            )
        })?;

        if let Some(first) = rel.components().next() {
            let name = first.as_os_str();
            if name == ".girst" || name == ".git" {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("cannot add repository metadata path '{}'", target.display()),
                ));
            }
        }

        let mut results = Vec::new();
        self.expand_canonical_recursive(&canon_root, &canon_target, &mut results)?;
        Ok(results)
    }

    /// Recursively collects normalized `/` workspace-relative file paths under `canon_cur`.
    fn expand_canonical_recursive(
        &self,
        canon_root: &Path,
        canon_cur: &Path,
        out: &mut Vec<String>,
    ) -> io::Result<()> {
        if canon_cur != canon_root {
            if let Some(name) = canon_cur.file_name() {
                if canon_cur.is_dir() && (name == ".girst" || name == ".git" || name == "target") {
                    return Ok(());
                }
            }
        }

        if canon_cur.is_dir() {
            for entry in fs::read_dir(canon_cur)? {
                let entry = entry?;
                self.expand_canonical_recursive(canon_root, &entry.path(), out)?;
            }
        } else {
            let rel = canon_cur
                .strip_prefix(canon_root)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            out.push(normalize_rel_path(rel)?);
        }
        Ok(())
    }

    /// Returns the Git file mode (`"100755"` if executable on Unix, `"100644"` otherwise) for `path`.
    pub fn stat_mode(&self, path: &Path) -> io::Result<&'static str> {
        let metadata = self.stat_file(path)?;
        if metadata.permissions().mode() & 0o111 != 0 {
            Ok("100755")
        } else {
            Ok("100644")
        }
    }
}

/// Formats a workspace-relative `Path` as a normalized forward-slash UTF-8 string.
fn normalize_rel_path(rel: &Path) -> io::Result<String> {
    let mut parts = Vec::new();
    for component in rel.components() {
        if let std::path::Component::Normal(seg) = component {
            let s = seg.to_str().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("non-utf8 path component in '{}'", rel.display()),
                )
            })?;
            parts.push(s);
        }
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn temp_workspace_dir() -> PathBuf {
        std::env::temp_dir().join(format!("girst_ws_test_{}", Uuid::new_v4()))
    }

    #[test]
    fn list_files_returns_relative_paths_and_skips_ignored_dirs() {
        let root = temp_workspace_dir();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join(".girst")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();

        fs::write(root.join("a.txt"), b"hello").unwrap();
        let raw_bytes = vec![0xff, 0x00, 0xfe, 0x42];
        fs::write(root.join("sub/b.bin"), &raw_bytes).unwrap();
        fs::write(root.join(".git/config"), b"ignored").unwrap();
        fs::write(root.join(".girst/HEAD"), b"ignored").unwrap();
        fs::write(root.join("target/debug/girst"), b"ignored").unwrap();

        let ws = Workspace::new(&root);
        let mut files = ws.list_files().expect("list_files should succeed");
        files.sort();

        assert_eq!(
            files,
            vec![PathBuf::from("a.txt"), PathBuf::from("sub/b.bin")]
        );
        assert_eq!(
            ws.read_file(Path::new("sub/b.bin")).expect("read_file"),
            raw_bytes
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn stat_mode_distinguishes_regular_and_executable() {
        let root = temp_workspace_dir();
        fs::create_dir_all(&root).unwrap();

        let regular = Path::new("plain.txt");
        let exec = Path::new("run.sh");
        fs::write(root.join(regular), b"hi").unwrap();
        fs::write(root.join(exec), b"#!/bin/sh\n").unwrap();

        fs::set_permissions(root.join(regular), fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(root.join(exec), fs::Permissions::from_mode(0o755)).unwrap();

        let ws = Workspace::new(&root);
        assert_eq!(ws.stat_mode(regular).expect("stat regular"), "100644");
        assert_eq!(ws.stat_mode(exec).expect("stat exec"), "100755");

        let _ = fs::remove_dir_all(&root);
    }
}
