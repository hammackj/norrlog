#!/usr/bin/env python3
"""Fail if a tracked file names something kept out of the public repository.

Tests and comments are written from real play, and real character names and
install paths have crept back in after being scrubbed once. This checks every
tracked text file for:

- character names that were removed on purpose. They are stored as SHA-256
  hashes of the lowercased word, so this file does not publish them itself.
  A hash only hides a name from a casual read; it is not a secret.
- personal install paths.

Fixtures use made-up names instead (Tharn, Brennagh, Orrin, Mirela, Garrow).

    tools/check-private.py            # the tracked tree
    tools/check-private.py FILE...    # just these files
"""

import hashlib
import re
import subprocess
import sys

# sha256(lowercased word) of each excluded character name.
NAMES = {
    "e23e04048edf589c207d8670163ec22f75abf0e49231e827e995934f0f07a5cf",
    "2c629ffd59b2210bad8466ef5520199e2c0a183af35d5797a3744629ee603f0d",
    "0011912b589c2fa4fecf0a80f2d45daa93373ee5b903435c101c951ea6489544",
    "0a7c61ff60fa02ed5932ff73fb2f878c530b7a635b7f81de8c6df9b40853affe",
    "a19bb8f2e9bbfc376af36c2cccb1b73ad4d3bfd9eaf8565578aa894e9e0902d6",
    "3b9f784fd164c5b939961f5bcb90c9947b3a69893ec0c79bae10238f82ad1af2",
}

# Paths that only exist on the developer's machine. The author's handle is
# public on purpose (the author line, the palette name, the project URLs);
# their home directory and game drive are not.
PATHS = [
    re.compile(r"/home/user\b"),
    re.compile(r"/path/to/games\b"),
]

# The checker itself names the patterns.
SKIP = {"tools/check-private.py"}

WORD = re.compile(r"[A-Za-z]+")


def files():
    if len(sys.argv) > 1:
        return sys.argv[1:]
    out = subprocess.run(
        ["git", "ls-files", "-z"], check=True, capture_output=True
    ).stdout
    return [f for f in out.decode().split("\0") if f]


def main():
    found = 0
    for path in files():
        if path in SKIP:
            continue
        try:
            with open(path, "rb") as f:
                data = f.read()
        except (IsADirectoryError, FileNotFoundError):
            continue
        if b"\0" in data[:8192]:
            continue  # binary
        text = data.decode("utf-8", "replace")
        for n, line in enumerate(text.splitlines(), 1):
            for w in WORD.findall(line):
                if hashlib.sha256(w.lower().encode()).hexdigest() in NAMES:
                    print(f"{path}:{n}: an excluded character name ({w[0]}…)")
                    found += 1
            for p in PATHS:
                if p.search(line):
                    print(f"{path}:{n}: a personal path ({p.pattern})")
                    found += 1
    if found:
        print(f"{found} private reference(s); use a made-up name or a generic path.")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
