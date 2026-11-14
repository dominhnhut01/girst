//! Git reference (`HEAD` and `refs/heads/*`) reader and atomic lockfile updater.

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

use super::lockfile::Lockfile;

/// Parsed target of `.girst/HEAD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadRef {
    /// Symbolic reference pointing to a relative ref path under `.girst` (e.g. `"refs/heads/main"`).
    Symbolic(String),
    /// Detached `HEAD` holding a raw object ID.
    Detached(String),
}

/// Manages reading and atomically updating `.girst/HEAD` and local branch references.
pub struct Refs {
    girst_path: PathBuf,
}

impl Refs {
    /// Creates a `Refs` manager rooted at the `.girst` metadata directory.
    pub fn new(girst_path: impl Into<PathBuf>) -> Self {
        Refs {
            girst_path: girst_path.into(),
        }
    }

    /// Reads and parses `.girst/HEAD` into a [`HeadRef`], returning `Ok(None)` if missing or empty.
    ///
    /// Rejects symbolic targets that escape `.girst` via `..`, absolute paths, or backslashes.
    pub fn read_head_ref(&self) -> io::Result<Option<HeadRef>> {
        let head_path = self.girst_path.join("HEAD");
        let content = match fs::read_to_string(&head_path) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };

        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        if let Some(rest) = trimmed.strip_prefix("ref:") {
            let rel_path = rest.trim();
            validate_rel_ref_path(rel_path)?;
            Ok(Some(HeadRef::Symbolic(rel_path.to_string())))
        } else {
            Ok(Some(HeadRef::Detached(trimmed.to_string())))
        }
    }

    /// Reads the current commit OID referenced by `HEAD`, following symbolic refs if present.
    ///
    /// Returns `Ok(None)` when `HEAD` is missing/empty or points to an unborn branch.
    pub fn read_head(&self) -> io::Result<Option<String>> {
        match self.read_head_ref()? {
            None => Ok(None),
            Some(HeadRef::Detached(oid)) => Ok(Some(oid)),
            Some(HeadRef::Symbolic(rel_path)) => read_trimmed_file(&self.girst_path.join(rel_path)),
        }
    }

    /// Atomically updates `HEAD` (or its target branch ref when symbolic) to `oid`.
    ///
    /// Returns `AlreadyExists` if the target `.lock` file is already held.
    pub fn update_head(&self, oid: &str) -> io::Result<()> {
        let target_path = match self.read_head_ref()? {
            Some(HeadRef::Symbolic(rel_path)) => {
                let path = self.girst_path.join(rel_path);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                path
            }
            Some(HeadRef::Detached(_)) | None => self.girst_path.join("HEAD"),
        };
        write_ref_atomically(&target_path, oid)
    }

    /// Returns the active local branch name when `HEAD` symbolically points to `refs/heads/<name>`.
    pub fn current_branch(&self) -> io::Result<Option<String>> {
        match self.read_head_ref()? {
            Some(HeadRef::Symbolic(rel_path)) => {
                Ok(rel_path.strip_prefix("refs/heads/").map(str::to_string))
            }
            _ => Ok(None),
        }
    }

    /// Reads the OID stored for local branch `name`, returning `Ok(None)` if invalid or absent.
    pub fn read_branch(&self, name: &str) -> io::Result<Option<String>> {
        if validate_branch_name(name).is_err() {
            return Ok(None);
        }
        let branch_path = self.heads_dir().join(name);
        if !branch_path.is_file() {
            return Ok(None);
        }
        read_trimmed_file(&branch_path)
    }

    /// Creates a new local branch `name` pointing to `oid`.
    ///
    /// Fails with `InvalidInput` for unsafe branch names and `AlreadyExists` if `name` already
    /// exists or conflicts with a file/directory prefix under `refs/heads`.
    pub fn create_branch(&self, name: &str, oid: &str) -> io::Result<()> {
        validate_branch_name(name)?;
        let heads_dir = self.heads_dir();
        let target = heads_dir.join(name);

        if target.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("a branch named '{name}' already exists"),
            ));
        }
        if target.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("cannot create branch '{name}': ref path conflicts with directory"),
            ));
        }

        // Guard against creating `a/b` when `a` already exists as a branch file.
        let mut prefix = heads_dir.clone();
        let parts: Vec<&str> = name.split('/').collect();
        for part in &parts[..parts.len().saturating_sub(1)] {
            prefix.push(part);
            if prefix.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("cannot create branch '{name}': parent ref already exists as file"),
                ));
            }
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        write_ref_atomically(&target, oid)
    }

    /// Recursively lists all local branches under `.girst/refs/heads`, sorted lexicographically.
    ///
    /// Each entry pairs the branch name with a boolean indicating whether it is the active branch.
    pub fn list_branches(&self) -> io::Result<Vec<(String, bool)>> {
        let heads_dir = self.heads_dir();
        if !heads_dir.is_dir() {
            return Ok(Vec::new());
        }

        let mut names = Vec::new();
        collect_branch_names(&heads_dir, "", &mut names)?;
        names.sort();

        let active = self.current_branch()?;
        Ok(names
            .into_iter()
            .map(|name| {
                let is_active = active.as_deref() == Some(name.as_str());
                (name, is_active)
            })
            .collect())
    }

    /// Deletes local branch `name`, prunes empty parent directories under `refs/heads`, and returns its OID.
    ///
    /// Refuses with `InvalidInput` if `name` is the currently checked-out branch, or `NotFound` if missing.
    pub fn delete_branch(&self, name: &str) -> io::Result<String> {
        validate_branch_name(name)?;
        if self.current_branch()?.as_deref() == Some(name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cannot delete branch '{name}' checked out at current HEAD"),
            ));
        }

        let heads_dir = self.heads_dir();
        let target = heads_dir.join(name);
        if !target.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("branch '{name}' not found"),
            ));
        }

        let oid = read_trimmed_file(&target)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, format!("empty ref '{name}'"))
        })?;
        fs::remove_file(&target)?;

        let mut cur = target.parent();
        while let Some(dir) = cur {
            if dir == heads_dir {
                break;
            }
            if fs::remove_dir(dir).is_err() {
                break;
            }
            cur = dir.parent();
        }
        Ok(oid)
    }

    /// Updates `.girst/HEAD` to symbolically point to `target` if it is an existing branch, else detaches at `commit_oid`.
    pub fn set_head(&self, target: &str, commit_oid: &str) -> io::Result<()> {
        let head_path = self.girst_path.join("HEAD");
        if validate_branch_name(target).is_ok() && self.heads_dir().join(target).is_file() {
            write_ref_atomically(&head_path, &format!("ref: refs/heads/{target}"))
        } else {
            write_ref_atomically(&head_path, commit_oid)
        }
    }

    /// Returns the path to `.girst/refs/heads`.
    fn heads_dir(&self) -> PathBuf {
        self.girst_path.join("refs/heads")
    }
}

