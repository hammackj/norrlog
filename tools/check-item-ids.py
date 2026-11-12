#!/usr/bin/env python3
"""Has the daemon read my inventory dump yet? Answered before you clear it.

Clearing a bag is not reversible and the item ids in it are the only thing
that names an item canonically, so "I think it got folded" is not good enough
to act on. This compares the dumps sitting in the game folder against the
record the daemon keeps, row by row, and says which ids are not in it.

Two questions, and the second is the one that matters:

    is the file listed as a source?   -- the daemon has seen the filename
    is every id in it in the record?  -- the daemon has read this version

The first can be true while the second is false: a source is remembered by
name, and a dump you overwrote an hour ago is the same filename carrying
different rows. So the ids are checked directly rather than trusting the
source list, and an id missing from the record is reported with its name.

    tools/check-item-ids.py                     # ask the running daemon
    tools/check-item-ids.py --game-dir DIR      # where the dumps are
    tools/check-item-ids.py --record FILE       # read the file, not the daemon

Exits non-zero when something is unrecorded, so it can gate a script.
"""

import argparse
import csv
import json
import os
import re
import sys
import urllib.error
import urllib.request

PORT = int(os.environ.get("NORRLOGD_PORT") or os.environ.get("EQLPARSERD_PORT", "8777"))
GAME_DIR = os.environ.get("NORRLOG_GAME_DIR") or os.environ.get("EQLPARSER_GAME_DIR") or os.environ.get("EQL_GAME_DIR")
RECORD = os.path.expanduser("~/.config/norrlog/item-ids.json")

# The grade rule, as `norrlog-core/src/itemids.rs` states it: `+1` to `+10`
# is a roll of the same item and not part of its name. Only used to report a
# missing row legibly -- the id is what is being checked, and an id is an id
# whatever grade the copy in your bags happens to be.
_GRADE = re.compile(r"^(?P<base>.*?)\s*\+(?P<g>\d{1,2})$")


def base_name(name):
    m = _GRADE.match(name.strip())
    if m and 1 <= int(m.group("g")) <= 10:
        return m.group("base").strip()
    return name.strip()


def from_daemon(port):
    """The record the *running* daemon holds, which is the one that matters.

    Read from the daemon rather than the file because the file is written
    after a fold and the question being asked is about the program: a daemon
    that has the ids in memory has recorded them, and one that is not running
    has not folded anything since it stopped.
    """
    url = f"http://127.0.0.1:{port}/api/item-ids"
    try:
        with urllib.request.urlopen(url, timeout=5) as r:
            return json.load(r), f"the daemon on :{port}"
    except (urllib.error.URLError, OSError, json.JSONDecodeError) as e:
        raise SystemExit(
            f"check-item-ids: no answer from {url} ({e}).\n"
            f"  The daemon is how this is normally asked. To read the file it\n"
            f"  last wrote instead: tools/check-item-ids.py --record {RECORD}"
        )


def from_file(path):
    try:
        with open(path, encoding="utf-8") as f:
            return {"path": path, "registry": json.load(f)}, path
    except OSError as e:
        raise SystemExit(f"check-item-ids: cannot read {path}: {e}")


def ids_in(registry):
    """Every item id the record holds, as strings."""
    items = registry.get("items")
    out = set()
    if isinstance(items, dict):
        out.update(str(k) for k in items)
    elif isinstance(items, list):
        for r in items:
            v = r.get("item_id", r.get("id")) if isinstance(r, dict) else None
            if v is not None:
                out.add(str(v))
    return out


def dumps_in(game_dir):
    """The client's dumps, newest first -- the one you just wrote is first."""
    try:
        names = os.listdir(game_dir)
    except OSError as e:
        raise SystemExit(f"check-item-ids: cannot list {game_dir}: {e}")
    found = [
        os.path.join(game_dir, n)
        for n in names
        if n.endswith("-Inventory.txt") or n.endswith("-Equipment.txt")
    ]
    return sorted(found, key=os.path.getmtime, reverse=True)


def rows_of(path):
    """-> [(id, name)] for the rows that name an item and carry an id.

    An empty slot and a row whose id is 0 are both skipped: the client writes
    a 0 where it has no id to give, which is not the same as an item whose id
    is unrecorded, and reporting it as missing would cry wolf on every dump.
    """
    with open(path, encoding="utf-8", errors="replace") as f:
        rows = list(csv.DictReader(f, delimiter="\t"))
    out = []
    for r in rows:
        name = (r.get("Name") or "").strip()
        rid = (r.get("ID") or "").strip()
        if name in ("", "Empty") or not rid.isdigit() or rid == "0":
            continue
        out.append((rid, name))
    return out, len(rows)


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--game-dir", default=GAME_DIR, required=not GAME_DIR,
                    help="the game directory (default: $NORRLOG_GAME_DIR)")
    ap.add_argument("--port", type=int, default=PORT)
    ap.add_argument("--record", help="read this file instead of asking the daemon")
    ap.add_argument("-v", "--verbose", action="store_true",
                    help="list every unrecorded row, not the first ten")
    args = ap.parse_args()

    doc, whence = (from_file(args.record) if args.record
                   else from_daemon(args.port))
    reg = doc.get("registry") or {}
    have = ids_in(reg)
    sources = {s.get("file") for s in reg.get("sources", []) if isinstance(s, dict)}
    print(f"record: {len(have)} item ids, {len(sources)} source(s), from {whence}")
    print(f"        {doc.get('path', args.record or '?')}\n")

    dumps = dumps_in(args.game_dir)
    if not dumps:
        raise SystemExit(f"check-item-ids: no dumps in {args.game_dir}")

    bad = 0
    for p in dumps:
        name = os.path.basename(p)
        rows, total = rows_of(p)
        missing = [(i, n) for i, n in rows if i not in have]
        when = __import__("datetime").datetime.fromtimestamp(
            os.path.getmtime(p)).isoformat(" ", "seconds")
        listed = "listed" if name in sources else "NOT a known source"
        if missing:
            bad += 1
            print(f"  ✗ {name}")
            print(f"      written {when} · {total} rows, {len(rows)} with an id · {listed}")
            print(f"      {len(missing)} NOT in the record:")
            for i, n in (missing if args.verbose else missing[:10]):
                print(f"        {i:>7}  {base_name(n)}")
            if not args.verbose and len(missing) > 10:
                print(f"        … and {len(missing) - 10} more (-v for all)")
        else:
            print(f"  ✓ {name}")
            print(f"      written {when} · {total} rows, {len(rows)} with an id · {listed}")
            print(f"      all {len(rows)} recorded")

    print()
    if bad:
        print("Not everything is recorded. Do not clear those bags yet.")
        print("The daemon folds a dump when it appears; if it is not running,")
        print("start it and it will fold on the next pass.")
        return 1
    print("Every id in every dump is in the record. Safe to clear.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
