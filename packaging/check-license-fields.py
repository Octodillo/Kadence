#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check the package recipes' license fields against what the binary links.

    packaging/check-license-fields.py THIRD-PARTY-LICENSES

THIRD-PARTY-LICENSES (written by third-party-licenses.py during the build)
lists every crate linked into kadence with its SPDX expression. The Fedora
spec's License tag and the Arch PKGBUILD's license array must name

- enough licenses to cover every crate: for each crate, all the licenses of
  at least one alternative of its expression, and
- no license that no crate uses (Kadence's own aside).

Standard library only.
"""

import os
import re
import sys

OWN = "GPL-3.0-or-later"
PACKAGING = os.path.dirname(os.path.abspath(__file__))
CRATE_LINE = re.compile(r"^(\S+ \S+): (.+)$")


def alternatives(expression):
    """An SPDX expression as a list of alternatives, each a set of licenses.

    Crates use only flat expressions (`A OR B`, `A AND B`, the old `A/B`),
    with AND binding tighter; `X WITH exception` counts as one license.
    """
    if "(" in expression:
        sys.exit(f"check-license-fields: nested expression not handled: {expression}")
    expression = expression.replace("/", " OR ")
    return [
        {re.sub(r"\s+", " ", term).strip() for term in alternative.split(" AND ")}
        for alternative in expression.split(" OR ")
    ]


def linked_crates(path):
    """{crate: expression} from the "Crates" section of THIRD-PARTY-LICENSES."""
    crates, inside = {}, False
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.rstrip("\n")
            if line == "Crates":
                inside = True
            elif inside and line.startswith("="):
                break
            elif inside and (m := CRATE_LINE.match(line)):
                crates[m.group(1)] = m.group(2)
    if not crates:
        sys.exit(f"check-license-fields: no crate list in {path}")
    return crates


def field(path, pattern):
    with open(path, encoding="utf-8") as f:
        m = re.search(pattern, f.read(), re.MULTILINE)
    if not m:
        sys.exit(f"check-license-fields: no license field in {path}")
    return m.group(1)


def declared():
    """{recipe: set of licenses it names}."""
    spec = field(os.path.join(PACKAGING, "fedora", "kadence.spec"), r"^License:\s*(.+)$")
    pkgbuild = field(os.path.join(PACKAGING, "arch", "PKGBUILD"), r"^license=\((.+)\)$")
    return {
        "fedora/kadence.spec": {
            name for alternative in alternatives(re.sub(r"[()]", "", spec)) for name in alternative
        },
        "arch/PKGBUILD": set(pkgbuild.split()),
    }


def problems(crates, names):
    found = []
    used = {OWN}
    for crate, expression in sorted(crates.items()):
        options = alternatives(expression)
        used.update(*options)
        if not any(option <= names for option in options):
            found.append(f"{crate} ({expression}) is not covered")
    for name in sorted(names - used):
        found.append(f"{name} is listed but no linked crate uses it")
    return found


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    crates = linked_crates(sys.argv[1])
    failed = False
    for recipe, names in declared().items():
        for problem in problems(crates, names):
            print(f"packaging/{recipe}: {problem}", file=sys.stderr)
            failed = True
    if failed:
        sys.exit(1)
    print(f"license fields cover all {len(crates)} linked crates")


if __name__ == "__main__":
    main()
