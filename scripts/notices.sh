#!/bin/sh
# Writes THIRD-PARTY-NOTICES.txt for a package: the licenses of the Rust crates (cargo-about,
# configured by about.toml and about.hbs) followed by the components cargo-about cannot see.
#
# usage: scripts/notices.sh windows|linux|macos|extension OUTPUT
#   extension: the browser extension alone, which has no Rust code; its notices are the
#   in-browser engine's.
set -eu
platform=$1
output=$2
cd "$(dirname "$0")/.."
if [ "$platform" = extension ]; then
    {
        echo "Third-party components"
        echo "======================"
        echo
        cat licenses/llama.cpp.txt
        echo
        cat licenses/wllama.txt
    } > "$output"
    exit 0
fi
if ! cargo about --version >/dev/null 2>&1; then
    echo "cargo-about is required to generate third-party notices." >&2
    echo "Install it with: cargo install cargo-about --locked (macOS: brew install cargo-about)" >&2
    exit 1
fi
# about.toml lists the Windows and Linux targets; the macOS package has its own crate set.
set --
if [ "$platform" = macos ]; then
    set -- --target aarch64-apple-darwin
fi
{
    cargo about generate --locked "$@" about.hbs
    echo "Other components"
    echo "================"
    echo
    cat licenses/llama.cpp.txt
    echo
    cat licenses/wllama.txt
    echo
    cat licenses/lucide.txt
    if [ "$platform" = windows ]; then
        echo
        cat licenses/msvc-runtime.txt
    fi
} > "$output"
