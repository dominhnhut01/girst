#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cargo build --quiet --manifest-path "$ROOT_DIR/Cargo.toml" ${CARGO_FLAGS:-}
GIRST="$ROOT_DIR/target/debug/girst"

DEMO_DIR="$(mktemp -d)"
trap 'rm -rf "$DEMO_DIR"' EXIT
cd "$DEMO_DIR"

step() { printf "\n==> %s\n" "$*"; }

step "girst init"
"$GIRST" init

step "create files & girst add"
mkdir -p src
printf "hello girst\n" > README.txt
printf "fn main() {}\n" > src/app.rs
"$GIRST" add README.txt src/app.rs

step "girst commit -m"
"$GIRST" commit -m "Initial commit"

step "edit file, status --porcelain, diff"
printf "hello girst\nsecond line\n" > README.txt
"$GIRST" status --porcelain
"$GIRST" diff

step "stage & commit update"
"$GIRST" add README.txt
"$GIRST" commit -m "Update README"

step "checkout -b feature HEAD^ & branch"
"$GIRST" checkout -b feature HEAD^
"$GIRST" branch

step "checkout main & log --oneline"
"$GIRST" checkout main
"$GIRST" log --oneline

step "verify with native git"
GIT_DIR=.girst git fsck --full
GIT_DIR=.girst GIT_INDEX_FILE=.girst/index GIT_WORK_TREE=. git status --porcelain
