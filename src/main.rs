//! CLI entry point for `girst`.

use std::{
    env,
    io::{self, Read},
    path::{Path, PathBuf},
    process::ExitCode,
};

use girst::{
    data_model::{
        author::Author,
        blob::Blob,
        checkout::{checkout, checkout_new_branch},
        commit::Commit,
        database::Database,
        diff::{diff_cached, diff_workspace},
        index::{hex_to_oid, Index, IndexEntry},
        log::{collect_log, collect_log_from, format_log, format_oneline},
        object::GitObject,
        refs::Refs,
        revision::resolve_revision,
        status::StatusReport,
        tree::Tree,
        workspace::Workspace,
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

/// Dispatches `init`, `add`, `commit`, `status`, `diff`, `log`, `branch`, and `checkout` subcommands.
fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();
    let command = args
        .get(1)
        .map(String::as_str)
        .ok_or_else(|| {
            "Usage: girst <init|add|commit|status|diff|log|branch|checkout> [options]".to_string()
        })?;

    match command {
        "init" => run_init(&args[2..]),
        "add" => run_add(&args[2..]),
        "commit" => run_commit(&args[2..]),
        "status" => run_status(&args[2..]),
        "diff" => run_diff(&args[2..]),
        "log" => run_log(&args[2..]),
        "branch" => run_branch(&args[2..]),
        "checkout" => run_checkout(&args[2..]),
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

/// Executes `girst add <paths...>` in the current working directory.
fn run_add(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("Usage: girst add <paths...>".to_string());
    }

    let root_dir = PathBuf::from(".");
    let girst_path = root_dir.join(".girst");
    if !girst_path.is_dir() {
        return Err("Not a girst repository (or any of the parent directories): .girst".to_string());
    }

    let workspace = Workspace::new(&root_dir);
    let db = Database::new(girst_path.join("objects"));
    let mut index = Index::new(girst_path.join("index"));
    index
        .load_for_update()
        .map_err(|e| format!("Failed locking index: {e}"))?;

    // Expand and validate all CLI paths before mutating blobs or committing the index.
    let mut expanded_paths = Vec::new();
    for arg in args {
        let paths = workspace
            .expand_path(Path::new(arg))
            .map_err(|e| format!("Failed expanding '{arg}': {e}"))?;
        expanded_paths.extend(paths);
    }

    for rel_str in expanded_paths {
        let rel_path = Path::new(&rel_str);
        let data = workspace
            .read_file(rel_path)
            .map_err(|e| format!("Failed reading '{rel_str}': {e}"))?;
        let meta = workspace
            .stat_file(rel_path)
            .map_err(|e| format!("Failed stating '{rel_str}': {e}"))?;

        let mut blob = Blob::new(data);
        db.store(&mut blob)
            .map_err(|e| format!("Failed storing blob for '{rel_str}': {e}"))?;
        let oid_hex = blob
            .oid()
            .ok_or_else(|| "Blob OID missing after store".to_string())?;
        let oid = hex_to_oid(oid_hex)
            .map_err(|e| format!("Invalid blob OID '{oid_hex}': {e}"))?;

        index.add(IndexEntry::from_metadata(rel_str, oid, &meta));
    }

    index
        .write_updates()
        .map_err(|e| format!("Failed writing index: {e}"))?;
    Ok(())
}

/// Executes `girst commit [-m <msg>]` from the staged `.girst/index`.
fn run_commit(args: &[String]) -> Result<(), String> {
    let root_dir = PathBuf::from(".");
    let girst_path = root_dir.join(".girst");
    if !girst_path.is_dir() {
        return Err("Not a girst repository (or any of the parent directories): .girst".to_string());
    }

    let message = parse_commit_message(args)?;
    let db = Database::new(girst_path.join("objects"));
    let refs = Refs::new(&girst_path);
    let mut index = Index::new(girst_path.join("index"));
    index
        .load()
        .map_err(|e| format!("Failed loading index: {e}"))?;

    if index.is_empty() {
        return Err("nothing to commit".to_string());
    }

    let mut tree_items = Vec::with_capacity(index.len());
    for entry in index.entries() {
        let oid = hex::encode(entry.oid);
        let mode = if entry.mode == 0o100755 {
            "100755"
        } else {
            "100644"
        };
        tree_items.push((PathBuf::from(&entry.path), oid, mode.to_string()));
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

/// Executes `girst status [--porcelain]` in the current working directory.
fn run_status(args: &[String]) -> Result<(), String> {
    let porcelain = match args {
        [] => false,
        [flag] if flag == "--porcelain" => true,
        _ => return Err("Usage: girst status [--porcelain]".to_string()),
    };

    let root_dir = PathBuf::from(".");
    let girst_path = root_dir.join(".girst");
    if !girst_path.is_dir() {
        return Err("Not a girst repository (or any of the parent directories): .girst".to_string());
    }

    let workspace = Workspace::new(&root_dir);
    let db = Database::new(girst_path.join("objects"));
    let refs = Refs::new(&girst_path);
    let mut index = Index::new(girst_path.join("index"));
    index
        .load()
        .map_err(|e| format!("Failed loading index: {e}"))?;

    let report = StatusReport::collect(&workspace, &db, &index, &refs)
        .map_err(|e| format!("Failed collecting status: {e}"))?;
    let output = if porcelain {
        report.format_porcelain()
    } else {
        report.format_human()
    };
    print!("{output}");
    Ok(())
}

/// Executes `girst diff [--cached|--staged]` in the current working directory.
fn run_diff(args: &[String]) -> Result<(), String> {
    let cached = match args {
        [] => false,
        [flag] if flag == "--cached" || flag == "--staged" => true,
        _ => return Err("Usage: girst diff [--cached|--staged]".to_string()),
    };

    let root_dir = PathBuf::from(".");
    let girst_path = root_dir.join(".girst");
    if !girst_path.is_dir() {
        return Err("Not a girst repository (or any of the parent directories): .girst".to_string());
    }

    let workspace = Workspace::new(&root_dir);
    let db = Database::new(girst_path.join("objects"));
    let refs = Refs::new(&girst_path);
    let mut index = Index::new(girst_path.join("index"));
    index
        .load()
        .map_err(|e| format!("Failed loading index: {e}"))?;

    let output = if cached {
        diff_cached(&db, &index, &refs)
    } else {
        diff_workspace(&workspace, &db, &index)
    }
    .map_err(|e| format!("Failed computing diff: {e}"))?;

    print!("{output}");
    Ok(())
}

/// Executes `girst log [--oneline] [<rev>]` in the current working directory.
fn run_log(args: &[String]) -> Result<(), String> {
    let mut oneline = false;
    let mut rev: Option<&str> = None;
    for arg in args {
        if arg == "--oneline" && !oneline {
            oneline = true;
        } else if arg.starts_with('-') || rev.is_some() {
            return Err("Usage: girst log [--oneline] [<rev>]".to_string());
        } else {
            rev = Some(arg.as_str());
        }
    }

    let root_dir = PathBuf::from(".");
    let girst_path = root_dir.join(".girst");
    if !girst_path.is_dir() {
        return Err("Not a girst repository (or any of the parent directories): .girst".to_string());
    }

    let db = Database::new(girst_path.join("objects"));
    let refs = Refs::new(&girst_path);
    let commits = match rev {
        None => collect_log(&db, &refs),
        Some(expr) => {
            let start_oid = resolve_revision(&db, &refs, expr)
                .map_err(|e| format!("Failed resolving '{expr}': {e}"))?;
            collect_log_from(&db, Some(start_oid))
        }
    }
    .map_err(|e| format!("Failed reading log: {e}"))?;

    let output = if oneline {
        format_oneline(&commits)
    } else {
        format_log(&commits)
    };
    print!("{output}");
    Ok(())
}

/// Executes `girst branch [-d <name> | <name> [<start-rev>]]` in the current working directory.
fn run_branch(args: &[String]) -> Result<(), String> {
    let root_dir = PathBuf::from(".");
    let girst_path = root_dir.join(".girst");
    if !girst_path.is_dir() {
        return Err("Not a girst repository (or any of the parent directories): .girst".to_string());
    }

    let refs = Refs::new(&girst_path);
    match args {
        [] => {
            let branches = refs
                .list_branches()
                .map_err(|e| format!("Failed listing branches: {e}"))?;
            for (name, is_active) in branches {
                if is_active {
                    println!("* {name}");
                } else {
                    println!("  {name}");
                }
            }
            Ok(())
        }
        [flag, name] if flag == "-d" => {
            let deleted_oid = refs
                .delete_branch(name)
                .map_err(|e| format!("Failed deleting branch '{name}': {e}"))?;
            let short_oid = deleted_oid.get(..7).unwrap_or(&deleted_oid);
            println!("Deleted branch {name} (was {short_oid}).");
            Ok(())
        }
        [name] if !name.starts_with('-') => {
            let db = Database::new(girst_path.join("objects"));
            let oid = resolve_revision(&db, &refs, "HEAD")
                .map_err(|e| format!("Failed resolving 'HEAD': {e}"))?;
            refs.create_branch(name, &oid)
                .map_err(|e| format!("Failed creating branch '{name}': {e}"))
        }
        [name, start_rev] if !name.starts_with('-') => {
            let db = Database::new(girst_path.join("objects"));
            let oid = resolve_revision(&db, &refs, start_rev)
                .map_err(|e| format!("Failed resolving '{start_rev}': {e}"))?;
            refs.create_branch(name, &oid)
                .map_err(|e| format!("Failed creating branch '{name}': {e}"))
        }
        _ => Err("Usage: girst branch [-d <name> | <name> [<start-rev>]]".to_string()),
    }
}

/// Executes `girst checkout [-b <new-branch> [<start-rev>] | <target>]` in the current working directory.
fn run_checkout(args: &[String]) -> Result<(), String> {
    let root_dir = PathBuf::from(".");
    let girst_path = root_dir.join(".girst");
    if !girst_path.is_dir() {
        return Err("Not a girst repository (or any of the parent directories): .girst".to_string());
    }

    let workspace = Workspace::new(&root_dir);
    let db = Database::new(girst_path.join("objects"));
    let refs = Refs::new(&girst_path);
    let mut index = Index::new(girst_path.join("index"));

    match args {
        [flag, new_branch] if flag == "-b" && !new_branch.starts_with('-') => {
            checkout_new_branch(&workspace, &db, &refs, &mut index, new_branch, None)
                .map_err(|e| format!("Failed checking out new branch '{new_branch}': {e}"))
        }
        [flag, new_branch, start_rev]
            if flag == "-b" && !new_branch.starts_with('-') && !start_rev.starts_with('-') =>
        {
            checkout_new_branch(&workspace, &db, &refs, &mut index, new_branch, Some(start_rev))
                .map_err(|e| format!("Failed checking out new branch '{new_branch}': {e}"))
        }
        [target] if !target.starts_with('-') => checkout(&workspace, &db, &refs, &mut index, target)
            .map_err(|e| format!("Failed checking out '{target}': {e}")),
        _ => Err("Usage: girst checkout [-b <new-branch> [<start-rev>] | <target>]".to_string()),
    }
}
