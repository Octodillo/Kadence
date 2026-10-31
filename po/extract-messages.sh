#!/bin/sh
# Regenerate po/kadence.pot from the QML and C++ sources.
# Translations go in po/<lang>/kadence.po (KDE layout); they are compiled to
# .mo files and installed by the CMake packaging (WU15).
set -eu
cd "$(dirname "$0")/.."
KEYWORDS="-ki18n:1 -ki18nc:1c,2 -ki18np:1,2 -ki18ncp:1c,2,3 \
    -kki18n:1 -kki18nc:1c,2 -kki18np:1,2 -kki18ncp:1c,2,3 \
    -kI18N_NOOP:1 -kI18NC_NOOP:1c,2"
COMMON="--from-code=UTF-8 --kde --package-name=kadence \
    --msgid-bugs-address=https://github.com/Octodillo/Kadence/issues"
# QML is close enough to JavaScript for xgettext; C++ goes through its own
# parser, then joins (-j) the same template.
find crates/daw-app/qml -name '*.qml' | sort > po/.files
# shellcheck disable=SC2086
xgettext $COMMON -L JavaScript $KEYWORDS --files-from=po/.files -o po/kadence.pot
find crates/daw-app/src/cpp -name '*.cpp' | sort > po/.files
# shellcheck disable=SC2086
xgettext $COMMON -L C++ -j $KEYWORDS --files-from=po/.files -o po/kadence.pot
rm po/.files
