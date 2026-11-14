//! CLI entry point for `girst`.

use std::{
    env,
    io::{self, Read},
    path::PathBuf,
    process::ExitCode,
};

use girst::{
    data_model::{
        author::Author, blob::Blob, commit::Commit, database::Database, object::GitObject,
        refs::Refs, tree::Tree, workspace::Workspace,
    },
    girst_init,
};

/// Parses CLI arguments, runs the requested command, and returns an exit code.
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

/// Dispatches `init` and `commit` subcommands from process arguments.
fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();
    let command = args
        .get(1)
        .map(String::as_str)
        .ok_or_else(|| "Usage: girst <init|commit> [options]".to_string())?;

    match command {
        "init" => run_init(&args[2..]),
        "commit" => run_commit(&args[2..]),
        _ => Err(format!("'{command}' is not a girst command.")),
    }
}

/// Executes `girst init [path]`, defaulting to `"."` when no path is given.
fn run_init(args: &[String]) -> Result<(), String> {
    if args.len() > 1 {
        return Err("Usage: girst init [path]".to_string());
    }
    let working_dir = args.first().map(String::as_str);
    let girst_path =
        girst_init::girst_init(working_dir).map_err(|e| format!("Error initializing .girst: {e}"))?;
    println!(
        "Initialized empty Girst repository in {}",
        girst_path.display()
    );
    Ok(())
}

/// Parses `commit` arguments (`-m <msg>` or stdin fallback) and returns the commit message.
fn parse_commit_message(args: &[String]) -> Result<String, String> {
    let message = match args {
        [] => {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("Failed reading commit message from stdin: {e}"))?;
            buf
        }
        [flag, msg] if flag == "-m" => msg.clone(),
        [flag] if flag == "-m" => {
            return Err("Option '-m' requires a commit message argument.".to_string());
        }
        _ => return Err("Usage: girst commit [-m <msg>]".to_string()),
    };

    if message.trim().is_empty() {
        return Err("Aborting commit due to empty commit message.".to_string());
    }
    Ok(message)
}

/// Executes `girst commit [-m <msg>]` in the current working directory.
fn run_commit(args: &[String]) -> Result<(), String> {
    let root_dir = PathBuf::from(".");
    let girst_path = root_dir.join(".girst");
    if !girst_path.is_dir() {
        return Err("Not a girst repository (or any of the parent directories): .girst".to_string());
    }

    let message = parse_commit_message(args)?;
    let workspace = Workspace::new(&root_dir);
    let db = Database::new(girst_path.join("objects"));
    let refs = Refs::new(&girst_path);

    let file_paths = workspace
        .list_files()
        .map_err(|e| format!("Failed listing workspace files: {e}"))?;

    let mut tree_items = Vec::with_capacity(file_paths.len());
    for rel_path in file_paths {
        let data = workspace
            .read_file(&rel_path)
            .map_err(|e| format!("Failed reading '{}': {e}", rel_path.display()))?;
        let mode = workspace
            .stat_mode(&rel_path)
            .map_err(|e| format!("Failed reading mode for '{}': {e}", rel_path.display()))?;

        let mut blob = Blob::new(data);
        db.store(&mut blob)
            .map_err(|e| format!("Failed storing blob for '{}': {e}", rel_path.display()))?;
        let oid = blob
            .oid()
            .ok_or_else(|| "Blob OID missing after store".to_string())?
            .to_string();
        tree_items.push((rel_path, oid, mode.to_string()));
    }

    let tree = Tree::build(tree_items, &db).map_err(|e| format!("Failed storing tree: {e}"))?;
    let tree_oid = tree
        .oid()
        .ok_or_else(|| "Tree OID missing after build".to_string())?
        .to_string();

    let parent = refs
        .read_head()
        .map_err(|e| format!("Failed reading HEAD: {e}"))?;
    let author = Author::from_env();
    let mut commit = Commit::new(tree_oid, parent.clone(), &author, &message);
    db.store(&mut commit)
        .map_err(|e| format!("Failed storing commit: {e}"))?;

    let commit_oid = commit
        .oid()
        .ok_or_else(|| "Commit OID missing after store".to_string())?;
    refs.update_head(commit_oid)
        .map_err(|e| format!("Failed updating HEAD: {e}"))?;

    let short_oid = &commit_oid[..7];
    let first_line = message.lines().next().unwrap_or("");
    if parent.is_none() {
        println!("[(root-commit) {short_oid}] {first_line}");
    } else {
        println!("[{short_oid}] {first_line}");
    }
    Ok(())
}
