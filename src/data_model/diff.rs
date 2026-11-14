//! Myers line-level diff, unified hunk grouping, and workspace/cached diff collectors.

use std::{collections::BTreeSet, io, os::unix::fs::MetadataExt, path::Path};

use super::{
    database::{Database, ParsedObject},
    index::Index,
    refs::Refs,
    status::{hash_blob_oid, load_head_tree},
    workspace::Workspace,
};

/// Single line edit operation produced by the Myers shortest-edit-script algorithm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit<'a> {
    /// Line is unchanged between old and new inputs.
    Equal {
        /// Line text (including trailing `\n` if present).
        line: &'a str,
        /// 0-based line index in the old input.
        old_idx: usize,
        /// 0-based line index in the new input.
        new_idx: usize,
    },
    /// Line was removed from the old input.
    Delete {
        /// Line text (including trailing `\n` if present).
        line: &'a str,
        /// 0-based line index in the old input.
        old_idx: usize,
        /// 0-based line position in the new input.
        new_idx: usize,
    },
    /// Line was inserted into the new input.
    Insert {
        /// Line text (including trailing `\n` if present).
        line: &'a str,
        /// 0-based line position in the old input.
        old_idx: usize,
        /// 0-based line index in the new input.
        new_idx: usize,
    },
}

/// Contiguous unified-diff hunk with 1-based line ranges and edit lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk<'a> {
    /// 1-based start line in the old file, or `0` when `old_count == 0`.
    pub old_start: usize,
    /// Number of old-file lines covered by this hunk.
    pub old_count: usize,
    /// 1-based start line in the new file, or `0` when `new_count == 0`.
    pub new_start: usize,
    /// Number of new-file lines covered by this hunk.
    pub new_count: usize,
    /// Ordered slice of edits (`Equal`, `Delete`, `Insert`) belonging to this hunk.
    pub edits: Vec<Edit<'a>>,
}

/// Computes the Myers shortest edit script between `old` and `new` text.
pub fn myers_diff<'a>(old: &'a str, new: &'a str) -> Vec<Edit<'a>> {
    let old_lines: Vec<&'a str> = old.split_inclusive('\n').collect();
    let new_lines: Vec<&'a str> = new.split_inclusive('\n').collect();
    let n = old_lines.len() as isize;
    let m = new_lines.len() as isize;

    if n == 0 && m == 0 {
        return Vec::new();
    }

    let max = (n + m) as usize;
    let offset = max as isize;
    let mut v = vec![0isize; 2 * max + 1];
    let mut trace: Vec<Vec<isize>> = Vec::new();

    'outer: for d in 0..=max {
        // Snapshot V[k] before advancing depth d so backtracking can reconstruct the exact path.
        trace.push(v.clone());
        let d_i = d as isize;
        for k in (-d_i..=d_i).step_by(2) {
            let mut x = if d == 0 {
                0
            } else {
                // Prefer moving right (Delete) over down (Insert) on ties so '-' precedes '+'.
                let go_down = k == -d_i
                    || (k != d_i && v[(k - 1 + offset) as usize] < v[(k + 1 + offset) as usize]);
                if go_down {
                    v[(k + 1 + offset) as usize]
                } else {
                    v[(k - 1 + offset) as usize] + 1
                }
            };
            let mut y = x - k;

            while x < n && y < m && old_lines[x as usize] == new_lines[y as usize] {
                x += 1;
                y += 1;
            }
            v[(k + offset) as usize] = x;

            if x >= n && y >= m {
                break 'outer;
            }
        }
    }

    backtrack_myers(&old_lines, &new_lines, &trace, offset)
}

