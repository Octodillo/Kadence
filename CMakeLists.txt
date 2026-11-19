# SPDX-License-Identifier: GPL-3.0-or-later
#
# Thin CMake shell for KDE install rules. Cargo builds everything; CMake
# runs Cargo and installs the binary plus the desktop file, AppStream
# metadata, icon, MIME type and translations to the standard KDE locations.
#
#   cmake -B build -DCMAKE_INSTALL_PREFIX=/usr
#   cmake --build build
#   DESTDIR=pkg cmake --install build

cmake_minimum_required(VERSION 3.22)
project(kadence VERSION 0.1.0 LANGUAGES C)

set(APP_ID io.github.octodillo.daw)
set(APP_BINARY kadence)

find_package(ECM 6.0 NO_MODULE)
if(ECM_FOUND)
    list(APPEND CMAKE_MODULE_PATH ${ECM_MODULE_PATH})
    include(KDEInstallDirs)
else()
    message(STATUS "extra-cmake-modules not found; using GNUInstallDirs layout")
    include(GNUInstallDirs)
    set(KDE_INSTALL_BINDIR ${CMAKE_INSTALL_BINDIR})
    set(KDE_INSTALL_APPDIR ${CMAKE_INSTALL_DATADIR}/applications)
    set(KDE_INSTALL_METAINFODIR ${CMAKE_INSTALL_DATADIR}/metainfo)
    set(KDE_INSTALL_MIMEDIR ${CMAKE_INSTALL_DATADIR}/mime/packages)
    set(KDE_INSTALL_FULL_ICONDIR ${CMAKE_INSTALL_FULL_DATADIR}/icons)
    set(KDE_INSTALL_LOCALEDIR ${CMAKE_INSTALL_LOCALEDIR})
    set(KDE_INSTALL_DATADIR ${CMAKE_INSTALL_DATADIR})
endif()

# Where LICENSE and THIRD-PARTY-LICENSES go. Arch wants
# share/licenses/<pkgname>; Fedora's %license copies them itself and turns
# this off.
option(KADENCE_INSTALL_LICENSES "Install LICENSE and THIRD-PARTY-LICENSES" ON)
set(KADENCE_LICENSE_DIR ${KDE_INSTALL_DATADIR}/licenses/${APP_BINARY}
    CACHE STRING "Directory for LICENSE and THIRD-PARTY-LICENSES")

# The app itself is built by Cargo.
find_program(CARGO cargo REQUIRED)
# cxx-qt and build.rs ask qmake where Qt lives. Distros call it qmake6
# or qmake-qt6; the Flatpak KDE SDK only has plain qmake.
find_program(QMAKE NAMES qmake6 qmake-qt6 qmake REQUIRED)
# Packages build the optimised release profile into the build directory.
# CI's install layout check (ci/check-install.sh) builds the dev profile into
# the workspace's target directory instead, reusing the build its tests left.
set(KADENCE_CARGO_PROFILE release
    CACHE STRING "Cargo profile kadence is built with")
set(KADENCE_CARGO_TARGET_DIR ${CMAKE_BINARY_DIR}/cargo
    CACHE PATH "Cargo target directory")
# Cargo puts the dev profile's output in debug/, any other profile's in a
# directory of its name.
if(KADENCE_CARGO_PROFILE STREQUAL "dev")
    set(CARGO_PROFILE_DIR debug)
else()
    set(CARGO_PROFILE_DIR ${KADENCE_CARGO_PROFILE})
endif()
set(APP_EXECUTABLE ${KADENCE_CARGO_TARGET_DIR}/${CARGO_PROFILE_DIR}/${APP_BINARY})
add_custom_target(cargo_build ALL
    COMMAND ${CMAKE_COMMAND} -E env CARGO_TARGET_DIR=${KADENCE_CARGO_TARGET_DIR} QMAKE=${QMAKE}
            ${CARGO} build --profile ${KADENCE_CARGO_PROFILE} --locked -p daw-app
    WORKING_DIRECTORY ${CMAKE_SOURCE_DIR}
    USES_TERMINAL
    COMMENT "Building ${APP_BINARY} with Cargo")
install(PROGRAMS ${APP_EXECUTABLE} DESTINATION ${KDE_INSTALL_BINDIR})

# The binary statically links MIT, Apache-2.0, Zlib and MPL-2.0 crates,
# whose notices must ship with it. Build it from the same Cargo.lock.
find_package(Python3 COMPONENTS Interpreter REQUIRED)
set(THIRD_PARTY_LICENSES ${CMAKE_BINARY_DIR}/THIRD-PARTY-LICENSES)
add_custom_command(OUTPUT ${THIRD_PARTY_LICENSES}
    COMMAND ${Python3_EXECUTABLE} -I ${CMAKE_SOURCE_DIR}/packaging/third-party-licenses.py
            --cargo ${CARGO} ${THIRD_PARTY_LICENSES}
    DEPENDS ${CMAKE_SOURCE_DIR}/Cargo.lock ${CMAKE_SOURCE_DIR}/packaging/third-party-licenses.py
    WORKING_DIRECTORY ${CMAKE_SOURCE_DIR}
    COMMENT "Collecting third-party license notices")
add_custom_target(third_party_licenses ALL DEPENDS ${THIRD_PARTY_LICENSES})
# After the build: Cargo has fetched the crates by then, and two rustup
# proxies starting at once race to install the toolchain's components.
add_dependencies(third_party_licenses cargo_build)
if(KADENCE_INSTALL_LICENSES)
    install(FILES LICENSE ${THIRD_PARTY_LICENSES} DESTINATION ${KADENCE_LICENSE_DIR})
endif()

install(FILES data/${APP_ID}.desktop DESTINATION ${KDE_INSTALL_APPDIR})
install(FILES data/${APP_ID}.metainfo.xml DESTINATION ${KDE_INSTALL_METAINFODIR})
install(FILES data/${APP_ID}.mime.xml DESTINATION ${KDE_INSTALL_MIMEDIR})
install(FILES data/icons/${APP_ID}.svg
        DESTINATION ${KDE_INSTALL_FULL_ICONDIR}/hicolor/scalable/apps)

# Translations: po/<lang>/kadence.po becomes <lang>/LC_MESSAGES/kadence.mo,
# where KLocalizedString::setApplicationDomain("kadence") looks. Plain msgfmt
# rather than ki18n_install(), which would pull Qt into this C-only project.
file(GLOB PO_FILES CONFIGURE_DEPENDS ${CMAKE_SOURCE_DIR}/po/*/kadence.po)
if(PO_FILES)
    find_package(Gettext REQUIRED)
    set(MO_FILES)
    foreach(po_file IN LISTS PO_FILES)
        get_filename_component(po_dir ${po_file} DIRECTORY)
        get_filename_component(lang ${po_dir} NAME)
        set(mo_dir ${CMAKE_BINARY_DIR}/locale/${lang})
        add_custom_command(OUTPUT ${mo_dir}/kadence.mo
            COMMAND ${CMAKE_COMMAND} -E make_directory ${mo_dir}
            COMMAND ${GETTEXT_MSGFMT_EXECUTABLE} --check -o ${mo_dir}/kadence.mo ${po_file}
            DEPENDS ${po_file}
            COMMENT "Compiling the ${lang} translation")
        list(APPEND MO_FILES ${mo_dir}/kadence.mo)
        install(FILES ${mo_dir}/kadence.mo DESTINATION ${KDE_INSTALL_LOCALEDIR}/${lang}/LC_MESSAGES)
    endforeach()
    add_custom_target(translations ALL DEPENDS ${MO_FILES})
endif()
