//! End-to-end integration test for `girst branch`, `girst checkout`, `girst log <rev>`, and `git` interop.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use uuid::Uuid;

/// Returns the compiled `girst` binary path.
fn girst_bin() -> &'static str {
    env!("CARGO_BIN_EXE_girst")
}

/// Returns a unique temporary repository directory path.
fn temp_repo_dir() -> PathBuf {
    std::env::temp_dir().join(format!("girst_branch_checkout_{}", Uuid::new_v4()))
}

/// Runs `girst` with `args` inside `cwd`.
fn run_girst(cwd: &Path, args: &[&str]) -> Output {
    Command::new(girst_bin())
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "Tester")
        .env("GIT_AUTHOR_EMAIL", "tester@example.com")
        .output()
        .expect("execute girst")
}

/// Runs `git` against `.girst` metadata, `.girst/index`, and `cwd` worktree.
fn run_git(cwd: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_DIR", ".girst")
        .env("GIT_INDEX_FILE", ".girst/index")
        .env("GIT_WORK_TREE", ".")
        .output()
        .expect("execute git")
}

#[test]
fn branch_checkout_dirty_refusal_detached_head_and_git_interop() {
    let repo = temp_repo_dir();
    fs::create_dir_all(&repo).unwrap();

    // 1. girst init writes symbolic HEAD -> refs/heads/main.
    assert!(run_girst(&repo, &["init"]).status.success());
    assert_eq!(
        fs::read_to_string(repo.join(".girst/HEAD")).unwrap(),
        "ref: refs/heads/main\n"
    );

    // 2. Two commits on main.
    fs::write(repo.join("README.md"), b"main v1\n").unwrap();
    fs::create_dir_all(repo.join("bin")).unwrap();
    let script = repo.join("bin/tool.sh");
    fs::write(&script, b"#!/bin/sh\necho v1\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run_girst(&repo, &["add", "."]).status.success());
    assert!(run_girst(&repo, &["commit", "-m", "main c1"]).status.success());

    fs::write(repo.join("README.md"), b"main v2\n").unwrap();
    assert!(run_girst(&repo, &["add", "README.md"]).status.success());
    assert!(run_girst(&repo, &["commit", "-m", "main c2"]).status.success());

    // 3. Create branches via `girst branch` and `girst checkout -b`, add nested dir + mode change to 0o644.
    assert!(run_girst(&repo, &["branch", "backup", "main^"]).status.success());
    assert!(run_girst(&repo, &["checkout", "-b", "feat/topic"]).status.success());
    fs::create_dir_all(repo.join("nested/sub")).unwrap();
    fs::write(repo.join("nested/sub/info.txt"), b"topic\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(run_girst(&repo, &["add", "."]).status.success());
    assert!(run_girst(&repo, &["commit", "-m", "topic c3"]).status.success());

    let branch_list = String::from_utf8_lossy(&run_girst(&repo, &["branch"]).stdout).into_owned();
    assert_eq!(branch_list, "  backup\n* feat/topic\n  main\n");

    // 4. Dirty-checkout refusal leaves workspace, index, and HEAD untouched.
    fs::write(repo.join("nested/sub/info.txt"), b"dirty edit\n").unwrap();
    let dirty_co = run_girst(&repo, &["checkout", "main"]);
    assert!(!dirty_co.status.success());
    assert_eq!(
        fs::read(repo.join("nested/sub/info.txt")).unwrap(),
        b"dirty edit\n"
    );
    assert_eq!(
        fs::read_to_string(repo.join(".girst/HEAD")).unwrap(),
        "ref: refs/heads/feat/topic\n"
    );

    // Restore clean file, then switch cleanly back to main.
    fs::write(repo.join("nested/sub/info.txt"), b"topic\n").unwrap();
    assert!(run_girst(&repo, &["checkout", "main"]).status.success());
    assert!(!repo.join("nested").exists());
    assert_eq!(fs::read(repo.join("README.md")).unwrap(), b"main v2\n");
    assert_eq!(
        fs::metadata(&script).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let git_status_main = run_git(&repo, &["status", "--porcelain"]);
    assert!(git_status_main.status.success());
    assert_eq!(String::from_utf8_lossy(&git_status_main.stdout), "");

    // 5. Detached checkout at HEAD~1 + girst log <rev> + branch -d & active delete refusal.
    assert!(run_girst(&repo, &["checkout", "HEAD~1"]).status.success());
    assert_eq!(fs::read(repo.join("README.md")).unwrap(), b"main v1\n");
    let head_content = fs::read_to_string(repo.join(".girst/HEAD")).unwrap();
    assert!(!head_content.starts_with("ref: "));
    let git_status_detached = run_git(&repo, &["status", "--porcelain"]);
    assert!(git_status_detached.status.success());
    assert_eq!(String::from_utf8_lossy(&git_status_detached.stdout), "");

    let log_topic = run_girst(&repo, &["log", "--oneline", "feat/topic"]);
    assert!(log_topic.status.success());
    let log_lines: Vec<&str> = std::str::from_utf8(&log_topic.stdout)
        .unwrap()
        .lines()
        .collect();
    assert_eq!(log_lines.len(), 3);
    assert!(log_lines[0].ends_with(" topic c3"));

    assert!(run_girst(&repo, &["checkout", "main"]).status.success());
    assert!(!run_girst(&repo, &["branch", "-d", "main"]).status.success());
    let del_out = run_girst(&repo, &["branch", "-d", "feat/topic"]);
    assert!(del_out.status.success());
    assert!(String::from_utf8_lossy(&del_out.stdout).starts_with("Deleted branch feat/topic (was "));

    let fsck = run_git(&repo, &["fsck", "--full"]);
    assert!(
        fsck.status.success(),
        "git fsck --full failed: {}",
        String::from_utf8_lossy(&fsck.stderr)
    );

    let _ = fs::remove_dir_all(&repo);
}
