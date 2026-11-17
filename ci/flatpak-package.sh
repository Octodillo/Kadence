#!/bin/sh
# Build the Flatpak, install it and smoke-test it in its sandbox, as root in
# a privileged Fedora container (flatpak-builder sandboxes each build step
# with bubblewrap).
#
#   ci/flatpak-package.sh setup [EXTRA_REF...]   install Flatpak and the runtimes
#   ci/flatpak-package.sh build BUNDLE           build, bundle, install, smoke test
#
# The runtime branches here must match the manifest's.
set -eu

app=io.github.octodillo.daw
top=$(cd "$(dirname "$0")/.." && pwd)
manifest=$top/packaging/flatpak/$app.yml
flathub=https://dl.flathub.org/repo/flathub.flatpakrepo

setup() {
    dnf -y install flatpak flatpak-builder git python3
    flatpak remote-add --if-not-exists flathub "$flathub"
    flatpak install -y --noninteractive flathub \
        org.kde.Sdk//6.11 org.kde.Platform//6.11 \
        org.freedesktop.Sdk.Extension.rust-stable//25.08 \
        org.freedesktop.Sdk.Extension.llvm21//25.08 \
        "$@"
}

# Run the smoke test in the sandbox; arguments are extra `flatpak run` options.
smoke() {
    # Flatpak keeps the app's config in ~/.var/app; start from a clean one.
    rm -rf ~/.var/app/$app
    log=$(mktemp)
    # The exit status that matters is kadence's, so no pipe into tee.
    flatpak run \
        --env=QT_QPA_PLATFORM=offscreen --env=QT_QUICK_BACKEND=software \
        --env=QT_FORCE_STDERR_LOGGING=1 "$@" \
        $app --smoke-test > "$log" 2>&1 || { cat "$log"; exit 1; }
    cat "$log"
    grep -q "smoke test: passed" "$log"
    rm -f "$log"
}

build() {
    bundle=$1
    mkdir -p "$(dirname "$bundle")"
    "$top/packaging/flatpak/cargo-sources.py"
    flatpak-builder --disable-rofiles-fuse --force-clean \
        --state-dir=/tmp/flatpak-state --repo=/tmp/repo /tmp/build "$manifest"
    # --runtime-repo: installing the bundle can then fetch the KDE runtime.
    flatpak build-bundle --runtime-repo="$flathub" /tmp/repo "$bundle" $app
    flatpak install -y --noninteractive "$bundle"

    # Audio needs libjack inside the sandbox; the runtime's comes from PipeWire.
    flatpak run --command=sh $app -c 'ldconfig -p | grep -F libjack.so.0'
    smoke

    # Again with everything the test saves, exports and recovers written to
    # the home directory outside ~/Music: a project is a folder, which the
    # file chooser portal cannot grant, so the sandbox has to reach it.
    scratch=$(mktemp -d "$HOME/kadence-flatpak-smoke.XXXXXX")
    smoke --env=TMPDIR="$scratch"
    rm -rf "$scratch"
}

case ${1:-} in
    setup) shift; setup "$@" ;;
    build) [ $# -eq 2 ] || { echo "usage: $0 build BUNDLE" >&2; exit 2; }; build "$2" ;;
    *) echo "usage: $0 setup [EXTRA_REF...] | build BUNDLE" >&2; exit 2 ;;
esac
