# SPDX-License-Identifier: GPL-3.0-or-later
#
# Builds from the self-contained tarball made by packaging/make-dist.sh,
# which vendors every crate, so the build needs no network:
#   packaging/make-dist.sh ~/rpmbuild/SOURCES
#   rpmbuild -ba packaging/fedora/kadence.spec

%global appid io.github.octodillo.daw

Name:           kadence
Version:        0.1.0
Release:        1%{?dist}
Summary:        Digital audio workstation for KDE Plasma
# Kadence is GPL-3.0-or-later. The binary also links the vendored crates it
# depends on, among them symphonia (MPL-2.0); regenerate this list with
#   cargo tree -p daw-app -e normal,no-proc-macro --prefix none -f '{l}' | sort -u
# when Cargo.lock changes.
License:        GPL-3.0-or-later AND MPL-2.0 AND Apache-2.0 AND MIT AND Zlib AND (MIT OR Apache-2.0) AND (Unlicense OR MIT) AND (Zlib OR Apache-2.0 OR MIT) AND (0BSD OR Apache-2.0)
URL:            https://github.com/Octodillo/Kadence
Source0:        %{name}-%{version}.tar.gz

ExclusiveArch:  x86_64 aarch64

BuildRequires:  cargo >= 1.88
BuildRequires:  rust >= 1.88
BuildRequires:  gcc-c++
BuildRequires:  clang
BuildRequires:  lld
BuildRequires:  cmake
BuildRequires:  ninja-build
BuildRequires:  extra-cmake-modules
# msgfmt, for the translations in po/<lang>/.
BuildRequires:  gettext
BuildRequires:  python3
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib
BuildRequires:  qt6-qtbase-devel
BuildRequires:  qt6-qtdeclarative-devel
BuildRequires:  kf6-kcoreaddons-devel
BuildRequires:  kf6-ki18n-devel
BuildRequires:  kf6-kcrash-devel
BuildRequires:  kf6-kconfig-devel
BuildRequires:  kf6-kcolorscheme-devel
BuildRequires:  kf6-kiconthemes-devel
BuildRequires:  kf6-kirigami-devel
BuildRequires:  pkgconfig(jack)
# LV2 plugin hosting; the library dependency is found automatically.
BuildRequires:  pkgconfig(lilv-0)
# %%check runs the real UI offscreen, which needs the QML style.
BuildRequires:  kf6-qqc2-desktop-style

# QML modules and libjack are loaded at run time, so RPM can't see them.
Requires:       kf6-kirigami%{?_isa}
Requires:       kf6-qqc2-desktop-style%{?_isa}
Requires:       libjack.so.0()(64bit)
Requires:       hicolor-icon-theme
Recommends:     pipewire-jack-audio-connection-kit
Recommends:     breeze-icon-theme

%description
Kadence is a digital audio workstation for KDE Plasma. It records and plays
back multitrack audio and MIDI, hosts CLAP, LV2 and VST3 plugins, and talks
to PipeWire or JACK.

%prep
%autosetup -p1

%build
export CARGO_NET_OFFLINE=true
%cmake -G Ninja -DKADENCE_INSTALL_LICENSES=OFF
%cmake_build

%install
%cmake_install

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/%{appid}.desktop
appstream-util validate-relax --nonet %{buildroot}%{_metainfodir}/%{appid}.metainfo.xml
config=$(mktemp -d)
XDG_CONFIG_HOME=$config QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
    %{__cmake_builddir}/cargo/release/kadence --smoke-test
rm -rf "$config"

%files
%license LICENSE %{__cmake_builddir}/THIRD-PARTY-LICENSES
%doc README.md
%{_bindir}/kadence
%{_datadir}/applications/%{appid}.desktop
%{_metainfodir}/%{appid}.metainfo.xml
%{_datadir}/mime/packages/%{appid}.mime.xml
%{_datadir}/icons/hicolor/scalable/apps/%{appid}.svg
# Once po/ holds a translation, add `%%find_lang %%{name}` to %%install and
# make this `%%files -f %%{name}.lang`; it fails while there are none.

%changelog
* Thu Oct 29 2026 Octodillo <14184084+Octodillo@users.noreply.github.com> - 0.1.0-1
- First package
