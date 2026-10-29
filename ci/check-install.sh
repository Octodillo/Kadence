#!/bin/sh
# Configure, build and install with CMake into a staging directory, then
# check that the desktop integration files land where Plasma looks and pass
# the freedesktop validators.
set -eu

build=${BUILD_DIR:-build-ci}
mkdir -p "$build"
build=$(cd "$build" && pwd)
stage=$build/stage
id=io.github.octodillo.daw

cmake -S . -B "$build" -DCMAKE_INSTALL_PREFIX=/usr
cmake --build "$build"
DESTDIR="$stage" cmake --install "$build"

for f in \
    usr/share/applications/$id.desktop \
    usr/share/metainfo/$id.metainfo.xml \
    usr/share/mime/packages/$id.mime.xml \
    usr/share/icons/hicolor/scalable/apps/$id.svg
do
    test -f "$stage/$f" || { echo "missing: $f"; exit 1; }
    echo "ok: $f"
done

desktop-file-validate "$stage/usr/share/applications/$id.desktop"
appstreamcli validate --no-net --pedantic "$stage/usr/share/metainfo/$id.metainfo.xml"
