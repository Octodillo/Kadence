#!/bin/bash
# Build, lint, install and smoke-test an Arch package, as root in an Arch
# container that has base-devel, namcap, sudo and the smoke test's display
# servers. The package files are left in OUT, with the PKGBUILD they came from.
#
#   ci/arch-package.sh git OUT              kadence-git, from this checkout
#   ci/arch-package.sh release OUT TARBALL  kadence, from a make-dist.sh tarball
#   ci/arch-package.sh lint FILE...         only namcap, on a PKGBUILD or package
set -euo pipefail

# namcap exits 0 whatever it finds, so fail on its error lines.
lint() {
    local log
    log=$(mktemp)
    namcap "$@" | tee "$log"
    ! grep -q ' E: ' "$log"
}
if [ "${1:-}" = lint ]; then
    shift
    lint "$@"
    exit
fi

usage() {
    echo "usage: $0 git OUT | release OUT TARBALL | lint FILE..." >&2
    exit 2
}
[ $# -ge 2 ] || usage
kind=$1
out=$(mkdir -p "$2" && cd "$2" && pwd)
top=$(cd "$(dirname "$0")/.." && pwd)

id builder > /dev/null 2>&1 || useradd -m builder
echo 'builder ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/builder
pkg=/home/builder/pkg
rm -rf "$pkg"
mkdir "$pkg"

case $kind in
    git)
        [ $# -eq 2 ] || usage
        git config --system --add safe.directory '*'
        cp "$top/packaging/arch/PKGBUILD" "$pkg/"
        KADENCE_SOURCE="git+file://$top#commit=$(git -C "$top" rev-parse HEAD)"
        export KADENCE_SOURCE
        ;;
    release)
        [ $# -eq 3 ] || usage
        tarball=$3
        version=$(basename "$tarball" .tar.gz)
        version=${version#kadence-}
        # The tarball sits next to the PKGBUILD, so makepkg uses it instead
        # of downloading the (not yet published) release asset.
        cp "$tarball" "$pkg/"
        "$top/packaging/arch/release-pkgbuild.sh" "$version" "$tarball" > "$pkg/PKGBUILD"
        ;;
    *) usage ;;
esac
chown -R builder "$pkg"

cd "$pkg"
sudo -u builder --preserve-env=KADENCE_SOURCE makepkg --syncdeps --noconfirm
lint PKGBUILD kadence-*.pkg.tar.zst
pacman -U --noconfirm kadence-*.pkg.tar.zst
(cd "$top" && sudo -u builder env REQUIRE_ALL=1 ci/smoke-matrix.sh /usr/bin/kadence)
cp PKGBUILD kadence-*.pkg.tar.zst "$out/"