/// Backtracks through recorded `V[k]` snapshots to build the ordered edit script.
fn backtrack_myers<'a>(
    old_lines: &[&'a str],
    new_lines: &[&'a str],
    trace: &[Vec<isize>],
    offset: isize,
) -> Vec<Edit<'a>> {
    let mut edits = Vec::new();
    let mut x = old_lines.len() as isize;
    let mut y = new_lines.len() as isize;

    for (d, prev_v) in trace.iter().enumerate().rev() {
        if d == 0 {
            while x > 0 && y > 0 {
                x -= 1;
                y -= 1;
                edits.push(Edit::Equal {
                    line: old_lines[x as usize],
                    old_idx: x as usize,
                    new_idx: y as usize,
                });
            }
            break;
        }

        let d_i = d as isize;
        let k = x - y;
        let go_down = k == -d_i
            || (k != d_i && prev_v[(k - 1 + offset) as usize] < prev_v[(k + 1 + offset) as usize]);
        let prev_k = if go_down { k + 1 } else { k - 1 };
        let prev_x = prev_v[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;

        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            edits.push(Edit::Equal {
                line: old_lines[x as usize],
                old_idx: x as usize,
                new_idx: y as usize,
            });
        }

        if x > prev_x {
            x -= 1;
            edits.push(Edit::Delete {
                line: old_lines[x as usize],
                old_idx: x as usize,
                new_idx: y as usize,
            });
        } else {
            y -= 1;
            edits.push(Edit::Insert {
                line: new_lines[y as usize],
                old_idx: x as usize,
                new_idx: y as usize,
            });
        }
    }

    edits.reverse();
    edits
}

/// Groups `edits` into unified hunks with `context` surrounding unchanged lines.
pub fn group_hunks<'a>(edits: &[Edit<'a>], context: usize) -> Vec<Hunk<'a>> {
    let mut windows: Vec<(usize, usize)> = Vec::new();
    for (idx, edit) in edits.iter().enumerate() {
        if matches!(edit, Edit::Equal { .. }) {
            continue;
        }
        let win_start = idx.saturating_sub(context);
        let win_end = (idx + context + 1).min(edits.len());
        if let Some((_, prev_end)) = windows.last_mut() {
            if win_start <= *prev_end {
                *prev_end = (*prev_end).max(win_end);
                continue;
            }
        }
        windows.push((win_start, win_end));
    }

    windows
        .into_iter()
        .map(|(start, end)| build_hunk(&edits[start..end]))
        .collect()
}

/// Builds a `Hunk` from a contiguous edit slice, computing 1-based old/new ranges.
fn build_hunk<'a>(slice: &[Edit<'a>]) -> Hunk<'a> {
    let mut old_count = 0;
    let mut new_count = 0;
    let mut first_old: Option<usize> = None;
    let mut first_new: Option<usize> = None;

    for edit in slice {
        match edit {
            Edit::Equal {
                old_idx, new_idx, ..
            } => {
                first_old.get_or_insert(*old_idx);
                first_new.get_or_insert(*new_idx);
                old_count += 1;
                new_count += 1;
            }
            Edit::Delete { old_idx, .. } => {
                first_old.get_or_insert(*old_idx);
                old_count += 1;
            }
            Edit::Insert { new_idx, .. } => {
                first_new.get_or_insert(*new_idx);
                new_count += 1;
            }
        }
    }

    let old_start = if old_count == 0 {
        0
    } else {
        first_old.map_or(0, |idx| idx + 1)
    };
    let new_start = if new_count == 0 {
        0
    } else {
        first_new.map_or(0, |idx| idx + 1)
    };

    Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        edits: slice.to_vec(),
    }
}

/// File state tuple `(oid_hex, mode_str, raw_bytes)` for one side of a file diff.
pub type DiffSide<'a> = (&'a str, &'a str, &'a [u8]);

