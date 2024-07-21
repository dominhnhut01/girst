use std::fs;
use std::io::Result;
use std::path::PathBuf;

pub fn girst_init(working_dir: &String) -> Result<()> {
    let girst_path = PathBuf::from(working_dir).join(".girst");
    for dir in ["objects", "refs"] {
        fs::create_dir_all(girst_path.join(dir))?;
    }
    Ok(())
}