/// Validates that `name` is a safe Git branch name.
pub fn validate_branch_name(name: &str) -> io::Result<()> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid branch name: '{name}'"),
        )
    };

    if name.is_empty()
        || name == "HEAD"
        || name == "@"
        || name.starts_with('/')
        || name.ends_with('/')
        || name.starts_with('.')
        || name.ends_with('.')
        || name.starts_with('-')
        || name.ends_with(".lock")
        || name.contains("..")
        || name.contains("//")
        || name.contains("@{")
        || name.contains('\\')
    {
        return Err(invalid());
    }

    if name.chars().any(|c| {
        c.is_control() || matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[')
    }) {
        return Err(invalid());
    }

    for component in name.split('/') {
        if component.is_empty() || component.starts_with('.') || component.ends_with(".lock") {
            return Err(invalid());
        }
    }

    Ok(())
}

/// Validates that a symbolic `HEAD` target is a safe relative path within `.girst`.
fn validate_rel_ref_path(rel_path: &str) -> io::Result<()> {
    if rel_path.is_empty() || rel_path.contains('\\') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid symbolic HEAD target",
        ));
    }
    let path = Path::new(rel_path);
    if path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "symbolic HEAD target must be relative",
        ));
    }
    for comp in path.components() {
        if !matches!(comp, Component::Normal(_)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsafe symbolic HEAD path component",
            ));
        }
    }
    Ok(())
}

