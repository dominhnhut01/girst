//! Atomic file locking via exclusive `<path>.lock` creation and rename-on-commit.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

/// Exclusive lockfile guard that writes to `<path>.lock` and atomically renames on commit.
///
/// If dropped without calling [`Lockfile::commit`], the temporary `.lock` file is removed.
pub struct Lockfile {
    file_path: PathBuf,
    lock_path: PathBuf,
    file: Option<File>,
    committed: bool,
}

impl Lockfile {
    /// Acquires an exclusive lock at `<path>.lock`, failing with `AlreadyExists` if held.
    pub fn hold_for_update(path: impl AsRef<Path>) -> io::Result<Self> {
        let file_path = path.as_ref().to_path_buf();
        let lock_path = lock_path_for(&file_path);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)?;

        Ok(Lockfile {
            file_path,
            lock_path,
            file: Some(file),
            committed: false,
        })
    }

    /// Writes `bytes` to the underlying `.lock` file.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("lockfile handle is already closed"))?;
        file.write_all(bytes)
    }

    /// Closes the `.lock` file and atomically renames it over the target path.
    ///
    /// Removes the `.lock` file if the rename fails.
    pub fn commit(mut self) -> io::Result<()> {
        drop(self.file.take());
        if let Err(err) = fs::rename(&self.lock_path, &self.file_path) {
            let _ = fs::remove_file(&self.lock_path);
            self.committed = true;
            return Err(err);
        }
        self.committed = true;
        Ok(())
    }

    /// Closes and removes the `.lock` file without updating the target path.
    pub fn rollback(mut self) -> io::Result<()> {
        drop(self.file.take());
        self.committed = true;
        fs::remove_file(&self.lock_path)
    }
}

impl Drop for Lockfile {
    fn drop(&mut self) {
        if !self.committed {
            drop(self.file.take());
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}

/// Appends `.lock` directly to the full OS path so filenames with or without extensions work identically.
fn lock_path_for(path: &Path) -> PathBuf {
    let mut os_str = path.as_os_str().to_os_string();
    os_str.push(".lock");
    PathBuf::from(os_str)
}
