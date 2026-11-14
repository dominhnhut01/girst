//! Git reference (`HEAD`) reader and atomic lockfile updater.

use std::{fs, io, path::PathBuf};

use super::lockfile::Lockfile;

/// Manages reading and atomically updating `.girst/HEAD`.
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

    /// Reads the current OID stored in `HEAD`, returning `Ok(None)` if `HEAD` is missing or empty.
    pub fn read_head(&self) -> io::Result<Option<String>> {
        let head_path = self.girst_path.join("HEAD");
        match fs::read_to_string(&head_path) {
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

    /// Atomically updates `HEAD` to `oid` using an exclusive `HEAD.lock` file.
    ///
    /// Returns `AlreadyExists` if `HEAD.lock` is already present.
    pub fn update_head(&self, oid: &str) -> io::Result<()> {
        let head_path = self.girst_path.join("HEAD");
        let mut lock = Lockfile::hold_for_update(&head_path)?;
        lock.write(format!("{}\n", oid).as_bytes())?;
        lock.commit()
    }
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
}
