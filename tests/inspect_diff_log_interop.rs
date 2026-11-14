//! End-to-end integration test for `girst status`, `girst diff`, `girst log`, and `git fsck`.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use uuid::Uuid;

/// Returns the compiled `girst` binary path.
fn girst_bin() -> &'static str {
    env!("CARGO_BIN_EXE_girst")
}

/// Returns a unique temporary directory path for isolating a test repository.
fn temp_sandbox_dir() -> PathBuf {
    std::env::temp_dir().join(format!("girst_inspect_interop_{}", Uuid::new_v4()))
}

/// Runs `girst` with `args` inside `cwd` and returns the process output.
fn run_girst(cwd: &Path, args: &[&str]) -> Output {
    Command::new(girst_bin())
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "Tester")
        .env("GIT_AUTHOR_EMAIL", "tester@example.com")
        .output()
        .expect("execute girst")
}

#[test]
fn inspect_status_diff_log_unborn_to_multi_commit_and_git_fsck() {
    let sandbox = temp_sandbox_dir();
    fs::create_dir_all(&sandbox).expect("create sandbox");

    let init_out = run_girst(&sandbox, &["init"]);
    assert!(init_out.status.success(), "girst init failed");

    // Unborn HEAD: status --porcelain, diff, diff --cached, log, and log --oneline all exit 0 empty.
    for args in [
        &["status", "--porcelain"][..],
        &["diff"][..],
        &["diff", "--cached"][..],
        &["log"][..],
        &["log", "--oneline"][..],
    ] {
        let out = run_girst(&sandbox, args);
        assert!(out.status.success(), "{args:?} failed on unborn HEAD");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    }

    // First commit: a.txt and dir/b.txt.
    fs::create_dir_all(sandbox.join("dir")).expect("create dir");
    fs::write(sandbox.join("a.txt"), "line1\nline2\n").expect("write a.txt");
    fs::write(sandbox.join("dir/b.txt"), "b1\n").expect("write dir/b.txt");
    assert!(run_girst(&sandbox, &["add", "."]).status.success());
    let c1_out = run_girst(&sandbox, &["commit", "-m", "initial commit"]);
    assert!(c1_out.status.success(), "first commit failed");

    // Second commit: modify a.txt and add c.txt.
    fs::write(sandbox.join("a.txt"), "line1\nline2_v2\n").expect("update a.txt");
    fs::write(sandbox.join("c.txt"), "c1\n").expect("write c.txt");
    assert!(run_girst(&sandbox, &["add", "a.txt", "c.txt"]).status.success());
    let c2_out = run_girst(&sandbox, &["commit", "-m", "second commit"]);
    assert!(c2_out.status.success(), "second commit failed");

    // Clean tree status & diff.
    let clean_status = run_girst(&sandbox, &["status"]);
    assert!(clean_status.status.success());
    assert_eq!(
        String::from_utf8_lossy(&clean_status.stdout),
        "nothing to commit, working tree clean\n"
    );

    // Build mixed staged, unstaged, MM, deleted, and untracked states:
    // 1) a.txt: stage v3, then edit workspace to v4 -> MM
    fs::write(sandbox.join("a.txt"), "line1\nline2_v3\n").expect("stage v3");
    assert!(run_girst(&sandbox, &["add", "a.txt"]).status.success());
    fs::write(sandbox.join("a.txt"), "line1\nline2_v4\n").expect("worktree v4");
    // 2) c.txt: stage new file d.txt -> A , delete c.txt from disk ->  D
    fs::write(sandbox.join("d.txt"), "d1\n").expect("write d.txt");
    assert!(run_girst(&sandbox, &["add", "d.txt"]).status.success());
    fs::remove_file(sandbox.join("c.txt")).expect("remove c.txt");
    // 3) Untracked nested dir -> ?? extra/
    fs::create_dir_all(sandbox.join("extra/sub")).expect("create extra");
    fs::write(sandbox.join("extra/sub/u.txt"), "u\n").expect("write u.txt");

    let porcelain = run_girst(&sandbox, &["status", "--porcelain"]);
    assert!(porcelain.status.success());
    assert_eq!(
        String::from_utf8_lossy(&porcelain.stdout),
        "MM a.txt\n D c.txt\nA  d.txt\n?? extra/\n"
    );

    // Unstaged diff shows a.txt (v3 -> v4) and deleted c.txt.
    let ws_diff = run_girst(&sandbox, &["diff"]);
    assert!(ws_diff.status.success());
    let ws_diff_str = String::from_utf8_lossy(&ws_diff.stdout);
    assert!(ws_diff_str.contains("diff --git a/a.txt b/a.txt"));
    assert!(ws_diff_str.contains("-line2_v3\n+line2_v4\n"));
    assert!(ws_diff_str.contains("deleted file mode 100644"));
    assert!(ws_diff_str.contains("--- a/c.txt\n+++ /dev/null"));

    // Cached diff shows a.txt (v2 -> v3) and added d.txt.
    let cached_diff = run_girst(&sandbox, &["diff", "--cached"]);
    assert!(cached_diff.status.success());
    let cached_diff_str = String::from_utf8_lossy(&cached_diff.stdout);
    assert!(cached_diff_str.contains("-line2_v2\n+line2_v3\n"));
    assert!(cached_diff_str.contains("new file mode 100644"));
    assert!(cached_diff_str.contains("--- /dev/null\n+++ b/d.txt"));

    // Log and log --oneline show second commit then initial commit.
    let log_out = run_girst(&sandbox, &["log"]);
    assert!(log_out.status.success());
    let log_str = String::from_utf8_lossy(&log_out.stdout);
    let pos_second = log_str.find("second commit").expect("second commit in log");
    let pos_first = log_str.find("initial commit").expect("initial commit in log");
    assert!(pos_second < pos_first, "newest commit must appear first");
    assert!(log_str.contains("Author: Tester <tester@example.com>"));
    assert!(log_str.contains("Date:   "));

    let oneline_out = run_girst(&sandbox, &["log", "--oneline"]);
    assert!(oneline_out.status.success());
    let oneline_str = String::from_utf8_lossy(&oneline_out.stdout);
    let lines: Vec<&str> = oneline_str.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].ends_with(" second commit"));
    assert!(lines[1].ends_with(" initial commit"));

    // Verify repository object graph with real git fsck --full.
    let fsck = Command::new("git")
        .args(["fsck", "--full"])
        .current_dir(&sandbox)
        .env("GIT_DIR", sandbox.join(".girst"))
        .output()
        .expect("execute git fsck");
    assert!(
        fsck.status.success(),
        "git fsck --full failed: {}",
        String::from_utf8_lossy(&fsck.stderr)
    );

    let _ = fs::remove_dir_all(&sandbox);
}
