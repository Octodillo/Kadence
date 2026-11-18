#!/bin/sh
# Install what an Arch Linux container needs to build, test and install the
# whole workspace, daw-app with Qt included, and make TOOLCHAIN (default:
# stable) the default Rust.
#
#   ci/arch-build-deps.sh [TOOLCHAIN [COMPONENT...]]
set -eu

toolchain=${1:-stable}
[ $# -gt 0 ] && shift

pacman -Syu --noconfirm --needed \
    base-devel git rustup cmake ninja clang \
    qt6-base qt6-declarative qt6-wayland kirigami qqc2-desktop-style \
    kcoreaddons ki18n kcrash kconfig kcolorscheme kiconthemes \
    breeze breeze-icons noto-fonts lld gettext python \
    extra-cmake-modules pipewire-jack lilv \
    desktop-file-utils appstream xorg-server-xvfb
rustup default "$toolchain"
for component in "$@"; do
    rustup component add "$component"
done
