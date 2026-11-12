#!/usr/bin/env python3
"""Import tradeskill recipes from an EQTraders "Full Recipe List" tree.

The log never says which tradeskill a combine belongs to, and for a recipe that
never skills you up there is nothing in the log to infer it from. The recipe
book fills that in: a curated file of what a combine makes, out of what, in
which tradeskill.

Input is the tree exactly as EQTraders prints it — paste it into a file and
point this at it:

    To make Small Brick of High Quality Ore (Smithing: Yield 2, trivial 29), Combine the following in a Forge, ...
        Smithing Chisel (*)
        Large Brick of High Quality Ore (*)
            To make Large Brick of High Quality Ore (Smithing: Yield 1, trivial 29), Combine the following in a Forge, or Tanaan Forge
                ...

Indentation is the structure: a component line belongs to the innermost recipe
opened above it, and a nested "To make" describes the component it sits under.
Sub-recipes are saved too — a paste for one item usually carries a dozen others
with it, and there is no reason to throw them away.

Merging is additive and deduplicated, so re-importing a paste, or importing an
overlapping one, changes nothing. Output is sorted for a readable diff.

Every recipe is tagged with the game it describes. EQTraders documents classic
EverQuest, so that is the default; pass --game eql for a Legends-specific
source. A wrong tag cannot be spotted later, so it is recorded rather than
inferred at read time.

Usage:
    tools/import-recipes.py paste.txt [more.txt ...]
    tools/import-recipes.py --game eql paste.txt
    tools/import-recipes.py --check paste.txt      # parse and report, write nothing
    pbpaste | tools/import-recipes.py -
"""

import argparse
import json
import re
import sys
from pathlib import Path

DEFAULT_OUT = Path(__file__).resolve().parent.parent / (
    "crates/norrlog-core/assets/recipes.json"
)

# Slugs shared with crates/norrlog-core/src/game.rs.
GAMES = {
    "eq": "everquest",
    "everquest": "everquest",
    "classic": "everquest",
    "eql": "everquest-legends",
    "legends": "everquest-legends",
    "everquest legends": "everquest-legends",
}

# EQTraders' names for the tradeskills against the names the client writes in
# its skill-up lines, which are what the rest of the app keys on.
SKILL_ALIASES = {
    "smithing": "Blacksmithing",
    "blacksmithing": "Blacksmithing",
    "alchemy": "Alchemy",
    "baking": "Baking",
    "brewing": "Brewing",
    "fletching": "Fletching",
    "jewelcraft": "Jewelry Making",
    "jewelry making": "Jewelry Making",
    "make poison": "Make Poison",
    "poison making": "Make Poison",
    "pottery": "Pottery",
    "research": "Research",
    "spell research": "Research",
    "fishing": "Fishing",
    "tailoring": "Tailoring",
    "tinkering": "Tinkering",
}

HEADER = re.compile(
    r"^(?:Alternatively:\s*)?To make (?P<product>.+?) "
    r"\((?P<skill>[^:()]+): Yield (?P<yield>\d+), trivial (?P<trivial>\d+)\), "
    r"Combine the following in (?P<containers>.+?)\.?$"
)

# "Small Brick of High Quality Ore (3) (*)" -> name, quantity, vendor-sold.
# The two trailing markers are independent and either may be absent.
COMPONENT = re.compile(
    r"^(?P<item>.+?)"
    r"(?:\s*\((?P<qty>\d+)\))?"
    r"(?P<vendor>\s*\(\*\))?$"
)

# Notes EQTraders puts inline that are not part of an item's name.
NOISE = re.compile(r"\s*\((?:Restricted|subcombines not shown)\)", re.I)

# Annotations EQTraders appends to a product name — "Elixir of Greater
# Concentration (TLP Only)". The client writes the bare name, so a recipe
# keeping the annotation would never match a combine. Kept as a note rather
# than discarded: "TLP Only" is worth knowing.
# Placeable/Portable/Stationary are the same family: EQTraders uses them to tell
# a container's variants apart ("Mixing Bowl (Portable)" vs "(Placeable)"), but
# the client writes the bare item name for the product of a combine.
PRODUCT_NOTES = re.compile(
    r"\s*\((TLP Only|Restricted|Placeable|Portable|Stationary|Expendable"
    r"|subcombines not shown)\)\s*$",
    re.I,
)

# Any other trailing parenthetical is left alone and reported, because
# silently trimming part of a real item name is worse than an odd-looking one.
TRAILING_PAREN = re.compile(r"\s*\(([^()]+)\)\s*$")


def clean_product(name, warn):
    """Split a product name from the annotations EQTraders hangs off it."""
    notes = []
    while True:
        m = PRODUCT_NOTES.search(name)
        if not m:
            break
        notes.append(m.group(1))
        name = name[: m.start()].strip()
    leftover = TRAILING_PAREN.search(name)
    if leftover:
        warn(
            f"product {name!r} ends with {leftover.group(1)!r}, which this tool "
            "does not recognise as an annotation — left in the name, so check it "
            "matches what the client writes"
        )
    return name, notes


