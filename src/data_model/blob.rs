//! Git blob object representing raw file contents.

use super::object::GitObject;

/// In-memory representation of a Git `blob` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    oid: Option<String>,
    data: Vec<u8>,
}

impl Blob {
    /// Creates a new `Blob` from raw file bytes with no assigned OID.
    pub fn new(data: Vec<u8>) -> Self {
        Blob { oid: None, data }
    }

    /// Returns a slice of the raw blob payload bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

impl GitObject for Blob {
    /// Returns `"blob"`.
    fn kind(&self) -> &str {
        "blob"
    }

    /// Returns a clone of the raw blob payload bytes.
    fn to_bytes(&self) -> Vec<u8> {
        self.data.clone()
    }

    /// Returns the blob's SHA-1 OID if it has been stored or hashed.
    fn oid(&self) -> Option<&str> {
        self.oid.as_deref()
    }

    /// Sets the blob's 40-character hex SHA-1 OID.
    fn set_oid(&mut self, oid: String) {
        self.oid = Some(oid);
    }
}
