use hex;
use sha1::{Digest, Sha1};
use std::{fs, io, path::PathBuf};
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

    pub fn store(&self, object: &mut Blob) {
        let string: &str = object.to_string();
        let content: String = format!("{} {}\\0{}", object.get_type(), string.len(), string);

        let mut hasher = Sha1::new();
        hasher.update(content.as_bytes());
        let hashed_content: String = hex::encode(hasher.finalize());
        object.id = Some(hashed_content);
        match &object.id {
            Some(id) => self.write_object(&id, &content),
            None => panic!("Object Id is None."),
        }
    }
    pub fn write_object(&self, id: &String, content: &String) {
        let object_path = self.path.join(id);
    }
}

pub struct Workspace {
    ignore: Vec<PathBuf>,
    path: PathBuf,
}

impl Workspace {
    pub fn new(path: &String) -> Self {
        Workspace {
            ignore: vec![
                PathBuf::from("./"),
                PathBuf::from("../"),
                PathBuf::from("./.girst"),
                PathBuf::from("./.git"),
                PathBuf::from("./.gitignore"),
            ],
            path: PathBuf::from(path),
        }
    }

    pub fn list_files(&self) -> Vec<PathBuf> {
        let mut entries: Vec<PathBuf> = fs::read_dir(".")
            .expect("Error reading directory")
            .filter_map(|res| res.ok().map(|e| e.path()))
            .collect();

        // Remove paths that are in the ignore list
        entries.retain(|x| !self.ignore.contains(x));
        entries
    }

    pub fn read_file(&self, path: PathBuf) -> String {
        let full_path = self.path.join(path);
        let file_content = fs::read_to_string(&full_path)
            .expect(format!("Error reading file {}", full_path.display()).as_str());

        file_content
    }
}
