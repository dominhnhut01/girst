//! Git commit author and committer identity formatting.

use std::{
    env, fmt,
    time::{SystemTime, UNIX_EPOCH},
};

/// Represents a Git author/committer identity with name, email, and UTC timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    name: String,
    email: String,
    timestamp_secs: u64,
    tz_offset: String,
}

impl Author {
    /// Creates an `Author` with explicit identity fields and timestamp.
    pub fn new(
        name: impl Into<String>,
        email: impl Into<String>,
        timestamp_secs: u64,
        tz_offset: impl Into<String>,
    ) -> Self {
        Author {
            name: name.into(),
            email: email.into(),
            timestamp_secs,
            tz_offset: tz_offset.into(),
        }
    }

    /// Builds an `Author` from `GIT_AUTHOR_NAME` and `GIT_AUTHOR_EMAIL` with defaults and current UTC time.
    pub fn from_env() -> Self {
        let name = env::var("GIT_AUTHOR_NAME")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Girst User".to_string());
        let email = env::var("GIT_AUTHOR_EMAIL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "girst@example.com".to_string());
        let timestamp_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Author::new(name, email, timestamp_secs, "+0000")
    }
}

impl fmt::Display for Author {
    /// Formats the identity as `<name> <<email>> <unix_secs> <tz>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} <{}> {} {}",
            self.name, self.email, self.timestamp_secs, self.tz_offset
        )
    }
}