/// Reads a ref file and returns its trimmed contents, or `Ok(None)` if missing or empty.
fn read_trimmed_file(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Atomically writes `<value>\n` to `path` via [`Lockfile`].
fn write_ref_atomically(path: &Path, value: &str) -> io::Result<()> {
    let mut lock = Lockfile::hold_for_update(path)?;
    lock.write(format!("{value}\n").as_bytes())?;
    lock.commit()
}

/// Recursively collects branch names under `dir` into `out`, skipping `.lock` files.
fn collect_branch_names(dir: &Path, prefix: &str, out: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if name.ends_with(".lock") {
            continue;
        }
        let rel = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        if path.is_dir() {
            collect_branch_names(&path, &rel, out)?;
        } else if path.is_file() {
            out.push(rel);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn temp_girst_dir() -> PathBuf {
        std::env::temp_dir().join(format!("girst_refs_test_{}", Uuid::new_v4()))
    }

    #[test]
    fn refs_atomic_head_update_and_lock_conflict() {
        let girst_dir = temp_girst_dir();
        fs::create_dir_all(&girst_dir).unwrap();
        let refs = Refs::new(&girst_dir);

        assert_eq!(refs.read_head().expect("missing HEAD"), None);

        let oid1 = "1111111111111111111111111111111111111111";
        refs.update_head(oid1).expect("initial update_head");
        assert!(!girst_dir.join("HEAD.lock").exists());
        assert_eq!(refs.read_head().expect("read HEAD"), Some(oid1.to_string()));

        fs::write(girst_dir.join("HEAD.lock"), b"busy\n").unwrap();
        let err = refs
            .update_head("2222222222222222222222222222222222222222")
            .expect_err("lock conflict should fail");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            refs.read_head().expect("HEAD unchanged"),
            Some(oid1.to_string())
        );

        let _ = fs::remove_dir_all(&girst_dir);
    }

    #[test]
    fn symbolic_head_and_branch_lifecycle() {
        let girst_dir = temp_girst_dir();
        fs::create_dir_all(girst_dir.join("refs/heads")).unwrap();
        fs::write(girst_dir.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        let refs = Refs::new(&girst_dir);

        // Unborn symbolic HEAD reports current_branch("main") but read_head() == None.
        assert_eq!(refs.current_branch().unwrap(), Some("main".to_string()));
        assert_eq!(refs.read_head().unwrap(), None);

        // Root and child commits advance refs/heads/main while keeping HEAD symbolic.
        let oid1 = "1111111111111111111111111111111111111111";
        let oid2 = "2222222222222222222222222222222222222222";
        refs.update_head(oid1).unwrap();
        assert_eq!(refs.read_head().unwrap(), Some(oid1.to_string()));
        refs.update_head(oid2).unwrap();
        assert_eq!(refs.read_head().unwrap(), Some(oid2.to_string()));
        assert_eq!(
            refs.read_head_ref().unwrap(),
            Some(HeadRef::Symbolic("refs/heads/main".to_string()))
        );

        // Nested and flat branch creation, conflict detection, and sorted listing.
        refs.create_branch("feat/topic", oid1).unwrap();
        refs.create_branch("alpha", oid2).unwrap();
        assert_eq!(
            refs.create_branch("main/sub", oid1).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            refs.create_branch("feat", oid1).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            refs.list_branches().unwrap(),
            vec![
                ("alpha".to_string(), false),
                ("feat/topic".to_string(), false),
                ("main".to_string(), true),
            ]
        );

        // Invalid branch names and deleting active branch are rejected.
        for bad in ["", "HEAD", "@", "-dash", "a..b", "foo.lock", "a/.hidden", "a b"] {
            assert_eq!(
                refs.create_branch(bad, oid1).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        assert_eq!(
            refs.delete_branch("main").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );

        // Deleting nested branch removes ref and prunes empty parent directory.
        assert_eq!(refs.delete_branch("feat/topic").unwrap(), oid1);
        assert!(!girst_dir.join("refs/heads/feat").exists());

        // set_head switches symbolically to an existing branch, or detaches for raw OID.
        refs.set_head("alpha", oid2).unwrap();
        assert_eq!(refs.current_branch().unwrap(), Some("alpha".to_string()));
        refs.set_head(oid1, oid1).unwrap();
        assert_eq!(refs.current_branch().unwrap(), None);
        assert_eq!(
            refs.read_head_ref().unwrap(),
            Some(HeadRef::Detached(oid1.to_string()))
        );

        let _ = fs::remove_dir_all(&girst_dir);
    }
}
