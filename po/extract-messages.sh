#!/bin/sh
# Regenerate po/kadence.pot from the QML and C++ sources, or with --check
# say whether it is up to date (CI runs that).
# Translations go in po/<lang>/kadence.po (KDE layout); they are compiled to
# .mo files and installed by the CMake packaging.
set -eu
cd "$(dirname "$0")/.."
KEYWORDS="-ki18n:1 -ki18nc:1c,2 -ki18np:1,2 -ki18ncp:1c,2,3 \
    -kki18n:1 -kki18nc:1c,2 -kki18np:1,2 -kki18ncp:1c,2,3 \
    -kI18N_NOOP:1 -kI18NC_NOOP:1c,2"
# File names without line numbers, so the template only changes when a
# message does.
COMMON="--from-code=UTF-8 --kde --package-name=kadence --add-location=file \
    --msgid-bugs-address=https://github.com/Octodillo/Kadence/issues"

# Writes the template to the file $1.
extract() {
    files=$(mktemp)
    # QML is close enough to JavaScript for xgettext; C++ goes through its
    # own parser, then joins (-j) the same template.
    find crates/daw-app/qml -name '*.qml' | LC_ALL=C sort > "$files"
    # shellcheck disable=SC2086
    xgettext $COMMON -L JavaScript $KEYWORDS --files-from="$files" -o "$1"
    find crates/daw-app/src/cpp -name '*.cpp' | LC_ALL=C sort > "$files"
    # shellcheck disable=SC2086
    xgettext $COMMON -L C++ -j $KEYWORDS --files-from="$files" -o "$1"
    rm "$files"
}

# The messages of the template $1, one per line and sorted: what a
# translator is asked for, whatever gettext version wrote the file.
messages() {
    msgcat --no-wrap --no-location "$1" | awk '
        BEGIN { RS = ""; FS = "\n" }
        {
            entry = ""
            for (i = 1; i <= NF; i++)
                if ($i ~ /^(msgctxt|msgid|msgid_plural) /)
                    entry = entry $i "\t"
            if (entry != "msgid \"\"\t")
                print entry
        }' | LC_ALL=C sort
}

if [ "${1:-}" = --check ]; then
    dir=$(mktemp -d)
    trap 'rm -r "$dir"' EXIT
    extract "$dir/now.pot"
    messages po/kadence.pot > "$dir/committed"
    messages "$dir/now.pot" > "$dir/now"
    if ! diff -u "$dir/committed" "$dir/now"; then
        echo "po/kadence.pot is out of date: run po/extract-messages.sh and commit it." >&2
        exit 1
    fi
else
    extract po/kadence.pot
fi
