#!/usr/bin/env python3
"""Writes package.json, which tells the app which package it came from, so that it can update
itself (crates/update): `package-marker.py <folder> <kind> [--no-entries]`.

The kind is the package's, as in its file name (`windows-x86_64`, `linux-x86_64`, ...). The
entries are the folder's top-level names, package.json included: an update replaces those and
leaves anything else in the folder. The macOS app is replaced as a whole and lists none
(`--no-entries`, with the folder its Contents/Resources). Run it once the folder is complete.
"""

import json
import os
import sys


def main() -> None:
    args = sys.argv[1:]
    with_entries = "--no-entries" not in args
    args = [arg for arg in args if arg != "--no-entries"]
    if len(args) != 2:
        sys.exit(__doc__)
    folder, kind = args
    marker = os.path.join(folder, "package.json")
    entries = sorted(set(os.listdir(folder)) | {"package.json"}) if with_entries else []
    with open(marker, "w", encoding="utf-8", newline="\n") as f:
        json.dump({"kind": kind, "entries": entries}, f, indent=2)
        f.write("\n")
    print(f"{marker}: {kind}, {len(entries)} entries")


if __name__ == "__main__":
    main()
