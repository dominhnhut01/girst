# girst

A Git implementation in Rust following *Building Git*. It stores repository data under `.girst/` using standard Git loose objects, a `DIRC` v2 index, and refs so native `git` tools can inspect it directly.

## Quick Try

**One-command live demo** (builds `girst`, runs a full workflow in a temp directory, and verifies with `git`):

```bash
./scripts/demo.sh
```

**Interactive playground:**

```bash
cargo build --release
export PATH="$PWD/target/release:$PATH"
mkdir /tmp/girst-play && cd /tmp/girst-play
girst init
echo "hello" > hello.txt
girst add hello.txt
girst commit -m "Initial commit"
echo "world" >> hello.txt
girst diff
girst add hello.txt && girst commit -m "Second commit"
girst checkout -b feature HEAD^
girst log --oneline
```

**Run tests:**

```bash
cargo test
```

## Commands

| Command | Syntax | Description |
| --- | --- | --- |
| `init` | `girst init [path]` | Initialize a `.girst/` repository (defaults to `.`) |
| `add` | `girst add <paths...>` | Stage files or directories into `.girst/index` |
| `commit` | `girst commit [-m <msg>]` | Create a commit from staged entries (`-m` or stdin) |
| `status` | `girst status [--porcelain]` | Show branch, staged, unstaged, and untracked changes |
| `diff` | `girst diff [--cached\|--staged]` | Show workspace vs. index diff, or index vs. `HEAD` |
| `log` | `girst log [--oneline] [<rev>]` | Print commit history from `HEAD` or `<rev>` |
| `branch` | `girst branch [-d <name> \| <name> [<start-rev>]]` | List, create, or delete branches |
| `checkout` | `girst checkout [-b <new-branch> [<start-rev>] \| <target>]` | Switch branches/revisions or create and switch |

## Git Compatibility

Repositories created by `girst` use Git's on-disk format and can be verified directly with `git`:

```bash
GIT_DIR=.girst git fsck --full
GIT_DIR=.girst git log -p
GIT_DIR=.girst GIT_INDEX_FILE=.girst/index GIT_WORK_TREE=. git status --porcelain
```

## Project Layout

```text
src/
  main.rs                 # CLI entry point and subcommand dispatch
  lib.rs                  # Library root
  girst_init.rs           # Repository initialization (.girst/)
  data_model/             # Git objects, index, refs, diff, log, checkout
    author.rs, blob.rs, checkout.rs, commit.rs, database.rs,
    diff.rs, entry.rs, index.rs, lockfile.rs, log.rs,
    object.rs, refs.rs, revision.rs, status.rs, tree.rs, workspace.rs
tests/
  index_add_interop.rs           # Index & add interop tests with git
  commit_interop.rs              # Commit & tree interop tests with git
  inspect_diff_log_interop.rs    # Status, diff, and log interop tests
  branch_checkout_interop.rs     # Branch & checkout interop tests
```
