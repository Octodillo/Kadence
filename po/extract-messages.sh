#!/bin/sh
# Regenerate po/kadence.pot from the QML and C++ sources.
# Translations go in po/<lang>/kadence.po (KDE layout); they are compiled to
# .mo files and installed by the CMake packaging (WU15).
set -eu
cd "$(dirname "$0")/.."
find crates/daw-app/qml crates/daw-app/src/cpp -name '*.qml' -o -name '*.cpp' | sort > po/.files
xgettext --from-code=UTF-8 -C --qt --package-name=kadence \
    --msgid-bugs-address=https://github.com/Octodillo/daw/issues \
    -ki18n:1 -ki18nc:1c,2 -ki18np:1,2 -ki18ncp:1c,2,3 \
    -kki18n:1 -kki18nc:1c,2 -kki18np:1,2 -kki18ncp:1c,2,3 \
    -kI18N_NOOP:1 -kI18NC_NOOP:1c,2 \
    --files-from=po/.files -o po/kadence.pot
rm po/.files
