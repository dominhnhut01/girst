//! Shared trait for Git objects stored in the object database.

/// Represents a Git object (`blob`, `tree`, `commit`) that can be serialized and addressed by SHA-1 OID.
pub trait GitObject {
    /// Returns the Git object type string (e.g. `"blob"`, `"tree"`, `"commit"`).
    fn kind(&self) -> &str;

    /// Serializes the object's raw payload bytes (without the Git `<kind> <len>\0` header).
    fn to_bytes(&self) -> Vec<u8>;

    /// Returns the 40-character hex SHA-1 object ID if computed.
    fn oid(&self) -> Option<&str>;

    /// Stores the computed 40-character hex SHA-1 object ID on this object.
    fn set_oid(&mut self, oid: String);
}
