#!/bin/sh
set -eu

REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || fail "$2"
}

need cargo "Rust is required. Install rustup from https://rustup.rs, then run this installer again."
need cmake "CMake is required. On macOS: brew install cmake ninja zig@0.15"
need ninja "Ninja is required. On macOS: brew install cmake ninja zig@0.15"

if [ -n "${ZIG:-}" ]; then
    [ -x "$ZIG" ] || fail "ZIG points to a missing or non-executable file: $ZIG"
    zig_command=$ZIG
elif command -v zig >/dev/null 2>&1; then
    zig_command=$(command -v zig)
elif command -v brew >/dev/null 2>&1 && brew_prefix=$(brew --prefix zig@0.15 2>/dev/null); then
    zig_command="$brew_prefix/bin/zig"
    [ -x "$zig_command" ] || fail "Homebrew reports zig@0.15 at $brew_prefix, but its executable is missing. Reinstall it with: brew reinstall zig@0.15"
else
    fail "Zig 0.15.2 is required. On macOS: brew install cmake ninja zig@0.15"
fi

zig_version=$("$zig_command" version 2>/dev/null) || fail "Could not execute Zig at $zig_command"
[ "$zig_version" = "0.15.2" ] || fail "GoWild requires Zig 0.15.2; found $zig_version at $zig_command"

printf 'Installing GoWild from source with Zig %s...\n' "$zig_version"
ZIG=$zig_command cargo install --path "$REPO_ROOT" --locked "$@"
