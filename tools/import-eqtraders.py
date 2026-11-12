#!/usr/bin/env python3
"""Archive tradeskill recipes from eqtraders.com item pages, slowly.

The site is a twenty-year-old community reference whose data is factual — what
a combine takes, its trivial, its yield — gathered from the game itself. It has
been unwell, and the point of this tool is to keep a copy of that data before
it is lost.

# Being a good guest

A struggling server is the thing this must not make worse, so the defaults are
deliberately timid and every knob only makes it slower:

* One request at a time. Never concurrent, and there is no option to be.
* Five seconds between requests by default, with jitter so the pattern is not
  a metronome.
* Adaptive backoff: a slow reply is taken as a server under strain and the
  delay grows for a while afterwards. Errors back off harder.
* It gives up rather than hammers — `--max-failures` consecutive errors ends
  the run.
* Every page is cached on disk, so a second run costs *zero* requests for
  anything already fetched. Resuming after a stop is free.
* Touch the stop file (`<cache>/STOP`) and it finishes the page in flight and
  exits cleanly. No need to kill it.

At the default delay this fetches roughly 700 pages an hour. Sixty thousand
item ids is therefore a matter of days, not minutes — which is the intended
shape of the thing. Split it with `--items` and run it over a week.

# What it takes, and what it leaves

Only the recipe tree from item pages: products, tradeskills, trivials, yields,
containers and components. It does not fetch, parse or store the site's
articles, guides or commentary — that is Niami Denmother's writing rather than
game data, and archiving it is neither necessary here nor ours to do.

The raw HTML is kept in the cache directory. That, not the JSON, is the actual
archive: the parse can be redone from it later without touching the site again.

# Output

Recipes are merged into the same book `tools/import-recipes.py` writes, through
the same parser, with `source` and `retrieved` recorded per recipe so any row
can be traced back to the page it came from.

Usage:
    tools/import-eqtraders.py --items 1-2000
    tools/import-eqtraders.py --items 14578,60252 --delay 10
    tools/import-eqtraders.py --items 1-70000 --cache ~/eqtraders-archive
    tools/import-eqtraders.py --reparse            # re-read the cache, no network
"""

import argparse
import gzip
import http.cookiejar
import importlib.util
import json
import random
import signal
import re
import sys
import time
import urllib.error
import urllib.request
from html import unescape
from html.parser import HTMLParser
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT_OUT = HERE.parent / "crates/norrlog-core/assets/recipes.json"
DEFAULT_CACHE = HERE.parent / ".eqtraders-cache"

BASE = "https://www.eqtraders.com"
HOME = BASE + "/"
ITEM_URL = BASE + "/items/show_item.php?item={id}"

# Identify the tool honestly, and leave a way to be told to stop.
UA = (
    "eqlparser-archive/1.0 (personal archival import of factual recipe data; "
    "one request at a time, 5s apart; +https://github.com/hammackj/norrlog)"
)


