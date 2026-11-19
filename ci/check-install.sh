#!/bin/sh
# Configure, build and install with CMake into a staging directory, then
# check that the desktop integration files land where Plasma looks and pass
# the freedesktop validators.
#
# What is checked is the layout, not the optimised binary (the Packaging
# workflow builds that), so this builds the dev profile into the workspace's
# target directory: it reuses the build the tests left there, and leaves
# target/debug/kadence for the screenshots.
set -eu

build=${BUILD_DIR:-build-ci}
mkdir -p "$build"
build=$(cd "$build" && pwd)
stage=$build/stage
id=io.github.octodillo.daw

cmake -S . -B "$build" -DCMAKE_INSTALL_PREFIX=/usr \
    -DKADENCE_CARGO_PROFILE=dev -DKADENCE_CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
cmake --build "$build"
DESTDIR="$stage" cmake --install "$build"

for f in \
    usr/share/applications/$id.desktop \
    usr/share/metainfo/$id.metainfo.xml \
    usr/share/mime/packages/$id.mime.xml \
    usr/share/icons/hicolor/scalable/apps/$id.svg \
    usr/share/licenses/kadence/LICENSE \
    usr/share/licenses/kadence/THIRD-PARTY-LICENSES
do
    test -f "$stage/$f" || { echo "missing: $f"; exit 1; }
    echo "ok: $f"
done

# symphonia is the MPL-2.0 code whose notice must ship with the binary.
grep -q '^symphonia .*: MPL-2.0$' "$stage/usr/share/licenses/kadence/THIRD-PARTY-LICENSES" ||
    { echo "THIRD-PARTY-LICENSES lacks the symphonia notice"; exit 1; }

# The spec's License tag and the PKGBUILD's license array cover what was linked.
packaging/check-license-fields.py "$stage/usr/share/licenses/kadence/THIRD-PARTY-LICENSES"

desktop-file-validate "$stage/usr/share/applications/$id.desktop"
appstreamcli validate --no-net --pedantic "$stage/usr/share/metainfo/$id.metainfo.xml"
