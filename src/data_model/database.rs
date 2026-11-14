//! Loose Git object database backed by `.girst/objects`.

use std::{
    fs::{self, File},
    io::{self, Write},
    path::PathBuf,
};

use deflate::deflate_bytes_zlib;
use rand::Rng;
use sha1::{Digest, Sha1};

use super::object::GitObject;

/// Content-addressable loose object store rooted at an `objects` directory.
pub struct Database {
    path: PathBuf,
}

impl Database {
    /// Creates a `Database` rooted at `path` (typically `.girst/objects`).
    pub fn new(path: PathBuf) -> Self {
        Database { path }
    }

    /// Hashes `object` with its Git header, assigns its OID, and writes it if not already present.
    ///
    /// Propagates any filesystem I/O error encountered while creating directories or writing the object.
    pub fn store<T: GitObject>(&self, object: &mut T) -> io::Result<()> {
        let payload = object.to_bytes();
        let header = format!("{} {}\0", object.kind(), payload.len());
        let mut content = Vec::with_capacity(header.len() + payload.len());
        content.extend_from_slice(header.as_bytes());
        content.extend_from_slice(&payload);

        let mut hasher = Sha1::new();
        hasher.update(&content);
        let oid = hex::encode(hasher.finalize());
        object.set_oid(oid.clone());

        self.write_object(&oid, &content)
    }

    /// Writes `content` compressed with zlib to `<db>/<oid[0..2]>/<oid[2..]>` atomically via temp file.
    fn write_object(&self, oid: &str, content: &[u8]) -> io::Result<()> {
        let object_path = self.path.join(&oid[0..2]).join(&oid[2..]);
        if object_path.exists() {
            return Ok(());
        }

        let dirname = object_path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "Parent directory not found")
        })?;
        fs::create_dir_all(dirname)?;

        let temp_path = dirname.join(format!("tmp_obj_{}", generate_random_string(14)));
        let compressed = deflate_bytes_zlib(content);

        let mut file = File::create(&temp_path)?;
        file.write_all(&compressed)?;
        fs::rename(&temp_path, object_path)?;
        Ok(())
    }
}

/// Generates an alphanumeric suffix of `length` characters for temporary object filenames.
fn generate_random_string(length: usize) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();

    (0..length)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use uuid::Uuid;

    use super::*;
    use crate::data_model::blob::Blob;

    fn temp_db_dir() -> PathBuf {
        std::env::temp_dir().join(format!("girst_db_test_{}", Uuid::new_v4()))
    }

    #[test]
    fn blob_hello_hashes_to_git_oid() {
        let db_path = temp_db_dir();
        let db = Database::new(db_path.clone());
        let mut blob = Blob::new(b"hello\n".to_vec());

        db.store(&mut blob).expect("store should succeed");
        assert_eq!(blob.oid(), Some("ce013625030ba8dba906f756967f9e9ca394464a"));

        let _ = fs::remove_dir_all(&db_path);
    }

    #[test]
    fn store_writes_zlib_object_and_skips_existing() {
        let db_path = temp_db_dir();
        let db = Database::new(db_path.clone());
        let mut blob = Blob::new(b"hello\n".to_vec());

        db.store(&mut blob).expect("initial store should succeed");
        let obj_path = db_path
            .join("ce")
            .join("013625030ba8dba906f756967f9e9ca394464a");
        assert!(obj_path.exists(), "loose object must exist at 2/38 path");

        let output = Command::new("python3")
            .arg("-c")
            .arg("import sys, zlib; sys.stdout.buffer.write(zlib.decompress(open(sys.argv[1], 'rb').read()))")
            .arg(&obj_path)
            .output()
            .expect("python3 should run");
        assert!(output.status.success(), "python3 zlib.decompress failed");
        assert_eq!(output.stdout, b"blob 6\0hello\n");

        // Overwrite with sentinel bytes to verify subsequent store skips writing when file exists.
        fs::write(&obj_path, b"sentinel").expect("write sentinel");
        let mut duplicate = Blob::new(b"hello\n".to_vec());
        db.store(&mut duplicate)
            .expect("duplicate store should succeed");
        assert_eq!(
            fs::read(&obj_path).expect("read sentinel"),
            b"sentinel",
            "store should skip rewriting existing object"
        );

        let _ = fs::remove_dir_all(&db_path);
    }
}
