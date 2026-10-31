#!/bin/sh
# Check a release tag against every place the version is written down, so
# a release can't ship with packages that disagree about what they are.
#
#   ci/check-release-version.sh vX.Y.Z[-suffix]
#
# Prints `version=X.Y.Z` and `prerelease=true|false` for $GITHUB_OUTPUT.
# A suffix (v0.1.0-test.1, v0.2.0-rc.1) marks a trial or prerelease build.
set -eu

tag=${1:?usage: $0 vX.Y.Z[-suffix]}
top=$(git rev-parse --show-toplevel)
cd "$top"

if ! printf '%s\n' "$tag" | grep -Eqx 'v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?'; then
    echo "error: tag '$tag' is not vX.Y.Z or vX.Y.Z-suffix" >&2
    exit 1
fi
version=${tag#v}
version=${version%%-*}
case $tag in
    *-*) prerelease=true ;;
    *) prerelease=false ;;
esac

failed=0
check() {
    # check WHERE FOUND
    if [ "$2" != "$version" ]; then
        echo "error: $1 says '$2', the tag says '$version'" >&2
        failed=1
    fi
}

check Cargo.toml "$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
check CMakeLists.txt "$(sed -n 's/^project(kadence VERSION \([^ ]*\) .*/\1/p' CMakeLists.txt)"
check packaging/arch/release/PKGBUILD "$(sed -n 's/^pkgver=//p' packaging/arch/release/PKGBUILD)"
check packaging/fedora/kadence.spec "$(sed -n 's/^Version: *//p' packaging/fedora/kadence.spec)"
# The newest release in the metainfo comes first.
check data/io.github.octodillo.daw.metainfo.xml \
    "$(sed -n 's/.*<release version="\([^"]*\)".*/\1/p' data/io.github.octodillo.daw.metainfo.xml | head -n1)"
# The Fedora changelog's newest entry ends with VERSION-RELEASE.
check "the newest %changelog entry in kadence.spec" \
    "$(sed -n '/^%changelog/,$s/^\* .* - \([^-]*\)-[^-]*$/\1/p' packaging/fedora/kadence.spec | head -n1)"
if [ ! -f "docs/releases/$version.md" ]; then
    echo "error: docs/releases/$version.md (the release notes) is missing" >&2
    failed=1
fi

if [ "$failed" -ne 0 ]; then
    exit 1
fi
echo "version=$version"
echo "prerelease=$prerelease"
