#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Write the license notices of every crate linked into the kadence binary.

    packaging/third-party-licenses.py OUTPUT [--cargo CARGO]

Walks `cargo metadata` from the daw-app crate along normal (not dev or
build) dependencies for the host platform, skips proc-macros (they run in
the compiler, not in kadence) and workspace crates (they are Kadence's own
GPL code), and writes each remaining crate's name, version, SPDX license
and license, copyright and notice files to OUTPUT. Runs offline, so it
works from vendored sources (Fedora, Flatpak) and from a fetched cache
(Arch). Standard library only.
"""

import argparse
import json
import os
import re
import subprocess
import sys

ROOT_CRATE = "daw-app"
NOTICE_FILE = re.compile(r"^(LICEN[CS]E|COPYING|COPYRIGHT|NOTICE|UNLICENSE|AUTHORS)", re.IGNORECASE)

HEADER = """\
Third-party software in Kadence
===============================

Kadence is licensed under the GNU General Public License, version 3 or
later (see LICENSE). The kadence binary also contains the Rust crates
listed below, each under its own license. Their license, copyright and
notice files follow, as shipped in each crate's source.

Source code for every crate, including those under the Mozilla Public
License 2.0, is available from crates.io (https://crates.io/crates/NAME/
VERSION) and in the Kadence source release, which vendors it.
"""


def run(cmd):
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit(f"third-party-licenses: {' '.join(cmd)} failed:\n{result.stderr}")
    return result.stdout


def host_triple(cargo):
    # Ask cargo, not rustc: CMake hands us cargo's path, and a bare `rustc`
    # on PATH may be a different install, or a rustup proxy with no default.
    for line in run([cargo, "-vV"]).splitlines():
        if line.startswith("host: "):
            return line[len("host: "):].strip()
    sys.exit("third-party-licenses: cargo -vV printed no host triple")


def metadata(cargo, manifest, host):
    return json.loads(run(
        [cargo, "metadata", "--format-version", "1", "--locked", "--offline",
         "--filter-platform", host, "--manifest-path", manifest]))


def linked_packages(meta):
    """Packages reachable from ROOT_CRATE through normal dependencies."""
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    workspace = set(meta["workspace_members"])
    roots = [p for p in workspace if packages[p]["name"] == ROOT_CRATE]
    if not roots:
        sys.exit(f"third-party-licenses: no {ROOT_CRATE} in the workspace")

    seen, stack = set(), list(roots)
    while stack:
        pkg = stack.pop()
        if pkg in seen:
            continue
        seen.add(pkg)
        for dep in nodes[pkg]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                target = packages[dep["pkg"]]
                if any("proc-macro" in t["kind"] for t in target["targets"]):
                    continue
                stack.append(dep["pkg"])
    linked = [packages[p] for p in seen if p not in workspace]
    return sorted(linked, key=lambda p: (p["name"], p["version"]))


def notice_files(pkg):
    root = os.path.dirname(pkg["manifest_path"])
    names = sorted(n for n in os.listdir(root)
                   if NOTICE_FILE.match(n) and os.path.isfile(os.path.join(root, n)))
    if pkg.get("license_file"):
        extra = os.path.normpath(os.path.join(root, pkg["license_file"]))
        if os.path.isfile(extra) and os.path.basename(extra) not in names:
            names.append(os.path.relpath(extra, root))
    return [(n, os.path.join(root, n)) for n in names]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("output")
    parser.add_argument("--cargo", default=os.environ.get("CARGO", "cargo"))
    parser.add_argument("--manifest-path",
                        default=os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "Cargo.toml"))
    args = parser.parse_args()

    meta = metadata(args.cargo, args.manifest_path, host_triple(args.cargo))
    packages = linked_packages(meta)

    lines = [HEADER, "Crates", "------", ""]
    for p in packages:
        lines.append(f"{p['name']} {p['version']}: {p.get('license') or 'see its license file'}")
    lines.append("")

    missing = []
    for p in packages:
        files = notice_files(p)
        if not files:
            missing.append(p)
            continue
        for name, path in files:
            title = f"{p['name']} {p['version']}: {name}"
            lines += ["", "=" * len(title), title, "=" * len(title), ""]
            with open(path, encoding="utf-8", errors="replace") as f:
                lines.append(f.read().rstrip())
    if missing:
        # A few crates publish only an SPDX id. Name them so the gap is
        # visible; the license texts themselves are in the SPDX list.
        lines += ["", "Crates without a license file in their source", "-" * 45, ""]
        for p in missing:
            source = p.get("repository") or f"https://crates.io/crates/{p['name']}/{p['version']}"
            lines.append(f"{p['name']} {p['version']}: {p.get('license')} ({source})")

    os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
    with open(args.output, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
