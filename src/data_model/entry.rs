//! Git tree entry representing a file or subtree reference.

/// Single entry inside a Git `tree` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub oid: String,
    pub mode: String,
}

impl Entry {
    /// Creates a new tree `Entry` with a basename `name`, 40-char hex `oid`, and Git `mode`.
    pub fn new(name: impl Into<String>, oid: impl Into<String>, mode: impl Into<String>) -> Self {
        Entry {
            name: name.into(),
            oid: oid.into(),
            mode: mode.into(),
        }
    }

    /// Returns `true` when this entry represents a subtree (`mode == "40000"` or `"040000"`).
    pub fn is_dir(&self) -> bool {
        self.mode == "40000" || self.mode == "040000"
    }
}
