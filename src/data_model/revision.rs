//! Git revision expression parser and commit resolver.

use std::io;

use super::{
    commit::Commit,
    database::{Database, ParsedObject},
    refs::Refs,
};

/// Resolves a Git revision expression `<expr>` into a verified 40-character `Commit` OID.
///
/// Supports `@`, `HEAD`, local branch names, 4..=40 hex object prefixes, and chained `^` / `~` / `~<n>` operators.
pub fn resolve_revision(db: &Database, refs: &Refs, expr: &str) -> io::Result<String> {
    let op_start = expr.find(['^', '~']).unwrap_or(expr.len());
    let (base, ops_str) = expr.split_at(op_start);
    if base.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid revision expression: '{expr}'"),
        ));
    }

    let steps = parse_parent_ops(ops_str, expr)?;
    let mut oid = resolve_base(db, refs, base)?;
    let mut commit = load_commit(db, &oid)?;

    for step_count in steps {
        for _ in 0..step_count {
            let parent_oid = commit
                .parent()
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("commit {oid} has no parent"),
                    )
                })?
                .to_string();
            commit = load_commit(db, &parent_oid)?;
            oid = parent_oid;
        }
    }

    Ok(oid)
}

/// Parses chained `^`, `^1`, `~`, and `~<n>` operators into first-parent step counts.
fn parse_parent_ops(ops_str: &str, full_expr: &str) -> io::Result<Vec<usize>> {
    let bytes = ops_str.as_bytes();
    let mut steps = Vec::new();
    let mut idx = 0;

    while idx < bytes.len() {
        match bytes[idx] {
            b'^' => {
                idx += 1;
                let digit_start = idx;
                while idx < bytes.len() && bytes[idx].is_ascii_digit() {
                    idx += 1;
                }
                let digits = &ops_str[digit_start..idx];
                if digits.is_empty() || digits == "1" {
                    steps.push(1);
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unsupported parent selector '^{digits}' in '{full_expr}'"),
                    ));
                }
            }
            b'~' => {
                idx += 1;
                let digit_start = idx;
                while idx < bytes.len() && bytes[idx].is_ascii_digit() {
                    idx += 1;
                }
                let digits = &ops_str[digit_start..idx];
                if digits.is_empty() {
                    steps.push(1);
                } else {
                    let count: usize = digits.parse().map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("invalid ancestor count '~{digits}' in '{full_expr}'"),
                        )
                    })?;
                    steps.push(count);
                }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid revision operator in '{full_expr}'"),
                ));
            }
        }
    }

    Ok(steps)
}

/// Resolves the base name (`@`, `HEAD`, branch name, or 4..=40 hex OID prefix) to a 40-char OID.
fn resolve_base(db: &Database, refs: &Refs, base: &str) -> io::Result<String> {
    if base == "@" || base == "HEAD" {
        return refs.read_head()?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "HEAD is unborn")
        });
    }

    if let Some(branch_oid) = refs.read_branch(base)? {
        return Ok(branch_oid);
    }

    if (4..=40).contains(&base.len()) && base.bytes().all(|b| b.is_ascii_hexdigit()) {
        let lower_hex = base.to_ascii_lowercase();
        let matches = db.resolve_prefix(&lower_hex)?;
        return match matches.len() {
            0 => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("unknown revision '{base}'"),
            )),
            1 => matches
                .into_iter()
                .next()
                .ok_or_else(|| io::Error::other("missing resolved OID")),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("ambiguous object id '{base}': {}", matches.join(", ")),
            )),
        };
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("unknown revision '{base}'"),
    ))
}

