use std::{fs::{self, File}, io::{Error, Write}, path::PathBuf};

use deflate::deflate_bytes;
use rand::Rng;
use sha1::{Digest, Sha1};

use super::blob::Blob;

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
        object.set_id(hashed_content.clone());
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