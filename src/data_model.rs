use hex;
use rand::Rng;
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::fs::File;
use std::io::{Error, Write};
use std::{fs, path::PathBuf};
use deflate::deflate_bytes;

pub struct Blob {
    id: Option<String>,
    data: String,
}
impl Blob {
    pub fn new(data: String) -> Self {
        Blob { id: None, data }
    }

    pub fn get_type(&self) -> &str {
        "blob"
    }

    pub fn to_string(&self) -> &str {
        &self.data
    }
}

pub struct Database {
    path: PathBuf,
}

impl Database {
    pub fn new(path: PathBuf) -> Self {
        Database { path }
    }

    pub fn store(&self, object: &mut Blob) -> Result<(), Error> {
        let string: String = String::from(object.to_string());
        let content: String = format!("#{} #{}\\0#{}", object.get_type(), string.len(), string);

        let mut hasher = Sha1::new();
        hasher.update(content.as_bytes());
        let hashed_content: String = hex::encode(hasher.finalize());
        object.id = Some(hashed_content.clone());
        self.write_object(&hashed_content, &content)?;
        Ok(())
    }
    fn write_object(&self, id: &String, content: &String) -> Result<(), std::io::Error> {
        let object_path = self.path.join(&id[0..3]).join(&id[4..]);
        let dirname = match object_path.parent() {
            Some(parent_path) => parent_path,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Parent directory not found",
                ))
            }
        };
        let temp_path = dirname.join(self.generate_random_string(14));

        fs::create_dir_all(dirname)?; // Ensure the directory exists

        let mut file = File::create(&temp_path)?;
        let compressed = deflate_bytes(content.as_bytes());
        file.write_all(&compressed)?;
        fs::rename(&temp_path, object_path)?;
        Ok(())
    }

    fn generate_random_string(&self, length: usize) -> String {
        const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ\
                                 abcdefghijklmnopqrstuvwxyz\
                                 0123456789";
        let mut rng = rand::thread_rng();

        let random_string: String = (0..length)
            .map(|_| {
                let idx = rng.gen_range(0..CHARSET.len());
                CHARSET[idx] as char
            })
            .collect();

        random_string
    }
}

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