/// Formats a single file's unified diff (or binary notice) between optional `old` and `new` sides.
pub fn format_file_diff(path: &str, old: Option<DiffSide<'_>>, new: Option<DiffSide<'_>>) -> String {
    let mut out = format!("diff --git a/{path} b/{path}\n");
    let (a_label, b_label) = match (old, new) {
        (None, Some((new_oid, new_mode, _))) => {
            out.push_str(&format!("new file mode {new_mode}\n"));
            out.push_str(&format!("index 0000000..{}\n", short_oid(new_oid)));
            ("/dev/null".to_string(), format!("b/{path}"))
        }
        (Some((old_oid, old_mode, _)), None) => {
            out.push_str(&format!("deleted file mode {old_mode}\n"));
            out.push_str(&format!("index {}..0000000\n", short_oid(old_oid)));
            (format!("a/{path}"), "/dev/null".to_string())
        }
        (Some((old_oid, old_mode, _)), Some((new_oid, new_mode, _))) => {
            if old_mode != new_mode {
                out.push_str(&format!("old mode {old_mode}\n"));
                out.push_str(&format!("new mode {new_mode}\n"));
                out.push_str(&format!(
                    "index {}..{}\n",
                    short_oid(old_oid),
                    short_oid(new_oid)
                ));
            } else {
                out.push_str(&format!(
                    "index {}..{} {new_mode}\n",
                    short_oid(old_oid),
                    short_oid(new_oid)
                ));
            }
            (format!("a/{path}"), format!("b/{path}"))
        }
        (None, None) => return String::new(),
    };

    let old_bytes = old.map_or(&[][..], |(_, _, bytes)| bytes);
    let new_bytes = new.map_or(&[][..], |(_, _, bytes)| bytes);
    if (old.is_some() && is_binary(old_bytes)) || (new.is_some() && is_binary(new_bytes)) {
        out.push_str(&format!("Binary files {a_label} and {b_label} differ\n"));
        return out;
    }

    let old_text = std::str::from_utf8(old_bytes).unwrap_or("");
    let new_text = std::str::from_utf8(new_bytes).unwrap_or("");
    let edits = myers_diff(old_text, new_text);
    let hunks = group_hunks(&edits, 3);
    if hunks.is_empty() {
        return out;
    }

    out.push_str(&format!("--- {a_label}\n"));
    out.push_str(&format!("+++ {b_label}\n"));
    for hunk in hunks {
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
        ));
        for edit in hunk.edits {
            let (prefix, line) = match edit {
                Edit::Equal { line, .. } => (' ', line),
                Edit::Delete { line, .. } => ('-', line),
                Edit::Insert { line, .. } => ('+', line),
            };
            out.push(prefix);
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push_str("\n\\ No newline at end of file\n");
            }
        }
    }
    out
}

/// Computes the unstaged unified diff between `.girst/index` and the working tree.
pub fn diff_workspace(workspace: &Workspace, db: &Database, index: &Index) -> io::Result<String> {
    let mut out = String::new();

    for entry in index.entries() {
        let idx_oid = hex::encode(entry.oid);
        let idx_mode = mode_str(entry.mode);
        let meta = match workspace.stat_file(Path::new(&entry.path)) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let old_bytes = load_blob_bytes(db, &idx_oid)?;
                out.push_str(&format_file_diff(
                    &entry.path,
                    Some((&idx_oid, idx_mode, &old_bytes)),
                    None,
                ));
                continue;
            }
            Err(e) => return Err(e),
        };

        if !meta.is_file() {
            let old_bytes = load_blob_bytes(db, &idx_oid)?;
            out.push_str(&format_file_diff(
                &entry.path,
                Some((&idx_oid, idx_mode, &old_bytes)),
                None,
            ));
            continue;
        }

        let ws_mode_num = if meta.mode() & 0o111 != 0 {
            0o100755
        } else {
            0o100644
        };
        let stat_matches = meta.size() as u32 == entry.size
            && meta.mtime() as u32 == entry.mtime_sec
            && meta.mtime_nsec() as u32 == entry.mtime_nsec;
        let racily_clean = entry.mtime_sec == entry.ctime_sec
            || (meta.mtime() as u32) == (meta.ctime() as u32);

        if ws_mode_num == entry.mode && stat_matches && !racily_clean {
            continue;
        }

        let ws_bytes = workspace.read_file(Path::new(&entry.path))?;
        let ws_oid_raw = hash_blob_oid(&ws_bytes);
        if ws_mode_num == entry.mode && ws_oid_raw == entry.oid {
            continue;
        }

        let old_bytes = load_blob_bytes(db, &idx_oid)?;
        let ws_oid = hex::encode(ws_oid_raw);
        let ws_mode = mode_str(ws_mode_num);
        out.push_str(&format_file_diff(
            &entry.path,
            Some((&idx_oid, idx_mode, &old_bytes)),
            Some((&ws_oid, ws_mode, &ws_bytes)),
        ));
    }

    Ok(out)
}

