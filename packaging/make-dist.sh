#!/bin/sh
# Make a release tarball that builds without network access: the git tree
# at HEAD plus every crate from Cargo.lock vendored into vendor/, with a
# .cargo/config.toml that points Cargo there. Distro packages (the Fedora
# spec, or a PKGBUILD for a release) build from this.
#
#   packaging/make-dist.sh [OUTDIR]   -> OUTDIR/kadence-VERSION.tar.gz
#
# OUTDIR defaults to dist/ at the top of the repository.
set -eu

umask 022
top=$(git rev-parse --show-toplevel)
out=$(mkdir -p "${1:-$top/dist}" && cd "${1:-$top/dist}" && pwd)
cd "$top"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
name=kadence-$version
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git archive --format=tar --prefix="$name/" HEAD | tar -x -C "$work"
mkdir -p "$work/$name/.cargo"
(cd "$work/$name" && cargo vendor --quiet --locked --versioned-dirs vendor)
cat > "$work/$name/.cargo/config.toml" <<'TOML'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
TOML
tar -C "$work" --sort=name --owner=0 --group=0 --numeric-owner --mode=go-w \
    --mtime="@$(git log -1 --format=%ct)" -czf "$out/$name.tar.gz" "$name"
echo "$out/$name.tar.gz"
