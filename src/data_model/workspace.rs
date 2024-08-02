use std::{collections::HashSet, fs, io::Error, path::PathBuf};

pub struct Workspace {
    ignore_dir: HashSet<PathBuf>,
    path: PathBuf,
}

impl Workspace {
    pub fn new(path: &String) -> Self {
        Workspace {
            ignore_dir: {
                let mut hashset: HashSet<PathBuf> = HashSet::new();
                hashset.insert(PathBuf::from("./.girst"));
                hashset.insert(PathBuf::from("./.git"));
                hashset.insert(PathBuf::from("./.gitignore"));
                hashset.insert(PathBuf::from("./target"));
                hashset
            },
            path: PathBuf::from(path),
        }
    }

    pub fn list_files(&self) -> Result<Vec<PathBuf>, std::io::Error> {
        let mut entries: Vec<PathBuf> = Vec::new();
        Workspace::list_files_recursive(&mut entries, &self.path, &self.ignore_dir)?;
        println!("{:?}", entries);
        Ok(entries)
    }

    fn list_files_recursive(
        entries: &mut Vec<PathBuf>,
        cur_dir: &PathBuf,
        ignore_dir: &HashSet<PathBuf>,
    ) -> Result<(), std::io::Error> {
        if ignore_dir.contains(cur_dir) {
            return Ok(());
        }
        if cur_dir.is_dir() {
            let read_dir = fs::read_dir(cur_dir)?;

            for entry in read_dir {
                let entry = entry?;
                let next_path = entry.path();
                Workspace::list_files_recursive(entries, &next_path, ignore_dir)?;
            }
        } else {
            entries.push(cur_dir.clone());
        }
        Ok(())
    }

    pub fn read_file(&self, path: PathBuf) -> Result<String, Error> {
        let full_path = self.path.join(path);
        let file_content = fs::read_to_string(&full_path)?;

        Ok(file_content)
    }
}
