//! Repository initialization for `.girst` metadata directories and initial `HEAD`.

use std::{fs, io, path::PathBuf};

use crate::data_model::lockfile::Lockfile;

/// Initializes `.girst/objects`, `.girst/refs/heads`, and symbolic `HEAD` under `working_dir` (defaulting to `"."`).
///
/// Returns the created `.girst` directory path.
pub fn girst_init(working_dir: Option<&str>) -> io::Result<PathBuf> {
    let root = PathBuf::from(working_dir.unwrap_or("."));
    let girst_path = root.join(".girst");
    for dir in ["objects", "refs/heads"] {
        fs::create_dir_all(girst_path.join(dir))?;
    }
    let head_path = girst_path.join("HEAD");
    if !head_path.exists() {
        let mut lock = Lockfile::hold_for_update(&head_path)?;
        lock.write(b"ref: refs/heads/main\n")?;
        lock.commit()?;
    }
    Ok(girst_path)
}