/// Computes the staged unified diff between `HEAD` and `.girst/index`.
pub fn diff_cached(db: &Database, index: &Index, refs: &Refs) -> io::Result<String> {
    let head_tree = load_head_tree(db, refs)?;
    let mut paths: BTreeSet<String> = head_tree.keys().cloned().collect();
    for entry in index.entries() {
        paths.insert(entry.path.clone());
    }

    let mut out = String::new();
    for path in paths {
        match (head_tree.get(&path), index.get(&path)) {
            (None, Some(idx)) => {
                let new_oid = hex::encode(idx.oid);
                let new_bytes = load_blob_bytes(db, &new_oid)?;
                out.push_str(&format_file_diff(
                    &path,
                    None,
                    Some((&new_oid, mode_str(idx.mode), &new_bytes)),
                ));
            }
            (Some((old_oid, old_mode)), None) => {
                let old_bytes = load_blob_bytes(db, old_oid)?;
                out.push_str(&format_file_diff(
                    &path,
                    Some((old_oid, old_mode, &old_bytes)),
                    None,
                ));
            }
            (Some((old_oid, old_mode)), Some(idx)) => {
                let new_oid = hex::encode(idx.oid);
                let new_mode = mode_str(idx.mode);
                if old_oid == &new_oid && old_mode == new_mode {
                    continue;
                }
                let old_bytes = load_blob_bytes(db, old_oid)?;
                let new_bytes = load_blob_bytes(db, &new_oid)?;
                out.push_str(&format_file_diff(
                    &path,
                    Some((old_oid, old_mode, &old_bytes)),
                    Some((&new_oid, new_mode, &new_bytes)),
                ));
            }
            (None, None) => {}
        }
    }
    Ok(out)
}

/// Loads raw blob bytes for `oid` from `db`, returning `InvalidData` if it is not a blob.
fn load_blob_bytes(db: &Database, oid: &str) -> io::Result<Vec<u8>> {
    match db.load(oid)? {
        ParsedObject::Blob(blob) => Ok(blob.data().to_vec()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("object {oid} is not a blob"),
        )),
    }
}

/// Returns `true` when `bytes` contains NUL bytes or is not valid UTF-8.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

/// Formats a numeric index file mode as `"100755"` or `"100644"`.
fn mode_str(mode: u32) -> &'static str {
    if mode & 0o111 != 0 {
        "100755"
    } else {
        "100644"
    }
}

/// Returns the 7-character abbreviated prefix of `oid`.
fn short_oid(oid: &str) -> &str {
    oid.get(..7).unwrap_or(oid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn myers_diff_and_hunk_merging_vs_splitting() {
        let old_merged = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";
        let new_merged = "1\nTWO\n3\n4\n5\n6\n7\n8\nNINE\n10\n";
        let merged_hunks = group_hunks(&myers_diff(old_merged, new_merged), 3);
        assert_eq!(merged_hunks.len(), 1, "<= 6 equal lines must merge");
        assert_eq!(merged_hunks[0].old_start, 1);
        assert_eq!(merged_hunks[0].old_count, 10);
        assert_eq!(merged_hunks[0].new_start, 1);
        assert_eq!(merged_hunks[0].new_count, 10);

        let old_split = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n";
        let new_split = "1\nTWO\n3\n4\n5\n6\n7\n8\n9\nTEN\n11\n";
        let split_hunks = group_hunks(&myers_diff(old_split, new_split), 3);
        assert_eq!(split_hunks.len(), 2, "> 6 equal lines must split");
        assert_eq!(
            (split_hunks[0].old_start, split_hunks[0].old_count),
            (1, 5)
        );
        assert_eq!(
            (split_hunks[1].old_start, split_hunks[1].old_count),
            (7, 5)
        );
    }

    #[test]
    fn unified_diff_added_modified_deleted_and_binary() {
        let added = format_file_diff(
            "new.txt",
            None,
            Some(("1111111222222233333334444444555555566666", "100644", b"a\nb\n")),
        );
        assert!(added.contains("new file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+a\n+b\n"));

        let modified = format_file_diff(
            "mod.txt",
            Some(("aaaaaaa111111122222223333333444444455555", "100644", b"old")),
            Some(("bbbbbbb111111122222223333333444444455555", "100755", b"new\n")),
        );
        assert!(modified.contains("old mode 100644\nnew mode 100755\nindex aaaaaaa..bbbbbbb\n"));
        assert!(modified.contains("-old\n\\ No newline at end of file\n+new\n"));

        let deleted = format_file_diff(
            "gone.txt",
            Some(("ccccccc111111122222223333333444444455555", "100644", b"bye\n")),
            None,
        );
        assert!(deleted.contains("deleted file mode 100644\nindex ccccccc..0000000\n--- a/gone.txt\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-bye\n"));

        let binary = format_file_diff(
            "img.bin",
            Some(("ddddddd111111122222223333333444444455555", "100644", b"\x00\xff")),
            Some(("eeeeeee111111122222223333333444444455555", "100644", b"\x00\xfe")),
        );
        assert!(binary.contains("Binary files a/img.bin and b/img.bin differ\n"));
        assert!(!binary.contains("--- a/img.bin"));
    }
}
