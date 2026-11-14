//! Git commit object serialization.

use super::{author::Author, object::GitObject};

/// In-memory representation of a Git `commit` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    oid: Option<String>,
    tree: String,
    parent: Option<String>,
    author: String,
    committer: String,
    message: String,
}

impl Commit {
    /// Creates a `Commit` pointing to `tree` OID, optional `parent` OID, `author`, and commit `message`.
    pub fn new(
        tree: impl Into<String>,
        parent: Option<String>,
        author: &Author,
        message: impl Into<String>,
    ) -> Self {
        let formatted_author = author.to_string();
        Commit {
            oid: None,
            tree: tree.into(),
            parent,
            author: formatted_author.clone(),
            committer: formatted_author,
            message: message.into(),
        }
    }

    /// Reconstructs a `Commit` from parsed header fields and commit `message`.
    pub fn from_parts(
        tree: String,
        parent: Option<String>,
        author: String,
        committer: String,
        message: String,
    ) -> Self {
        Commit {
            oid: None,
            tree,
            parent,
            author,
            committer,
            message,
        }
    }

    /// Returns the root tree 40-character hex OID referenced by this commit.
    pub fn tree(&self) -> &str {
        &self.tree
    }

    /// Returns the parent commit 40-character hex OID, if any.
    pub fn parent(&self) -> Option<&str> {
        self.parent.as_deref()
    }

    /// Returns the formatted author line.
    pub fn author(&self) -> &str {
        &self.author
    }

    /// Returns the formatted committer line.
    pub fn committer(&self) -> &str {
        &self.committer
    }

    /// Returns the commit message body.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl GitObject for Commit {
    /// Returns `"commit"`.
    fn kind(&self) -> &str {
        "commit"
    }

    /// Serializes the commit header lines and message into Git's commit wire format.
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = format!("tree {}\n", self.tree);
        if let Some(parent) = &self.parent {
            out.push_str(&format!("parent {}\n", parent));
        }
        out.push_str(&format!("author {}\n", self.author));
        out.push_str(&format!("committer {}\n\n", self.committer));
        out.push_str(&self.message);
        if !self.message.ends_with('\n') {
            out.push('\n');
        }
        out.into_bytes()
    }

    /// Returns the commit's 40-character hex SHA-1 OID if computed.
    fn oid(&self) -> Option<&str> {
        self.oid.as_deref()
    }

    /// Sets the commit's 40-character hex SHA-1 OID.
    fn set_oid(&mut self, oid: String) {
        self.oid = Some(oid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_formats_root_and_child_wire_bytes() {
        let author = Author::new("Ada", "ada@example.com", 1700000000, "+0000");
        let tree_oid = "1111111111111111111111111111111111111111";
        let parent_oid = "2222222222222222222222222222222222222222";

        let root = Commit::new(tree_oid, None, &author, "initial commit");
        let expected_root = format!(
            "tree {tree_oid}\nauthor Ada <ada@example.com> 1700000000 +0000\ncommitter Ada <ada@example.com> 1700000000 +0000\n\ninitial commit\n"
        );
        assert_eq!(root.to_bytes(), expected_root.as_bytes());

        let child = Commit::new(tree_oid, Some(parent_oid.to_string()), &author, "second\n");
        let expected_child = format!(
            "tree {tree_oid}\nparent {parent_oid}\nauthor Ada <ada@example.com> 1700000000 +0000\ncommitter Ada <ada@example.com> 1700000000 +0000\n\nsecond\n"
        );
        assert_eq!(child.to_bytes(), expected_child.as_bytes());
    }
}
