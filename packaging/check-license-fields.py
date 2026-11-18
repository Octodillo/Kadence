#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check the package recipes' license fields against what the binary links.

    packaging/check-license-fields.py THIRD-PARTY-LICENSES

THIRD-PARTY-LICENSES (written by third-party-licenses.py during the build)
lists every crate linked into kadence with its SPDX expression.

- The Fedora spec's License tag enumerates: its terms joined by AND must be
  exactly the distinct expressions of the linked crates plus Kadence's own
  (an `A AND B` crate counts as the two terms A and B).
- The Arch PKGBUILD's license array covers: it names all the licenses of at
  least one alternative of every crate's expression, and no license that no
  crate uses.

Standard library only.
"""

import os
import re
import sys

OWN = "GPL-3.0-or-later"
PACKAGING = os.path.dirname(os.path.abspath(__file__))
CRATE_LINE = re.compile(r"^(\S+ \S+): (.+)$")


TOKEN = re.compile(r"\(|\)|\bAND\b|\bOR\b|/")


def alternatives(expression):
    """An SPDX expression as a set of alternatives, each a set of licenses.

    AND binds tighter than OR, `A/B` is the old spelling of `A OR B`, and
    `X WITH exception` counts as one license. `(A OR B) AND C` comes out as
    {A, C} or {B, C}, so two spellings of one choice compare equal.
    """
    tokens, at = [], 0
    for m in TOKEN.finditer(expression):
        tokens += [expression[at : m.start()], m.group()]
        at = m.end()
    tokens.append(expression[at:])
    tokens = [" ".join(t.split()) for t in tokens if t.strip()]
    tokens.reverse()

    def bad():
        sys.exit(f"check-license-fields: cannot read the expression: {expression}")

    def operand():
        token = tokens.pop() if tokens else bad()
        if token == "(":
            inner = either()
            if not tokens or tokens.pop() != ")":
                bad()
            return inner
        if token in (")", "AND", "OR", "/"):
            bad()
        return {frozenset([token])}

    def both():
        result = operand()
        while tokens and tokens[-1] == "AND":
            tokens.pop()
            right = operand()
            result = {a | b for a in result for b in right}
        return result

    def either():
        result = both()
        while tokens and tokens[-1] in ("OR", "/"):
            tokens.pop()
            result = result | both()
        return result

    result = either()
    if tokens:
        bad()
    return frozenset(result)


def terms(expression):
    """The terms a license tag joins with AND, each as `alternatives` gives it."""
    options = alternatives(expression)
    if len(options) == 1:
        return {frozenset([frozenset([name])]) for name in next(iter(options))}
    return {options}


def spell(term):
    return " OR ".join(sorted(" AND ".join(sorted(option)) for option in term))


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


def spec_terms(tag):
    """The top-level AND terms of the spec's License tag."""
    found, depth, start = set(), 0, 0
    for m in re.finditer(r"\(|\)|\bAND\b", tag):
        if m.group() == "(":
            depth += 1
        elif m.group() == ")":
            depth -= 1
        elif depth == 0:
            found |= terms(tag[start : m.start()])
            start = m.end()
    return found | terms(tag[start:])


def spec_problems(crates):
    """The spec's tag against the exact list of expressions."""
    tag = field(os.path.join(PACKAGING, "fedora", "kadence.spec"), r"^License:\s*(.+)$")
    listed = spec_terms(tag)
    wanted = terms(OWN)
    for expression in crates.values():
        wanted |= terms(expression)
    return [f"{spell(term)} is missing from the License tag" for term in wanted - listed] + [
        f"{spell(term)} is in the License tag but no linked crate has it"
        for term in listed - wanted
    ]


def pkgbuild_problems(crates):
    """The PKGBUILD's array against what covers every crate."""
    array = field(os.path.join(PACKAGING, "arch", "PKGBUILD"), r"^license=\((.+)\)$")
    names = set(array.replace("'", " ").replace('"', " ").split())
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
    for recipe, check in (
        ("fedora/kadence.spec", spec_problems),
        ("arch/PKGBUILD", pkgbuild_problems),
    ):
        for problem in sorted(check(crates)):
            print(f"packaging/{recipe}: {problem}", file=sys.stderr)
            failed = True
    if failed:
        sys.exit(1)
    print(f"license fields match all {len(crates)} linked crates")


if __name__ == "__main__":
    main()
