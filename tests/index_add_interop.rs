//! End-to-end integration test for `girst add`, index interoperability, and index-backed `girst commit`.

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

/// Returns a unique temporary directory path for isolating a test repository.
fn temp_sandbox_dir() -> PathBuf {
    std::env::temp_dir().join(format!("girst_add_interop_{}", Uuid::new_v4()))
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

/// Runs `git` against `.girst` metadata and `.girst/index` inside `cwd`.
fn run_git_with_index(cwd: &Path, args: &[&str]) -> Output {
    let head_path = cwd.join(".girst/HEAD");
    let created_temp_head = if !head_path.exists() {
        // Git requires HEAD to recognize a GIT_DIR; remove it afterward so girst sees an unborn repo.
        fs::write(&head_path, b"ref: refs/heads/main\n").unwrap();
        true
    } else {
        false
    };

    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_DIR", ".girst")
        .env("GIT_INDEX_FILE", ".girst/index")
        .output()
        .expect("execute git");

    if created_temp_head {
        let _ = fs::remove_file(&head_path);
    }
    output
}

#[test]
fn add_index_interop_conflict_replacement_and_selective_commit() {
    let sandbox = temp_sandbox_dir();
    let repo = sandbox.join("repo");
    fs::create_dir_all(&repo).unwrap();
    let outside = sandbox.join("outside.txt");
    fs::write(&outside, b"outside\n").unwrap();

    let init_out = run_girst(&repo, &["init"]);
    assert!(init_out.status.success(), "girst init failed");

    // 1. Empty index commit and invalid add paths fail cleanly without leaving index.lock.
    let empty_commit = run_girst(&repo, &["commit", "-m", "should fail"]);
    assert!(!empty_commit.status.success());
    assert!(
        String::from_utf8_lossy(&empty_commit.stderr).contains("nothing to commit"),
        "expected 'nothing to commit', got: {}",
        String::from_utf8_lossy(&empty_commit.stderr)
    );

    let missing_add = run_girst(&repo, &["add", "missing.txt"]);
    assert!(!missing_add.status.success());
    assert!(!repo.join(".girst/index.lock").exists());

    let outside_add = run_girst(&repo, &["add", "../outside.txt"]);
    assert!(!outside_add.status.success());
    assert!(!repo.join(".girst/index.lock").exists());

    // 2. Selective staging: regular file + nested directory with 0o755 executable, leaving untracked.txt unstaged.
    fs::create_dir_all(repo.join("bin")).unwrap();
    fs::write(repo.join("README.md"), b"hello\n").unwrap();
    let script = repo.join("bin/tool.sh");
    fs::write(&script, b"#!/bin/sh\necho hi\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(repo.join("untracked.txt"), b"ignore me\n").unwrap();
    fs::write(repo.join("conflict"), b"file version\n").unwrap();

    let add_out = run_girst(&repo, &["add", "README.md", "bin", "conflict"]);
    assert!(
        add_out.status.success(),
        "girst add failed: {}",
        String::from_utf8_lossy(&add_out.stderr)
    );
    assert!(!repo.join(".girst/index.lock").exists());

    // 3. Verify Git reads .girst/index via git ls-files --stage.
    let ls_stage_1 = run_git_with_index(&repo, &["ls-files", "--stage"]);
    assert!(
        ls_stage_1.status.success(),
        "git ls-files --stage failed: {}",
        String::from_utf8_lossy(&ls_stage_1.stderr)
    );
    let stage_1_str = String::from_utf8_lossy(&ls_stage_1.stdout);
    assert!(stage_1_str.contains("100644 ") && stage_1_str.contains(" 0\tREADME.md"));
    assert!(stage_1_str.contains("100755 ") && stage_1_str.contains(" 0\tbin/tool.sh"));
    assert!(stage_1_str.contains("100644 ") && stage_1_str.contains(" 0\tconflict"));
    assert!(!stage_1_str.contains("untracked.txt"));

    // 4. Replace file `conflict` with directory `conflict/nested.txt` and verify via git ls-files --stage.
    fs::remove_file(repo.join("conflict")).unwrap();
    fs::create_dir_all(repo.join("conflict")).unwrap();
    fs::write(repo.join("conflict/nested.txt"), b"nested version\n").unwrap();

    let add_conflict = run_girst(&repo, &["add", "conflict/nested.txt"]);
    assert!(
        add_conflict.status.success(),
        "girst add conflict/nested.txt failed: {}",
        String::from_utf8_lossy(&add_conflict.stderr)
    );
    let ls_stage_2 = run_git_with_index(&repo, &["ls-files", "--stage"]);
    assert!(ls_stage_2.status.success());
    let stage_2_str = String::from_utf8_lossy(&ls_stage_2.stdout);
    assert!(stage_2_str.contains("100644 ") && stage_2_str.contains(" 0\tconflict/nested.txt"));
    assert!(!stage_2_str.contains("\tconflict\n"));

    // 5. Index-backed commit omits untracked.txt, preserves 100755, and passes git fsck --full.
    let commit_out = run_girst(&repo, &["commit", "-m", "staged commit"]);
    assert!(
        commit_out.status.success(),
        "girst commit failed: {}",
        String::from_utf8_lossy(&commit_out.stderr)
    );

    let ls_tree = run_git_with_index(&repo, &["ls-tree", "-r", "HEAD"]);
    assert!(ls_tree.status.success());
    let tree_str = String::from_utf8_lossy(&ls_tree.stdout);
    assert!(tree_str.contains("100644 blob ") && tree_str.contains("\tREADME.md"));
    assert!(tree_str.contains("100755 blob ") && tree_str.contains("\tbin/tool.sh"));
    assert!(tree_str.contains("100644 blob ") && tree_str.contains("\tconflict/nested.txt"));
    assert!(!tree_str.contains("untracked.txt"));

    let fsck = run_git_with_index(&repo, &["fsck", "--full"]);
    assert!(
        fsck.status.success(),
        "git fsck --full failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&fsck.stdout),
        String::from_utf8_lossy(&fsck.stderr)
    );

    let _ = fs::remove_dir_all(&sandbox);
}
