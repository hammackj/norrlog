#!/usr/bin/env python3
"""Import Plane of Sky item reference data from eqlwiki.com.

The wiki runs MediaWiki, so this uses its API rather than scraping pages:
titles batch fifty at a time, which turns 223 lookups into five requests.

Output is a JSON map of lowercased item name to {name, url, icon, stats},
written to the daemon's assets so it ships with the build. The daemon never
touches the network itself — this runs when the data needs refreshing, and its
output is committed.

Icons are downloaded and inlined as data URIs rather than linked. Linking would
mean the app fetching from eqlwiki.com every time a tooltip renders: it would
need the renderer's content-security-policy opened up to a third-party host,
would break offline, and would tell the wiki whenever anyone opened the tab.
Three hundred kilobytes of PNG is a fair price for none of that.

Usage:
    tools/import-wiki.py [--sky http://127.0.0.1:8777/api/sky] [-o PATH]
"""

import argparse
import json
import re
import sys
import time
import base64
import urllib.error
import urllib.parse
import urllib.request

API = "https://eqlwiki.com/api.php"
# Identify the tool, as a courtesy to whoever runs the wiki.
UA = "eqlparser-wiki-import/1.0 (+https://github.com/hammackj/eqlparser)"
# MediaWiki's limit for anonymous multi-title queries.
BATCH = 50
# Between batches. The whole run is five requests; there is no reason to rush.
DELAY = 1.0


def api(params: dict) -> dict:
    params = {**params, "format": "json"}
    url = f"{API}?{urllib.parse.urlencode(params)}"
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def batches(seq, n):
    for i in range(0, len(seq), n):
        yield seq[i : i + n]


def strip_wiki_markup(text: str) -> str:
    """Reduce a stats block to plain lines.

    The blocks are hand-written HTML fragments inside wikitext, so this only
    needs to handle <br>, links and bold — not a general parser.
    """
    text = re.sub(r"<br\s*/?>", "\n", text, flags=re.I)
    text = re.sub(r"\[\[([^|\]]+)\|([^\]]+)\]\]", r"\2", text)
    text = re.sub(r"\[\[([^\]]+)\]\]", r"\1", text)
    text = re.sub(r"'''?", "", text)
    text = re.sub(r"<[^>]+>", "", text)
    lines = [ln.strip() for ln in text.splitlines()]
    return "\n".join(ln for ln in lines if ln)


def field(wikitext: str, name: str) -> str:
    """Pull one field out of the Itempage template.

    Fields run to the next `|field =` at the start of a line, or the closing
    braces — a plain regex to the next `|` would truncate anything containing
    one.
    """
    m = re.search(
        rf"\|\s*{name}\s*=\s*(.*?)(?=\n\s*\|\s*\w[\w_]*\s*=|\n?\}}\}})",
        wikitext,
        re.S,
    )
    return m.group(1).strip() if m else ""


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--sky", default="http://127.0.0.1:8777/api/sky")
    ap.add_argument("-o", "--out", default="crates/eqlparserd/assets/wiki-items.json")
    args = ap.parse_args()

    with urllib.request.urlopen(args.sky, timeout=30) as r:
        sky = json.load(r)
    names = set()
    for q in sky["quests"]:
        names.update(i["name"] for i in q["items"])
        names.add(q["reward"])
    names = sorted(names)
    print(f"{len(names)} unique items", file=sys.stderr)

    out, icon_wanted = {}, {}
    for i, chunk in enumerate(batches(names, BATCH), 1):
        d = api(
            {
                "action": "query",
                "titles": "|".join(chunk),
                "prop": "info|revisions",
                "inprop": "url",
                "rvprop": "content",
                "rvslots": "main",
                "redirects": 1,
            }
        )
        q = d.get("query", {})
        # A redirect means the title we asked for is not the title we got.
        alias = {r["to"]: r["from"] for r in q.get("redirects", [])}
        for pid, p in q.get("pages", {}).items():
            title = p.get("title", "")
            asked = alias.get(title, title)
            if pid == "-1":
                continue
            wt = ""
            try:
                wt = p["revisions"][0]["slots"]["main"]["*"]
            except (KeyError, IndexError):
                pass
            rec = {
                "name": title,
                "url": p.get("fullurl", ""),
                "stats": strip_wiki_markup(field(wt, "statsblock")),
                "icon": "",
            }
            out[asked.lower()] = rec
            if title.lower() != asked.lower():
                out[title.lower()] = rec
            img = field(wt, "lucy_img_ID")
            if img.isdigit():
                icon_wanted.setdefault(f"File:Item {img}.png", []).append(rec)
        print(f"  batch {i}: {len(out)} known", file=sys.stderr)
        time.sleep(DELAY)

    # Resolve icons in a second pass, batched the same way.
    icon_urls = {}
    files = sorted(icon_wanted)
    for i, chunk in enumerate(batches(files, BATCH), 1):
        d = api(
            {
                "action": "query",
                "titles": "|".join(chunk),
                "prop": "imageinfo",
                "iiprop": "url",
                "iiurlwidth": 48,
            }
        )
        for p in d.get("query", {}).get("pages", {}).values():
            info = (p.get("imageinfo") or [{}])[0]
            url = info.get("thumburl") or info.get("url") or ""
            if url:
                icon_urls[p.get("title", "")] = url
        print(f"  icons batch {i}", file=sys.stderr)
        time.sleep(DELAY)

    # Fetch each icon once and inline it, so the app needs no network at all.
    blobs = {}
    for i, (title, url) in enumerate(sorted(icon_urls.items()), 1):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": UA})
            with urllib.request.urlopen(req, timeout=30) as r:
                data = r.read()
            mime = r.headers.get("Content-Type", "image/png").split(";")[0]
            blobs[title] = f"data:{mime};base64," + base64.b64encode(data).decode()
        except (urllib.error.URLError, OSError) as e:
            print(f"  icon failed {title}: {e}", file=sys.stderr)
        if i % 25 == 0:
            print(f"  icons {i}/{len(icon_urls)}", file=sys.stderr)
        time.sleep(0.15)
    for title, blob in blobs.items():
        for rec in icon_wanted.get(title, []):
            rec["icon"] = blob

    missing = [n for n in names if n.lower() not in out]
    with_icon = sum(1 for r in out.values() if r["icon"])
    with open(args.out, "w") as f:
        json.dump(out, f, indent=1, sort_keys=True)
        f.write("\n")
    print(
        f"wrote {args.out}: {len(out)} entries, {with_icon} with icons, "
        f"{len(missing)} not found on the wiki",
        file=sys.stderr,
    )
    for n in missing:
        print(f"  missing: {n}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
