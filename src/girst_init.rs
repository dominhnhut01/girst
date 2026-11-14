//! Repository initialization for `.girst` metadata directories.

use std::{
    fs, io,
    path::PathBuf,
};

/// Initializes `.girst/objects` and `.girst/refs/heads` under `working_dir` (defaulting to `"."`).
///
/// Returns the created `.girst` directory path.
pub fn girst_init(working_dir: Option<&str>) -> io::Result<PathBuf> {
    let root = PathBuf::from(working_dir.unwrap_or("."));
    let girst_path = root.join(".girst");
    for dir in ["objects", "refs/heads"] {
        fs::create_dir_all(girst_path.join(dir))?;
    }
    Ok(girst_path)
}
