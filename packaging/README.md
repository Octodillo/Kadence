# Packaging Kadence

Kadence installs the same way everywhere: Cargo builds the `kadence` binary
and CMake installs it with the desktop file, AppStream metadata, MIME type
and icon. App ID `io.github.octodillo.daw`, binary `kadence`.

| Where | Recipe | Tested in CI (`.github/workflows/packaging.yml`) |
|---|---|---|
| Arch Linux, SteamOS (distrobox), Manjaro | `arch/PKGBUILD` (`kadence-git`) | makepkg, namcap, install, smoke test |
| Fedora KDE | `fedora/kadence.spec` | offline rpmbuild, rpmlint, install, smoke test |
| KDE neon, Kubuntu | CMake from source, or the Flatpak | install to `/usr`, smoke test |
| Debian 13 and newer | CMake from source, or the Flatpak | install to `/usr`, smoke test (oldest supported: Qt 6.8, KF 6.13) |
| Any distribution | `flatpak/io.github.octodillo.daw.yml` | flatpak-builder, Flathub lint (advisory), smoke test in the sandbox |

Every native install is smoke-tested by `ci/smoke-matrix.sh`, which runs
`kadence --smoke-test` on X11 (Xvfb), Wayland (headless weston) and at 100%,
125%, 150% and 200% scale. The Flatpak job runs the smoke test offscreen
inside the sandbox and checks that libjack is visible there.

## Runtime requirements

- Qt 6.8 or newer: QtBase (Gui, Widgets), QtDeclarative (Quick,
  QuickControls2, Quick Dialogs; on Debian and Ubuntu the last is the
  separate `qml6-module-qtquick-dialogs`). Before Qt 6.10 the Wayland client plugin is a separate
  package (`qt6-wayland`), needed for native Wayland windows.
- KDE Frameworks 6.13 or newer: Kirigami, qqc2-desktop-style, KCoreAddons,
  KI18n, KCrash, KConfig, KColorScheme, KIconThemes. Breeze icons are
  recommended outside Plasma.
- libjack (`libjack.so.0`), loaded at run time. On Plasma this is normally
  PipeWire's (`pipewire-jack`); JACK2's works too.

Kirigami and qqc2-desktop-style are QML modules and libjack is loaded with
`dlopen`, so automatic shared-library dependency scanners miss them. Each
recipe lists them by hand.

## Build requirements

Rust 1.88 or newer, a C++ compiler, clang (bindgen), `lld` (CXX-Qt picks
it over the deprecated gold linker), CMake, Ninja, extra-cmake-modules,
the `-dev` packages of everything above, and `jack.pc` (from
`pipewire-jack` or JACK2's development package).

## Arch Linux

```sh
cd packaging/arch
makepkg -si
```

The PKGBUILD builds from git. To build a local checkout instead:

```sh
KADENCE_SOURCE="git+file://$PWD/../..#commit=$(git rev-parse HEAD)" makepkg -si
```

## Fedora

```sh
packaging/make-dist.sh ~/rpmbuild/SOURCES   # tarball with vendored crates
sudo dnf builddep packaging/fedora/kadence.spec
rpmbuild -ba packaging/fedora/kadence.spec
```

The spec is good for COPR and local builds. An official Fedora package
would also need the vendored crates' licenses in `License:` and a
`Provides: bundled(crate(...))` line for each crate.

## From source (any distribution)

```sh
cmake -B build -G Ninja -DCMAKE_INSTALL_PREFIX=/usr
cmake --build build
sudo cmake --install build
```

## Flatpak

```sh
packaging/flatpak/cargo-sources.py      # crate list from Cargo.lock
flatpak-builder --user --install --force-clean build-flatpak \
    packaging/flatpak/io.github.octodillo.daw.yml
flatpak run io.github.octodillo.daw
```

The manifest uses the KDE 6.11 runtime with the Rust and LLVM (for `lld`)
SDK extensions. Audio goes through PipeWire's JACK interface; the sandbox
gets the PipeWire socket. Plugins come from two places:

- The Flatpak plugin extension point `org.freedesktop.LinuxAudio.Plugins`,
  mounted at `/app/extensions/Plugins`, which Flathub's packaged CLAP, LV2 and
  VST3 plugins install into (as for Ardour and Zrythm).
- `~/.clap`, `~/.lv2` and `~/.vst3`, read only.

`CLAP_PATH` and `VST3_PATH` point at the extension's directories, and the
CLAP and VST3 hosts search them before the standard locations. `LV2_PATH` is
left unset, since it would replace lilv's defaults rather than add to them;
instead the LV2 host adds `/app/extensions/Plugins/lv2` (and the app's own
`/app/lib/lv2`) itself when it runs inside Flatpak (`/.flatpak-info` exists). It searches
them after `~/.lv2`, so for LV2 (unlike CLAP and VST3) a copy of a plugin in
the home directory wins over the extension's.

Plugins installed system-wide (`/usr/lib/{clap,lv2,vst3}`) are not visible
inside the sandbox, so the native packages remain the better choice for
users with many distro-packaged plugins.

### Submitting to Flathub

Flathub builds from a separate `flathub/io.github.octodillo.daw` repository.
Copy the manifest there, change the `dir` source to a `git` source with a
release tag and commit, and commit the generated `cargo-sources.json` next
to it. Regenerate it whenever `Cargo.lock` changes.

## Checking an install

`kadence --smoke-test` drives the real UI through every page and dialog and
exits 0 when it passes. Run it with `QT_QPA_PLATFORM=offscreen` on a machine
without a display.
