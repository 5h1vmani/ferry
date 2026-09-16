#!/usr/bin/env python3
"""What scripts/gen_tokens.py and scripts/gen_errors.py both need: reading
a JSON source file, reading a generated file already on disk, writing a
generated file, and the check-or-write flow every generator script runs
from its own `main`.

Only the Python standard library is used, on purpose, so a script that
imports this needs no setup beyond Python 3.
"""

import json
import sys


def load_json(path):
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


def read_existing(path):
    if not path.exists():
        return None
    with open(path, "r", encoding="utf-8") as handle:
        return handle.read()


def write_file(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(content)


def check_or_write(kind, script_name, root, files):
    """Run the check-or-write flow every generator script's `main` follows.

    `files` is a list of `(path, content)` pairs, each the generated
    content that belongs at that path. With `--check` on the command
    line, compares every path's content on disk to `content` and reports
    every mismatch without touching disk, so CI can stop anyone from
    hand-editing generated code. Otherwise writes every file and reports
    what it wrote. Returns the process exit code either way.
    """
    check_mode = "--check" in sys.argv[1:]

    if check_mode:
        mismatches = [
            str(path.relative_to(root))
            for path, content in files
            if read_existing(path) != content
        ]
        if mismatches:
            print(
                f"Generated {kind} are out of date. Run "
                f"`python3 scripts/{script_name}` and commit the result. "
                "Out of date: " + ", ".join(mismatches)
            )
            return 1
        print(f"Generated {kind} are up to date.")
        return 0

    for path, content in files:
        write_file(path, content)
        print(f"Wrote {path.relative_to(root)}")
    return 0
