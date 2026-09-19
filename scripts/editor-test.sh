#!/bin/sh
# Verifies the real text editor (kiro_cmd.rs, `kiro`/`nano`) actually
# works: opens a real file under a real pseudo-terminal, types real
# keystrokes, saves with Ctrl-S, quits with Ctrl-Q, and checks the file
# really changed on disk. Runs directly on the host (no QEMU/root
# needed — editing a file isn't a privileged operation), unlike
# boot-test.sh/login-test.sh.
#
# A real pty, not a plain pipe, is required: kiro-editor needs genuine
# terminal control (raw mode, cursor positioning) the same way
# `stty`/`more` elsewhere in this project do — see scripts/
# editor-test-driver.py's own comments for why a real `TIOCSWINSZ` is
# set on it before launching (kiro-editor otherwise falls back to
# probing the terminal size via an ANSI cursor-position query, which
# nothing here would answer, exactly the same class of issue found
# and fixed in login-test-driver.py for `brush`).
#
# Usage: scripts/editor-test.sh

set -eu

WORKSPACE_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="$WORKSPACE_ROOT/target/release"

if [ ! -x "$TARGET/coreutils-rs" ]; then
    echo "editor-test: $TARGET/coreutils-rs missing — run 'cargo build --release' first" >&2
    exit 1
fi
if ! command -v python3 >/dev/null 2>&1; then
    echo "editor-test: python3 required (drives the pty session)" >&2
    exit 1
fi

python3 "$WORKSPACE_ROOT/scripts/editor-test-driver.py" "$TARGET/coreutils-rs"
