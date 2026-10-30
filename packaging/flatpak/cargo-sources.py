#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Turn Cargo.lock into cargo-sources.json for flatpak-builder.

flatpak-builder downloads every crate listed there (checked against the
Cargo.lock checksum), unpacks it into cargo/vendor, and writes a Cargo config
that points crates.io at that directory, so Cargo then builds offline.
Rerun it whenever Cargo.lock changes:

    packaging/flatpak/cargo-sources.py [Cargo.lock] [cargo-sources.json]

Only crates.io dependencies are supported; a git or other registry source
is an error, so the build can't silently go online.
"""

import json
import sys
import tomllib
from pathlib import Path

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
VENDOR = "cargo/vendor"

CONFIG = f"""[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "{VENDOR}"
"""


def sources(lock: dict) -> list[dict]:
    out = []
    for pkg in lock["package"]:
        source = pkg.get("source")
        if source is None:
            continue  # a workspace crate
        if source != CRATES_IO:
            sys.exit(f"{pkg['name']} {pkg['version']}: unsupported source {source}")
        name, version, checksum = pkg["name"], pkg["version"], pkg["checksum"]
        dest = f"{VENDOR}/{name}-{version}"
        out.append(
            {
                "type": "archive",
                "archive-type": "tar-gzip",
                "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
                "sha256": checksum,
                "dest": dest,
            }
        )
        # Cargo only checks the package checksum for vendored crates when
        # "files" is empty, which is what we want: the archive was verified.
        out.append(
            {
                "type": "inline",
                "contents": json.dumps({"package": checksum, "files": {}}),
                "dest": dest,
                "dest-filename": ".cargo-checksum.json",
            }
        )
    out.append(
        {
            "type": "inline",
            "contents": CONFIG,
            "dest": "cargo",
            "dest-filename": "config.toml",
        }
    )
    return out


def main() -> None:
    here = Path(__file__).resolve().parent
    lock_path = Path(sys.argv[1]) if len(sys.argv) > 1 else here.parents[1] / "Cargo.lock"
    out_path = Path(sys.argv[2]) if len(sys.argv) > 2 else here / "cargo-sources.json"
    with open(lock_path, "rb") as f:
        lock = tomllib.load(f)
    out_path.write_text(json.dumps(sources(lock), indent=2) + "\n")
    print(f"{out_path}: {len(lock['package'])} packages")


if __name__ == "__main__":
    main()