def load_recipe_parser():
    """Reuse the EQTraders tree parser rather than writing a second one."""
    path = HERE / "import-recipes.py"
    spec = importlib.util.spec_from_file_location("import_recipes", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class TreeExtractor(HTMLParser):
    """Turn the "Full Recipe List" nested list into the indented text the
    recipe parser reads.

    The page expresses the tree as real nested `<ul>`/`<li>`, so depth is
    structural rather than visual and can be recovered exactly. Text is
    gathered per list item and emitted indented by its nesting depth, which is
    precisely the shape a pasted page has.
    """

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.armed = False       # seen the heading; waiting for the list
        self.depth = 0           # how many <ul> deep
        self.lines = []
        self._buf = []
        self._in_li = False

    def handle_starttag(self, tag, attrs):
        if tag == "ul" and self.armed:
            # Flush *before* descending. A recipe's components sit in a list
            # nested inside the recipe's own <li>, so leaving the header text
            # unflushed would emit it at the components' depth — and a header
            # no shallower than its components makes the tree parser hang them
            # on the wrong recipe.
            self._flush()
            self.depth += 1
        elif tag == "li" and self.depth:
            self._flush()
            self._in_li = True
        elif tag == "br" and self._in_li:
            self._buf.append(" ")

    def handle_endtag(self, tag):
        if tag == "li" and self.depth:
            self._flush()
        elif tag == "ul" and self.depth:
            self._flush()
            self.depth -= 1

    def handle_data(self, data):
        if not self.armed:
            # The heading is the only anchor; everything before it is the
            # item's own description, which is not wanted.
            if "Full Recipe List" in data:
                self.armed = True
            return
        if self._in_li:
            self._buf.append(data)

    def _flush(self):
        if not self._in_li:
            return
        text = re.sub(r"\s+", " ", unescape("".join(self._buf))).strip()
        self._in_li = False
        self._buf = []
        if text:
            self.lines.append("    " * self.depth + text)

    def result(self):
        self._flush()
        return "\n".join(self.lines)


def extract_tree(html):
    """The recipe tree as indented text, or "" when the page has none."""
    if "Full Recipe List" not in html:
        return ""
    p = TreeExtractor()
    try:
        p.feed(html)
    except Exception:
        return ""
    return p.result()


class Fetcher:
    """One-at-a-time HTTP with a session, jittered delay and backoff."""

    def __init__(self, delay, slow_after, max_failures, timeout):
        self.delay = delay
        self.base_delay = delay
        self.slow_after = slow_after
        self.max_failures = max_failures
        self.timeout = timeout
        self.failures = 0
        self.jar = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(self.jar)
        )
        self.opener.addheaders = []
        self._primed = False

    def _sleep(self):
        # Jitter so the request pattern is not a metronome.
        time.sleep(self.delay * random.uniform(0.8, 1.3))

    def _raw(self, url, referer):
        req = urllib.request.Request(
            url,
            headers={
                "User-Agent": UA,
                "Accept": "text/html",
                "Accept-Encoding": "gzip",
                # Deep links are refused without one; the site wants to see
                # you arrived from itself.
                "Referer": referer,
            },
        )
        started = time.monotonic()
        with self.opener.open(req, timeout=self.timeout) as r:
            body = r.read()
            if r.headers.get("Content-Encoding") == "gzip":
                body = gzip.decompress(body)
        return body.decode("utf-8", "replace"), time.monotonic() - started

    def prime(self):
        """Pick up a session from the front page before any deep link."""
        if self._primed:
            return
        self._raw(HOME, BASE)
        self._primed = True
        self._sleep()

    def get(self, url):
        """Fetch one page, or None if it failed. Raises when the run should
        end because the server is plainly unhappy."""
        self.prime()
        try:
            html, took = self._raw(url, HOME)
        except (urllib.error.URLError, urllib.error.HTTPError, OSError) as e:
            self.failures += 1
            # An error is a stronger signal than slowness: back off hard.
            self.delay = min(self.delay * 2, 120)
            say(f"  ! {e} (backing off to {self.delay:.0f}s)")
            if self.failures >= self.max_failures:
                raise SystemExit(
                    f"stopping: {self.failures} failures in a row. The cache is "
                    f"intact — rerun later and it resumes."
                )
            self._sleep()
            return None

        self.failures = 0
        if took > self.slow_after:
            # A slow reply means the server is working hard. Give it room,
            # rather than treating our schedule as its problem.
            self.delay = min(self.delay * 1.5, 120)
            say(f"  . slow reply ({took:.1f}s); delay now {self.delay:.0f}s")
        elif self.delay > self.base_delay:
            # Recover gradually once it is healthy again.
            self.delay = max(self.base_delay, self.delay * 0.9)
        self._sleep()
        return html


def say(msg):
    """Progress goes to stderr and is flushed every time.

    Without the flush a redirected log stays empty for minutes at a stretch,
    and a run that is merely quiet is indistinguishable from one that has
    hung — over a multi-day crawl that matters more than the cost of the
    syscall.
    """
    print(msg, file=sys.stderr, flush=True)


