//! Commit history traversal and log output formatting.

use std::{collections::HashSet, io};

use super::{
    commit::Commit,
    database::{Database, ParsedObject},
    object::GitObject,
    refs::Refs,
};

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Walks the commit parent chain starting from `HEAD`, returning commits newest-first.
///
/// Returns `Ok(Vec::new())` when `HEAD` is unborn and `InvalidData` if a parent cycle is detected.
pub fn collect_log(db: &Database, refs: &Refs) -> io::Result<Vec<Commit>> {
    collect_log_from(db, refs.read_head()?)
}

/// Walks the commit parent chain starting from `start_oid`, returning commits newest-first.
///
/// Returns `Ok(Vec::new())` when `start_oid` is `None` and `InvalidData` if a parent cycle is detected.
pub fn collect_log_from(db: &Database, start_oid: Option<String>) -> io::Result<Vec<Commit>> {
    let Some(mut cur_oid) = start_oid else {
        return Ok(Vec::new());
    };

    let mut visited = HashSet::new();
    let mut commits = Vec::new();

    loop {
        if !visited.insert(cur_oid.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cyclic commit parent detected at {cur_oid}"),
            ));
        }

        let ParsedObject::Commit(commit) = db.load(&cur_oid)? else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("object {cur_oid} is not a commit"),
            ));
        };

        let next = commit.parent().map(str::to_string);
        commits.push(commit);
        match next {
            Some(parent_oid) => cur_oid = parent_oid,
            None => break,
        }
    }

    Ok(commits)
}

/// Formats `commits` in Git's standard multi-line `log` format.
pub fn format_log(commits: &[Commit]) -> String {
    let mut out = String::new();
    for (idx, commit) in commits.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        let oid = commit.oid().unwrap_or("");
        out.push_str(&format!("commit {oid}\n"));

        match parse_author_line(commit.author()) {
            Some((identity, secs, tz, offset_secs)) => {
                out.push_str(&format!("Author: {identity}\n"));
                out.push_str(&format!(
                    "Date:   {}\n",
                    format_git_date(secs, offset_secs, tz)
                ));
            }
            None => {
                out.push_str(&format!("Author: {}\n", commit.author()));
            }
        }

        out.push('\n');
        let msg = commit.message().trim_end_matches('\n');
        for line in msg.split('\n') {
            if line.is_empty() {
                out.push('\n');
            } else {
                out.push_str(&format!("    {line}\n"));
            }
        }
    }
    out
}

/// Formats `commits` in compact `<7-char-oid> <summary>` `--oneline` format.
pub fn format_oneline(commits: &[Commit]) -> String {
    let mut out = String::new();
    for commit in commits {
        let oid = commit.oid().unwrap_or("");
        let short = oid.get(..7).unwrap_or(oid);
        let summary = commit.message().lines().next().unwrap_or("");
        out.push_str(&format!("{short} {summary}\n"));
    }
    out
}

/// Parses `<Name> <<email>> <unix_secs> <±HHMM>` into `(identity, unix_secs, tz, offset_secs)`.
fn parse_author_line(raw: &str) -> Option<(&str, i64, &str, i64)> {
    let (rest, tz) = raw.rsplit_once(' ')?;
    let (identity, secs_str) = rest.rsplit_once(' ')?;
    let secs: i64 = secs_str.parse().ok()?;
    let offset_secs = parse_tz_offset(tz)?;
    Some((identity, secs, tz, offset_secs))
}

/// Parses a `±HHMM` timezone offset into signed seconds.
fn parse_tz_offset(tz: &str) -> Option<i64> {
    let bytes = tz.as_bytes();
    if bytes.len() != 5 || (bytes[0] != b'+' && bytes[0] != b'-') {
        return None;
    }
    let sign = if bytes[0] == b'-' { -1 } else { 1 };
    let hours: i64 = tz[1..3].parse().ok()?;
    let mins: i64 = tz[3..5].parse().ok()?;
    if hours > 23 || mins > 59 {
        return None;
    }
    Some(sign * (hours * 3600 + mins * 60))
}

/// Formats a Unix timestamp and timezone offset as `<Dow> <Mon> <D> <HH>:<MM>:<SS> <YYYY> <tz>`.
fn format_git_date(unix_secs: i64, offset_secs: i64, tz: &str) -> String {
    let local_secs = unix_secs + offset_secs;
    let days = local_secs.div_euclid(86_400);
    let sec_of_day = local_secs.rem_euclid(86_400);

    let hh = sec_of_day / 3600;
    let mm = (sec_of_day % 3600) / 60;
    let ss = sec_of_day % 60;

    // 1970-01-01 (days == 0) was Thursday (index 4 in WEEKDAYS).
    let dow = WEEKDAYS[(days + 4).rem_euclid(7) as usize];
    let (year, month, day) = civil_from_days(days);
    let mon = MONTHS[(month - 1) as usize];

    format!("{dow} {mon} {day} {hh:02}:{mm:02}:{ss:02} {year} {tz}")
}

/// Converts days since 1970-01-01 to proleptic Gregorian `(year, month, day)`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Shift epoch to 0000-03-01 so leap days fall at the end of each 400-year era.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year, m as u32, d as u32)
}