/// Loads `oid` from `db` and verifies that it is a `Commit` object.
fn load_commit(db: &Database, oid: &str) -> io::Result<Commit> {
    match db.load(oid)? {
        ParsedObject::Commit(commit) => Ok(commit),
        ParsedObject::Blob(_) | ParsedObject::Tree(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("object {oid} is not a commit"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, fs, path::PathBuf};

    use sha1::{Digest, Sha1};
    use uuid::Uuid;

    use super::*;
    use crate::data_model::{author::Author, blob::Blob, object::GitObject, tree::Tree};

    fn temp_repo_dir() -> PathBuf {
        std::env::temp_dir().join(format!("girst_rev_test_{}", Uuid::new_v4()))
    }

    #[test]
    fn resolve_revision_symbols_prefixes_ambiguity_non_commit_and_ancestors() {
        let repo = temp_repo_dir();
        let girst_dir = repo.join(".girst");
        let db_dir = girst_dir.join("objects");
        fs::create_dir_all(girst_dir.join("refs/heads")).unwrap();
        fs::write(girst_dir.join("HEAD"), b"ref: refs/heads/main\n").unwrap();

        let db = Database::new(db_dir);
        let refs = Refs::new(&girst_dir);
        let author = Author::new("Ada", "ada@example.com", 1700000000, "+0000");

        let mut blob = Blob::new(b"data\n".to_vec());
        db.store(&mut blob).unwrap();
        let blob_oid = blob.oid().unwrap().to_string();

        let mut tree = Tree::new(Vec::new());
        db.store(&mut tree).unwrap();
        let tree_oid = tree.oid().unwrap().to_string();

        let mut c1 = Commit::new(&tree_oid, None, &author, "c1\n");
        db.store(&mut c1).unwrap();
        let c1_oid = c1.oid().unwrap().to_string();

        let mut c2 = Commit::new(&tree_oid, Some(c1_oid.clone()), &author, "c2\n");
        db.store(&mut c2).unwrap();
        let c2_oid = c2.oid().unwrap().to_string();

        let mut c3 = Commit::new(&tree_oid, Some(c2_oid.clone()), &author, "c3\n");
        db.store(&mut c3).unwrap();
        let c3_oid = c3.oid().unwrap().to_string();

        let mut c4 = Commit::new(&tree_oid, Some(c3_oid.clone()), &author, "c4\n");
        db.store(&mut c4).unwrap();
        let c4_oid = c4.oid().unwrap().to_string();

        refs.update_head(&c4_oid).unwrap();
        refs.create_branch("topic", &c2_oid).unwrap();

        assert_eq!(resolve_revision(&db, &refs, "@").unwrap(), c4_oid);
        assert_eq!(resolve_revision(&db, &refs, "HEAD").unwrap(), c4_oid);
        assert_eq!(resolve_revision(&db, &refs, "topic").unwrap(), c2_oid);
        assert_eq!(resolve_revision(&db, &refs, &c3_oid).unwrap(), c3_oid);
        assert_eq!(resolve_revision(&db, &refs, &c3_oid[..6]).unwrap(), c3_oid);
        assert_eq!(resolve_revision(&db, &refs, "HEAD^").unwrap(), c3_oid);
        assert_eq!(resolve_revision(&db, &refs, "HEAD^1~2").unwrap(), c1_oid);
        assert_eq!(resolve_revision(&db, &refs, "@~1^").unwrap(), c2_oid);

        // Stepping past root commit and unsupported ^2 fail with InvalidInput.
        assert_eq!(
            resolve_revision(&db, &refs, "HEAD~4").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            resolve_revision(&db, &refs, "HEAD^2").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );

        // Non-commit objects (blob and tree) are rejected.
        assert_eq!(
            resolve_revision(&db, &refs, &blob_oid[..6])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            resolve_revision(&db, &refs, &tree_oid).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );

        // Generate two commits whose 4-char OID prefixes collide to test ambiguous prefix reporting.
        let mut seen: HashMap<String, Commit> = HashMap::new();
        let mut pair: Option<(Commit, Commit)> = None;
        for i in 0..2000 {
            let candidate = Commit::new(&tree_oid, None, &author, format!("collide-{i}\n"));
            let payload = candidate.to_bytes();
            let raw = [
                format!("commit {}\0", payload.len()).as_bytes(),
                payload.as_slice(),
            ]
            .concat();
            let oid = hex::encode(Sha1::digest(&raw));
            let prefix = oid[..4].to_string();
            if let Some(prev) = seen.insert(prefix, candidate.clone()) {
                pair = Some((prev, candidate));
                break;
            }
        }
        let (mut dup1, mut dup2) = pair.expect("should find 4-char SHA-1 prefix collision");
        db.store(&mut dup1).unwrap();
        db.store(&mut dup2).unwrap();
        let mut expected = vec![
            dup1.oid().unwrap().to_string(),
            dup2.oid().unwrap().to_string(),
        ];
        expected.sort();
        let ambig_prefix = &expected[0][..4];
        let err = resolve_revision(&db, &refs, ambig_prefix).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let msg = err.to_string();
        assert!(msg.contains(&expected[0]) && msg.contains(&expected[1]));

        let _ = fs::remove_dir_all(&repo);
    }
}
