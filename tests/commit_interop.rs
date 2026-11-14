//! End-to-end integration test verifying `girst` commit interoperability with `git fsck`.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
};

use uuid::Uuid;

fn girst_bin() -> &'static str {
    env!("CARGO_BIN_EXE_girst")
}

fn temp_repo_dir() -> PathBuf {
    std::env::temp_dir().join(format!("girst_interop_test_{}", Uuid::new_v4()))
}

#[test]
fn init_two_commits_parent_chain_and_git_fsck_full() {
    let repo = temp_repo_dir();
    fs::create_dir_all(&repo).unwrap();

    let init_out = Command::new(girst_bin())
        .arg("init")
        .current_dir(&repo)
        .output()
        .expect("run girst init");
    assert!(
        init_out.status.success(),
        "girst init failed: {}",
        String::from_utf8_lossy(&init_out.stderr)
    );
    assert!(repo.join(".girst/objects").is_dir());
    assert!(repo.join(".girst/refs/heads").is_dir());

    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(repo.join("bin")).unwrap();
    fs::write(repo.join("README.md"), b"# girst\n").unwrap();
    fs::write(repo.join("src/lib.rs"), b"pub fn v() -> u8 { 1 }\n").unwrap();
    let script_path = repo.join("bin/run.sh");
    fs::write(&script_path, b"#!/bin/sh\necho ok\n").unwrap();
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

    let c1_out = Command::new(girst_bin())
        .args(["commit", "-m", "initial commit"])
        .current_dir(&repo)
        .env("GIT_AUTHOR_NAME", "Tester")
        .env("GIT_AUTHOR_EMAIL", "tester@example.com")
        .output()
        .expect("run first girst commit");
    assert!(
        c1_out.status.success(),
        "first commit failed: {}",
        String::from_utf8_lossy(&c1_out.stderr)
    );
    let c1_stdout = String::from_utf8_lossy(&c1_out.stdout);
    assert!(
        c1_stdout.starts_with("[(root-commit) ") && c1_stdout.trim_end().ends_with("] initial commit"),
        "unexpected first commit stdout: {c1_stdout}"
    );
    let oid1 = fs::read_to_string(repo.join(".girst/HEAD"))
        .expect("read HEAD after c1")
        .trim()
        .to_string();

    fs::create_dir_all(repo.join("docs")).unwrap();
    fs::write(repo.join("docs/guide.md"), b"guide\n").unwrap();
    fs::write(repo.join("src/lib.rs"), b"pub fn v() -> u8 { 2 }\n").unwrap();

    let c2_out = Command::new(girst_bin())
        .args(["commit", "-m", "second commit"])
        .current_dir(&repo)
        .env("GIT_AUTHOR_NAME", "Tester")
        .env("GIT_AUTHOR_EMAIL", "tester@example.com")
        .output()
        .expect("run second girst commit");
    assert!(
        c2_out.status.success(),
        "second commit failed: {}",
        String::from_utf8_lossy(&c2_out.stderr)
    );
    let c2_stdout = String::from_utf8_lossy(&c2_out.stdout);
    assert!(
        c2_stdout.starts_with('[')
            && !c2_stdout.contains("(root-commit)")
            && c2_stdout.trim_end().ends_with("] second commit"),
        "unexpected second commit stdout: {c2_stdout}"
    );
    let oid2 = fs::read_to_string(repo.join(".girst/HEAD"))
        .expect("read HEAD after c2")
        .trim()
        .to_string();
    assert_ne!(oid1, oid2);

    let parent_out = Command::new("git")
        .args(["rev-parse", "HEAD^"])
        .current_dir(&repo)
        .env("GIT_DIR", ".girst")
        .output()
        .expect("run git rev-parse HEAD^");
    assert!(parent_out.status.success(), "git rev-parse HEAD^ failed");
    assert_eq!(
        String::from_utf8_lossy(&parent_out.stdout).trim(),
        oid1,
        "commit 2 parent should equal commit 1"
    );

    let ls_tree_out = Command::new("git")
        .args(["ls-tree", "-r", "HEAD"])
        .current_dir(&repo)
        .env("GIT_DIR", ".girst")
        .output()
        .expect("run git ls-tree");
    assert!(ls_tree_out.status.success(), "git ls-tree failed");
    let ls_tree_str = String::from_utf8_lossy(&ls_tree_out.stdout);
    assert!(
        ls_tree_str.contains("100755 blob ") && ls_tree_str.contains("\tbin/run.sh"),
        "expected executable mode 100755 for bin/run.sh, got:\n{ls_tree_str}"
    );

    let fsck_out = Command::new("git")
        .args(["fsck", "--full"])
        .current_dir(&repo)
        .env("GIT_DIR", ".girst")
        .output()
        .expect("run git fsck --full");
    assert!(
        fsck_out.status.success(),
        "git fsck --full failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&fsck_out.stdout),
        String::from_utf8_lossy(&fsck_out.stderr)
    );

    let _ = fs::remove_dir_all(&repo);
}
