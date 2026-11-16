#!/usr/bin/env python3
"""Write THIRD-PARTY-LICENSES.txt for one release target.

    packaging/licenses.py TARGET OUT_FILE

TARGET is a Rust target triple (x86_64-unknown-linux-gnu,
x86_64-pc-windows-msvc, aarch64-apple-darwin). Needs cargo-about
(`cargo install --locked --features cli cargo-about`).

The list is made from what the two shipped binaries, norrlog and norrlogd,
actually link for that target: cargo-about walks each one's resolved
dependency graph, leaving out dev and build dependencies (about.toml), and
the two are merged. Where a crate offers a choice of licences, the one taken
is the first about.toml accepts, and it is named beside the crate.

Besides the crates' own licences it carries what they do not: SQLite, built
into the daemon from source, is in the public domain, and the MPL-2.0 crates
are listed with where their source is, as the MPL asks.
"""

import json
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SHIPPED = ["crates/norrlog/Cargo.toml", "crates/norrlogd/Cargo.toml"]


def about(manifest, target):
    out = subprocess.run(
        ["cargo", "about", "generate", "--locked", "--fail", "--format", "json",
         "-c", str(ROOT / "about.toml"), "-m", str(ROOT / manifest),
         "--target", target],
        check=True, capture_output=True, cwd=ROOT,
    ).stdout
    return json.loads(out)


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    target, out_file = sys.argv[1], Path(sys.argv[2])

    # (licence id, text) -> crates using it under that licence.
    groups = defaultdict(set)
    names = {}
    crates = {}
    for manifest in SHIPPED:
        data = about(manifest, target)
        for c in data["crates"]:
            p = c["package"]
            if (p.get("source") or "").startswith("registry+"):
                crates[(p["name"], p["version"])] = (c.get("license") or "", p.get("repository") or "")
        for lic in data["licenses"]:
            key = (lic["id"], lic["text"].strip())
            names[lic["id"]] = lic["name"]
            for u in lic["used_by"]:
                cr = u["crate"]
                if (cr.get("source") or "").startswith("registry+"):
                    groups[key].add((cr["name"], cr["version"]))

    used = sorted(crates)
    w = []
    w.append("Norrlog: third-party software\n")
    w.append(f"Target: {target}\n\n")
    w.append(
        "Norrlog itself is under the MIT licence; see LICENSE beside this file.\n"
        "The programs also contain the open-source components below, each\n"
        "under its own licence. Where a component offers a choice of licences,\n"
        "it is used here under the one named beside it.\n\n"
    )
    w.append(f"Components ({len(used)}):\n")
    for name, ver in used:
        expr, _ = crates[(name, ver)]
        taken = sorted({i for (i, _t), cs in groups.items() if (name, ver) in cs})
        w.append(f"  {name} {ver}  ({expr}; used under {', '.join(taken) or expr})\n")

    mpl = [(n, v) for (n, v) in used if any(
        i == "MPL-2.0" and (n, v) in cs for (i, _t), cs in groups.items())]
    if mpl:
        w.append("\nSource code of MPL-2.0 components\n")
        w.append(
            "These are used unmodified. Their source is available from crates.io\n"
            "and their repositories:\n"
        )
        for n, v in mpl:
            repo = crates[(n, v)][1]
            w.append(f"  {n} {v}: https://crates.io/crates/{n}/{v}")
            w.append(f"  {repo}\n" if repo else "\n")

    if any(n == "libsqlite3-sys" for n, _ in used):
        w.append(
            "\nSQLite\n"
            "The daemon is built with SQLite (https://sqlite.org), compiled in\n"
            "from source by libsqlite3-sys. SQLite is in the public domain.\n"
        )

    w.append("\n" + "=" * 72 + "\nLicence texts\n" + "=" * 72 + "\n")
    for (lid, text), cs in sorted(groups.items(), key=lambda g: (g[0][0], sorted(g[1]))):
        w.append(f"\n{names[lid]} ({lid})\nUsed by: ")
        w.append(", ".join(f"{n} {v}" for n, v in sorted(cs)))
        w.append("\n" + "-" * 72 + "\n" + text + "\n")

    out_file.parent.mkdir(parents=True, exist_ok=True)
    out_file.write_text("".join(w), encoding="utf-8")
    print(f"{out_file}: {len(used)} components, {len(groups)} licence texts")


if __name__ == "__main__":
    main()