def hms(seconds):
    """A duration a human can read at a glance."""
    seconds = int(max(seconds, 0))
    h, m = divmod(seconds // 60, 60)
    if h >= 24:
        return f"{h // 24}d {h % 24}h"
    return f"{h}h {m:02d}m" if h else f"{m}m {seconds % 60:02d}s"


def parse_items(spec):
    """"1-2000,14578" -> a list of item ids."""
    out = []
    for part in spec.split(","):
        part = part.strip()
        if not part:
            continue
        if "-" in part.lstrip("-"):
            lo, hi = part.split("-", 1)
            out.extend(range(int(lo), int(hi) + 1))
        else:
            out.append(int(part))
    return out


def write_book(args, recipes_mod, parsed, dry_run=False):
    """Merge what has been parsed into the book, additively.

    Shared by the periodic save and the final one, so an interrupted run and a
    completed one produce the same file. Returns (added, total).
    """
    book = {"note": "", "tools": [], "recipes": []}
    if args.out.exists():
        book.update(json.loads(args.out.read_text()))
    for r in book["recipes"]:
        r.setdefault("game", "everquest")

    if args.rebuild and parsed:
        # A parser fix cannot be merged in: the old rows are still there and
        # merging only ever adds. Scraped rows carry a `source`, so they can be
        # dropped and re-derived from the cache, while hand-written entries —
        # which have no source and cannot be regenerated — are kept.
        #
        # Guarded on `parsed` being non-empty: a rebuild whose run produced
        # nothing — stopped early, an empty id list — would otherwise delete
        # every scraped row and write back nothing at all.
        kept = [r for r in book["recipes"] if not r.get("source")]
        dropped = len(book["recipes"]) - len(kept)
        if dropped and not dry_run:
            say(f"  rebuild: dropped {dropped} scraped rows, kept {len(kept)} hand-written")
        book["recipes"] = kept

    seen = {recipes_mod.key(r): r for r in book["recipes"]}
    added = 0
    for r in parsed:
        k = recipes_mod.key(r)
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
            recipes_mod.key(r),
        ),
    )
    if not dry_run:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        # Written via a temporary file: a crawl interrupted mid-write must not
        # leave a truncated book behind.
        tmp = args.out.with_suffix(args.out.suffix + ".tmp")
        tmp.write_text(json.dumps(book, indent=1, ensure_ascii=False) + "\n")
        tmp.replace(args.out)
    return added, len(book["recipes"])


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--items", help="item ids: '1-2000' or '14578,60252'")
    ap.add_argument("--items-from", type=Path, metavar="FILE",
                    help="read item ids from a file, one per line. Most of the "
                         "id space is 'Invalid Item ID', so a list of ids known "
                         "to exist skips three quarters of the requests")
    ap.add_argument("--cache", type=Path, default=DEFAULT_CACHE,
                    help="where raw pages are kept; this is the real archive")
    ap.add_argument("-o", "--out", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--game", default="eq", help="which game these describe (default eq)")
    ap.add_argument("--delay", type=float, default=5.0,
                    help="seconds between requests (default 5; only raise it)")
    ap.add_argument("--slow-after", type=float, default=3.0,
                    help="a reply slower than this triggers backoff")
    ap.add_argument("--max-failures", type=int, default=5,
                    help="consecutive errors before giving up entirely")
    ap.add_argument("--timeout", type=float, default=30.0)
    ap.add_argument("--reparse", action="store_true",
                    help="re-read the cache and rebuild the book; no network at all")
    ap.add_argument("--check", action="store_true",
                    help="parse and report but write no book. NOTE: this still "
                         "fetches — use --dry-run to touch nothing at all")
    ap.add_argument("--dry-run", action="store_true",
                    help="say what would be fetched and how long it would take, "
                         "then stop. No requests, no writes.")
    ap.add_argument("--rebuild", action="store_true",
                    help="re-derive every scraped recipe from the cache instead "
                         "of adding to what is there. Use after a parser fix — "
                         "merging cannot correct rows that are already written. "
                         "Hand-written entries are untouched.")
    ap.add_argument("--slice", metavar="K/N",
                    help="take only every Nth id, offset K (1-based): '1/3' on "
                         "one machine, '2/3' and '3/3' on the others. Interleaved "
                         "rather than split into blocks, so each machine's share "
                         "covers the whole id range and a machine that stops "
                         "early leaves no contiguous hole. Divide --delay by the "
                         "number of machines' worth of politeness you are "
                         "spending: three machines at 5s is one request every "
                         "1.7s to the same server.")
    ap.add_argument("--quiet", action="store_true",
                    help="drop the per-item lines; keep warnings and the summary")
    ap.add_argument("--save-every", type=int, default=100, metavar="N",
                    help="write the book every N items so a long run leaves "
                         "usable output (0 to save only at the end)")
    args = ap.parse_args()

    if not (args.items or args.items_from or args.reparse):
        ap.error("give --items or --items-from, or --reparse to work from the cache alone")
    if args.delay < 1 and not args.reparse:
        ap.error("--delay below 1s is not being a good guest; raise it")

    recipes_mod = load_recipe_parser()
    game = recipes_mod.GAMES.get(args.game.strip().lower().replace("-", " "))
    if game is None:
        ap.error(f"unknown --game {args.game!r}; expected eq or eql")

    pages = args.cache / "pages"
    pages.mkdir(parents=True, exist_ok=True)
    stop_file = args.cache / "STOP"

    # A leftover stop file from the last run would otherwise make this one halt
    # on its first item and look like it simply had nothing to do. Say so and
    # refuse, rather than quietly doing nothing.
    if stop_file.exists():
        say(f"{stop_file} exists, so this run would stop immediately.")
        say("Remove it to continue: rm " + str(stop_file))
        return 2

    def cached_path(item_id):
        # Bucketed so no directory holds sixty thousand files.
        return pages / f"{item_id // 1000:03d}" / f"{item_id}.html"

    ids = set(parse_items(args.items)) if args.items else set()
    if args.items_from:
        ids |= {
            int(line) for line in args.items_from.read_text().split()
            if line.strip().isdigit()
        }
    ids = sorted(ids)

    if args.slice:
        try:
            k, n = (int(x) for x in args.slice.split("/", 1))
        except ValueError:
            ap.error("--slice takes K/N, e.g. 1/3")
        if not 1 <= k <= n:
            ap.error(f"--slice {args.slice}: K must be between 1 and N")
        ids = [i for idx, i in enumerate(ids) if idx % n == k - 1]
        say(f"slice {k}/{n}: {len(ids)} of the ids, interleaved")
    if args.reparse:
        ids = sorted(
            int(p.stem) for p in pages.rglob("*.html") if p.stem.isdigit()
        )
        say(f"reparsing {len(ids)} cached pages, no network")

    if args.dry_run:
        have = sum(1 for i in ids if cached_path(i).exists())
        todo = len(ids) - have
        say(f"{len(ids)} ids · {have} already cached · {todo} to fetch")
        say(f"at {args.delay:g}s apart that is about {hms(todo * args.delay)} "
            f"of fetching, finishing around "
            f"{time.strftime('%a %H:%M', time.localtime(time.time() + todo * args.delay))}")
        say("nothing was fetched and nothing was written")
        return 0

    fetcher = None if args.reparse else Fetcher(
        args.delay, args.slow_after, args.max_failures, args.timeout
    )

    warnings, parsed, fetched, from_cache, empty, dead = [], [], 0, 0, 0, 0
    started = time.monotonic()
    total = len(ids)

    # What the book held before this run. "New" has to mean new since we
    # started, not new since the last periodic save — otherwise saving as we
    # go makes the final tally read zero.
    baseline = set()
    if args.out.exists():
        for r in json.loads(args.out.read_text()).get("recipes", []):
            r.setdefault("game", "everquest")
            baseline.add(recipes_mod.key(r))
    say(f"{total} item ids to visit · cache {args.cache} · "
        f"{'no network (reparse)' if args.reparse else f'{args.delay:g}s between requests'}")

    # Ctrl-C behaves like the stop file rather than like a crash: the page in
    # flight finishes, what has been parsed is written out, and the run can be
    # resumed for free. A second Ctrl-C is the usual hard exit.
    state = {"interrupted": False}

    def on_interrupt(signum, frame):
        signal.signal(signal.SIGINT, signal.SIG_DFL)
        state["interrupted"] = True
        say("\ninterrupted — finishing this page, then saving. Ctrl-C again to force.")

    signal.signal(signal.SIGINT, on_interrupt)

    for n, item_id in enumerate(ids, 1):
        if stop_file.exists():
            say(f"stop file present — stopping cleanly after {n - 1} of {total}")
            break
        if state["interrupted"]:
            say(f"stopped after {n - 1} of {total}")
            break

        path = cached_path(item_id)
        was_cached = path.exists()
        if was_cached:
            html = path.read_text(encoding="utf-8", errors="replace")
            from_cache += 1
        elif args.reparse:
            continue
        else:
            html = fetcher.get(ITEM_URL.format(id=item_id))
            if html is None:
                continue
            path.parent.mkdir(parents=True, exist_ok=True)
            # Written before parsing: the archive is the point, and a parser
            # bug must never cost a fetch.
            path.write_text(html, encoding="utf-8")
            fetched += 1

        # The site answers an unknown id with a stock page. Counting those as
        # "items without recipes" would hide how much of the id space is empty,
        # which is the number that decides whether a sweep is worth its hours.
        invalid = "Invalid Item ID" in html
        if invalid:
            dead += 1

        tree = extract_tree(html)
        got = []
        if tree:
            got = recipes_mod.parse(tree, game, warnings.append)
            for r in got:
                r["source"] = ITEM_URL.format(id=item_id)
                r["retrieved"] = time.strftime("%Y-%m-%d", time.gmtime())
            parsed.extend(got)
        elif not invalid:
            empty += 1

        # One line per item. At a five-second cadence that is about seven
        # hundred lines an hour — cheap to write, and the difference between
        # watching a crawl and wondering about it.
        if not args.quiet:
            elapsed = time.monotonic() - started
            # Only fetches take time; cached items fly past and would flatter
            # an estimate built on the average.
            eta = ""
            remaining = total - n
            if fetched and remaining and not args.reparse:
                # Seconds per *fetch*, applied to everything left. Cached items
                # cost nothing, so this overestimates on a mixed run — the
                # forgiving direction for a progress figure.
                eta = f" · eta {hms(remaining * (elapsed / fetched))}"
            say(
                f"  [{n}/{total}] item {item_id}: "
                + (f"{len(got)} recipes" if got
                   else "invalid id" if invalid else "no recipes")
                + (" (cached)" if was_cached else "")
                + f" · {len(parsed)} total{eta}"
            )

        # Save as we go. A crawl that runs for days should leave usable output
        # if it is interrupted, even though the cache means nothing is ever
        # actually lost.
        if not args.check and args.save_every and n % args.save_every == 0:
            write_book(args, recipes_mod, parsed)
            say(f"  … saved {len(parsed)} recipes so far to {args.out}")

    _, total_recipes = write_book(args, recipes_mod, parsed, dry_run=args.check)
    added = len({recipes_mod.key(r) for r in parsed} - baseline)

    for w in warnings[:20]:
        say(f"warning: {w}")
    if len(warnings) > 20:
        say(f"... and {len(warnings) - 20} more warnings")
    visited = fetched + from_cache
    say(
        f"\n{fetched} fetched, {from_cache} from cache, {dead} invalid ids, "
        f"{empty} items with no recipes, {added} new recipes, "
        f"{total_recipes} in the book (elapsed {hms(time.monotonic() - started)})"
    )
    if dead and visited:
        say(
            f"{dead * 100 // visited}% of the ids visited do not exist. "
            f"A list of ids known to exist, fed in with --items-from, skips those."
        )
    if args.check:
        say("--check: nothing written")
    else:
        say(f"wrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
