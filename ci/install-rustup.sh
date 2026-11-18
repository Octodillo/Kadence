#!/bin/sh
# Install Rust with a pinned rustup-init, checked against its sha256, rather
# than curl | sh. For the distro containers whose own Rust is too old or
# missing; arguments go to rustup-init.
#
#   ci/install-rustup.sh [--component clippy ...]
set -eu

version=1.29.2
sha256=5be191e8c70a5007074254cb74d78dbfdd25cd5a61c27de7b8e5410122ec24f4

# rustup-init decides what to do from the name it is run under, so it keeps
# its name.
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
init=$dir/rustup-init
curl -fsSL -o "$init" \
    "https://static.rust-lang.org/rustup/archive/$version/x86_64-unknown-linux-gnu/rustup-init"
echo "$sha256  $init" | sha256sum -c -
chmod +x "$init"
"$init" -y --profile minimal "$@"
if [ -n "${GITHUB_PATH:-}" ]; then
    echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"
fi