def parse_containers(text):
    """"a Forge, Tanaan Forge, or Guild Forge (Placeable)" -> three names."""
    out = []
    for part in text.split(", "):
        part = re.sub(r"^(?:or |a |an )+", "", part.strip())
        if part:
            out.append(part)
    return out


def parse(text, game, warn):
    """Walk the tree, yielding one dict per recipe found at any depth."""
    recipes = []
    # (indent of the "To make" line, its recipe) for every recipe still open.
    stack = []
    for lineno, raw in enumerate(text.splitlines(), 1):
        if not raw.strip():
            continue
        line = NOISE.sub("", raw.rstrip())
        stripped = line.strip()
        indent = len(line) - len(line.lstrip())

        # Anything indented no deeper than an open recipe's header is no longer
        # inside it.
        while stack and stack[-1][0] >= indent:
            stack.pop()

        m = HEADER.match(stripped)
        if m:
            skill = SKILL_ALIASES.get(m.group("skill").strip().lower())
            if skill is None:
                warn(f"line {lineno}: unknown tradeskill {m.group('skill')!r}")
                skill = m.group("skill").strip()
            product, notes = clean_product(m.group("product").strip(), warn)
            recipe = {
                "product": product,
                "skill": skill,
                "game": game,
                "yield": int(m.group("yield")),
                "trivial": int(m.group("trivial")),
                "containers": parse_containers(m.group("containers")),
                "components": [],
            }
            if notes:
                recipe["notes"] = notes
            recipes.append(recipe)
            stack.append((indent, recipe))
            continue

        if not stack:
            # Preamble, headings, the "Full Recipe List:" line and so on.
            continue

        c = COMPONENT.match(stripped)
        if not c or not c.group("item").strip():
            warn(f"line {lineno}: not a component or a recipe: {stripped!r}")
            continue
        comp = {"item": c.group("item").strip(), "qty": int(c.group("qty") or 1)}
        if c.group("vendor"):
            comp["vendor"] = True
        stack[-1][1]["components"].append(comp)

    return recipes


def key(r):
    """Identity of a recipe: same output, inputs and station is the same one.

    Tolerates the partial entries a human writes by hand — a product and a
    tradeskill and nothing else — which must survive a merge untouched rather
    than being confused with a fully specified recipe for the same item.
    """
    return json.dumps(
        [
            r["product"].lower(),
            r["skill"],
            r.get("game", "everquest"),
            r.get("yield"),
            r.get("trivial"),
            sorted(r.get("containers", [])),
            sorted((c["item"].lower(), c.get("qty", 1)) for c in r.get("components", [])),
        ],
        sort_keys=True,
    )


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("files", nargs="+", help="pasted recipe trees, or - for stdin")
    ap.add_argument("-o", "--out", type=Path, default=DEFAULT_OUT)
    ap.add_argument(
        "--game",
        default="eq",
        help="which game these recipes describe: eq (default, what EQTraders "
        "documents) or eql",
    )
    ap.add_argument(
        "--check",
        action="store_true",
        help="parse and report what would be added, but write nothing",
    )
    args = ap.parse_args()

    game = GAMES.get(args.game.strip().lower().replace("-", " ").replace("_", " "))
    if game is None:
        ap.error(f"unknown --game {args.game!r}; expected eq or eql")

    warnings = []
    parsed = []
    for f in args.files:
        text = sys.stdin.read() if f == "-" else Path(f).read_text()
        got = parse(text, game, warnings.append)
        print(f"{f}: {len(got)} recipes", file=sys.stderr)
        parsed.extend(got)

    book = {"note": "", "tools": [], "recipes": []}
    if args.out.exists():
        book.update(json.loads(args.out.read_text()))

    # Entries written before recipes carried a game are EQTraders imports, so
    # they describe classic EverQuest. Backfilled rather than left blank: an
    # untagged row is the thing this is meant to prevent.
    for r in book["recipes"]:
        r.setdefault("game", "everquest")

    seen = {key(r): r for r in book["recipes"]}
    added = 0
    for r in parsed:
        k = key(r)
        if k not in seen:
            seen[k] = r
            added += 1

    book["recipes"] = sorted(
        seen.values(),
        key=lambda r: (
            r["product"].lower(),
            r.get("game", "everquest"),
            r["skill"],
            r.get("trivial", 0),
            key(r),
        ),
    )
    book["tools"] = sorted(book["tools"], key=str.lower)

    for w in warnings:
        print(f"warning: {w}", file=sys.stderr)
    products = {r["product"] for r in book["recipes"]}
    print(
        f"{added} new ({game}), {len(book['recipes'])} recipes total "
        f"covering {len(products)} products",
        file=sys.stderr,
    )
    if args.check:
        print("--check: nothing written", file=sys.stderr)
        return 0

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(book, indent=1, ensure_ascii=False) + "\n")
    print(f"wrote {args.out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
