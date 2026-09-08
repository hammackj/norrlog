#!/usr/bin/env python3
"""Turn photographs of the Quest Journal into a task's objectives.

`pipeline/scan_quests.py` in the OCR tree finds the Quest Journal in a
screenshot and cuts one crop per table cell; `ocr/read.py` decodes each. This
is the step after that: it merges the readings, groups them under the task
they belong to, and hands the result to the daemon, which is the only place
objectives can be known at all.

    tools/import-tasks.py draft --crops ~/Pictures/eql-items/pipeline/crops_quests_a
    # ... edit tasks.draft.json: name the task, fix the brackets ...
    tools/import-tasks.py send tasks.draft.json

# Why this exists

`task.rs` gets two lines out of the client and no more:

    You have been assigned the task 'What Happened Here?'.
    Your task 'What Happened Here?' has been updated.

No objective text, no "step 2 of 5", no completion message. It says so in its
own header, and says that step definitions "cannot be derived from the log;
they are seeded by hand". Seeding them by hand meant typing them; this reads
them off a picture of the window that states them.

# Several photographs of one task are several readings of each objective

The client reveals a task's steps as you finish them, so a run of screenshots
is the same list growing: 2 objectives named, then 3, 4, 5, 8. Objective *i*
is the same objective in every one of them, so five shots give up to five
readings of the first line and one of the last.

They are merged by position and every distinct reading is kept. Where they
agree, the agreement is real evidence -- five independent decodes of the same
pixels at slightly different JPEG noise. Where they disagree, the draft says
so and the person settles it, which is the same arrangement the item readings
have on `/reviews` and the book pages have in `import-books.py`.

# The brackets are the part to check

`[what happened here]` is not decoration: bracketed keywords are what you say
to the NPC, and `task.rs` extracts them from dialogue for exactly that reason.
They are also the reader's weakest glyphs in this face -- `[` comes back as
`j` or `l` and `]` as `l` or `I`, so `[found]` reads as `jfoundI`. The draft
repairs the obvious cases and flags the line either way, because a keyword
that is wrong is worse than a keyword that is missing: you can see a missing
one.

# The task's name does not come from the crop

The selected row in the task list is drawn inverted -- dark text on the
highlight -- and the reader's mask finds nothing there. That is left alone
rather than worked around, because the log states the name exactly and
transcribing it would be a reading of something already written down.

So the name comes from the daemon, which has the log: where exactly one task
is outstanding it is filled in, and where several are it is left empty for the
person. Guessing between two would file a task's objectives under the other
one, and nothing downstream could tell.
"""

import argparse
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

# The cells `scan_quests.py` writes: `quest<tag>_<shot>_<row>_<column>.png`.
CELL = re.compile(r"^quest[^_]*_(?P<shot>\d+)_(?P<row>\d+)_(?P<col>obj|status|zone)\.png$")

# Where the daemon listens, and the two endpoints this needs: one to read the
# assigned tasks off the log, one to seed a definition.
DAEMON = "http://127.0.0.1:8777"

# A `Kill <something>` objective names its target, and `TaskStepDef.kill_target`
# is what lets an update be attributed to a step rather than counted. Only this
# one form, and only from the start of the line: "Kill the Abomination of Ro"
# names a creature and "Find a scroll that details who cursed the forest" does
# not, however much of a kill it turns into.
KILL = re.compile(r"^Kill (?:the |a |an )?(?P<who>.+?)\.?$", re.I)

# `[` and `]` in this face, as the reader hands them back. Repaired rather than
# left, because a bracketed keyword is a thing you type into the game and a
# wrong one is silently useless -- see the module docstring.
#
# Anchored on the bracket's neighbours rather than applied to every `j` and
# `l`: `jfoundI.` is a keyword and `just left` is a sentence.
OPENERS = "jl!I["
CLOSERS = "lI1J]"


def unbracket(text):
    """-> (repaired text, whether anything was repaired).

    A keyword runs from a bracket-shaped glyph after a space to the next one
    before a space or a full stop. Only pairs are repaired: a lone opener is
    left exactly as it decoded, because inventing the other half would make up
    the extent of a keyword the reader could not see.
    """
    out, fixed = [], False
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        opens = c in OPENERS and (i == 0 or text[i - 1] == " ")
        if not opens:
            out.append(c)
            i += 1
            continue
        # Find the closer: a bracket-shaped glyph at the end of a word.
        j = i + 1
        end = None
        while j < n:
            if text[j] in CLOSERS and (j + 1 == n or text[j + 1] in " ."):
                end = j
                break
            if text[j] == "." or (j - i) > 60:
                break
            j += 1
        if end is None:
            out.append(c)
            i += 1
            continue
        out.append("[")
        out.append(text[i + 1:end])
        out.append("]")
        fixed = fixed or c != "[" or text[end] != "]"
        i = end + 1
    return "".join(out), fixed


def one_line(crops, name, reader):
    """-> the single decoded line of a cell, or None.

    A cell is one row of the table and so one line. More than one means the
    row bands ran together, and taking the first would quietly file half an
    objective.
    """
    lines = reader.read_crop(os.path.join(crops, name), find_window=False)
    lines = [l for l in lines if l.get("text", "").strip()]
    if len(lines) != 1:
        if len(lines) > 1:
            print(f"  {name}: {len(lines)} lines in one cell, skipped",
                  file=sys.stderr)
        return None
    return lines[0]


def cells(crops, reader):
    """-> {shot: {row: {'text','score','done','zone'}}} for every cell.

    The Zone column is read as well as the objective. It is not part of what
    an objective *says* -- the daemon's `TaskStepDef` has no field for it --
    but it is what the site's quest builder fills the quest's zone in from,
    and it comes off the same window at the same time. Read here rather than
    there because the reader is here.
    """
    seen = {}
    for name in sorted(os.listdir(crops)):
        m = CELL.match(name)
        if not m or m["col"] not in ("obj", "zone"):
            continue
        if m["col"] == "zone":
            # Attached to the objective beside it, and dropped where that
            # objective did not decode: a zone with no objective is a column
            # with nothing to be a column of.
            line = one_line(crops, name, reader)
            row = seen.get(m["shot"], {}).get(int(m["row"]))
            if line and row:
                row["zone"] = line["text"].strip()
            continue
        lines = reader.read_crop(os.path.join(crops, name), find_window=False)
        lines = [l for l in lines if l.get("text", "").strip()]
        if not lines:
            continue
        if len(lines) > 1:
            # A cell is one row of the table and so one line. More than one
            # means the row bands ran together, and taking the first would
            # quietly file half an objective.
            print(f"  {name}: {len(lines)} lines in one cell, skipped",
                  file=sys.stderr)
            continue
        line = lines[0]
        seen.setdefault(m["shot"], {})[int(m["row"])] = {
            "text": line["text"].strip(),
            "score": round(float(line.get("score", 0.0)), 3),
            # Green is done, white is outstanding. Taken from the colour and
            # not from the Status column beside it: `Done` decodes and `0/1`
            # does not, and the colour says the same thing for both.
            "done": line.get("colour") == "green",
            "zone": "",
            "evidence": name,
        }
    return seen


def merged(seen):
    """-> [{'readings': [...], 'agreed': bool, ...}] by objective position.

    Position, because the client reveals a task's steps in order and row *i*
    is the same objective in every shot of it.
    """
    by_row = {}
    for shot, rows in seen.items():
        for i, cell in rows.items():
            by_row.setdefault(i, []).append(dict(cell, shot=shot))
    out = []
    for i in sorted(by_row):
        rs = by_row[i]
        # Repaired first, then compared. Two readings of the same keyword can
        # differ only in which glyph the reader picked for the bracket --
        # `jwhat happened herel` against `lwhat happened hereI.` -- and those
        # are not two answers about what the objective says. Comparing the raw
        # readings called four of the eight steps here a disagreement and
        # three of them were this.
        for r in rs:
            r["fixed"], r["repaired"] = unbracket(r["text"])
        texts = {}
        for r in rs:
            texts.setdefault(r["fixed"], []).append(r)
        # The reading most photographs agree on, and the best-scoring one only
        # to break a tie. Five shots of an early objective are five decodes of
        # the same pixels through different JPEG noise, and what most of them
        # say is better evidence than what any one of them scored.
        best = max(texts.values(),
                   key=lambda g: (len(g), max(r["score"] for r in g)))
        pick = max(best, key=lambda r: r["score"])
        out.append({
            "step": i,
            "text": pick["fixed"],
            # What the reader actually returned, so a repair can be checked
            # against what was on the screen.
            "as_read": pick["text"],
            "brackets_repaired": pick["repaired"],
            "done": any(r["done"] for r in rs),
            # The zone the photographs agree on, and blank where they do not.
            # The column is narrow and truncates a long zone name, so a
            # disagreement here is usually two truncations of one name --
            # which is exactly the case where guessing would be wrong.
            "zone": pick.get("zone", ""),
            "score": pick["score"],
            "readings": len(rs),
            "votes": len(best),
            "agreed": len(texts) == 1,
            # Only where they differ: listing one reading five times would
            # bury the case this is here to show.
            "disagreement": sorted(texts) if len(texts) > 1 else [],
            "evidence": sorted(r["evidence"] for r in rs),
        })
    return out


def assigned_task():
    """-> the name of the one outstanding task, or None.

    None where the daemon is not running or where it is holding more than
    one: filing a task's objectives under the wrong task is not something
    anything downstream could notice.
    """
    try:
        with urllib.request.urlopen(f"{DAEMON}/api/tasks", timeout=5) as fh:
            doc = json.load(fh)
    except (urllib.error.URLError, OSError, ValueError):
        return None
    open_ = [t["name"] for t in doc.get("tasks", []) if t.get("still_to_do")]
    return open_[0] if len(open_) == 1 else None


def draft(args):
    """Read every cell and write a file for somebody to correct."""
    sys.path.insert(0, os.path.join(args.ocr, "ocr"))
    sys.path.insert(0, os.path.join(args.ocr, "pipeline"))
    try:
        import read as reader
    except ImportError as e:
        raise SystemExit(
            f"import-tasks: cannot import the reader from {args.ocr} ({e}).\n"
            f"  --ocr should point at the eql-items tree."
        )

    seen = cells(args.crops, reader)
    if not seen:
        raise SystemExit(f"import-tasks: no objective cells in {args.crops}")
    steps = merged(seen)
    name = args.task or assigned_task() or ""

    out = {
        "note": (
            "Draft task objectives, decoded from photographs of the Quest "
            "Journal and not yet corrected. Set `task` to the name the log "
            "uses, check every line whose `agreed` is false or whose "
            "`brackets_repaired` is true, then run `import-tasks.py send`. "
            "`as_read` is what the reader returned before the brackets were "
            "repaired; neither it nor `score` is sent."
        ),
        "task": name,
        "crops": os.path.abspath(args.crops),
        "steps": steps,
    }
    with open(args.out, "w", encoding="utf-8") as fh:
        json.dump(out, fh, indent=2, ensure_ascii=False)

    print(f"{len(steps)} objective(s) from {len(seen)} photograph(s) -> {args.out}")
    if name:
        print(f"  task: {name}"
              + ("" if args.task else " (the one task the daemon has outstanding)"))
    else:
        print("  task: not set. The name is not on the crop -- the selected row"
              "\n        is drawn inverted and does not read -- and the daemon"
              "\n        is holding none or several. Set it by hand.")
    dis = [s for s in steps if not s["agreed"]]
    rep = [s for s in steps if s["brackets_repaired"]]
    print(f"  {sum(1 for s in steps if s['done'])} done, "
          f"{sum(1 for s in steps if not s['done'])} outstanding")
    print(f"  {len(dis)} line(s) two photographs disagree about; "
          f"{len(rep)} with a bracket repaired -- check those first")
    for s in dis:
        print(f"    step {s['step']}: {' | '.join(s['disagreement'])}")
    return 0


def send(args):
    """Hand a corrected draft to the daemon."""
    with open(args.draft, encoding="utf-8") as fh:
        doc = json.load(fh)
    name = (doc.get("task") or "").strip()
    if not name:
        # Refused rather than guessed. See the module docstring: a task's
        # objectives filed under the wrong task are invisible from every side.
        raise SystemExit(
            "import-tasks: the draft has no `task`. It is the name the log "
            "uses --\n  `You have been assigned the task 'X'` -- and nothing "
            "on the crop states it."
        )
    steps = []
    for s in doc.get("steps", []):
        text = (s.get("text") or "").strip()
        if not text:
            continue
        m = KILL.match(text)
        steps.append({
            "description": text,
            # Only a kill step carries a target, and it is what lets an
            # update be attributed to a step instead of counted. See
            # `TaskTracker::only_candidate`.
            "kill_target": m["who"].strip() if m else None,
        })
    if not steps:
        raise SystemExit("import-tasks: the draft has no objectives in it")

    body = json.dumps({"name": name, "steps": steps}).encode()
    req = urllib.request.Request(
        f"{args.daemon}/api/tasks", data=body,
        headers={"Content-Type": "application/json"}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=10) as fh:
            fh.read()
    except urllib.error.URLError as e:
        raise SystemExit(
            f"import-tasks: could not reach the daemon at {args.daemon} ({e}).\n"
            f"  It is what holds the definitions; they are written to\n"
            f"  ~/.config/eqlparser/tasks.json when it takes one."
        )
    named = sum(1 for s in steps if s["kill_target"])
    print(f"sent {len(steps)} objective(s) for {name!r}")
    print(f"  {named} name a creature, so an update can be attributed rather "
          f"than counted")
    return 0


def push(args):
    """Put a corrected draft in front of a person on the site.

    `send` hands the objectives to the daemon, which is where a *running*
    parser needs them. This hands the same objectives to eqldb, where
    somebody writes the walkthrough: the quest builder opens on them, with the
    picture above the form, and what comes out is a quest rather than a task
    definition. The two are not alternatives -- the daemon needs the
    definitions to attribute an update, and the site needs the walkthrough --
    and a draft is normally sent to both.

    # The window, not the cells

    A cell is twelve pixels tall. Eight of them stacked on a review page show
    a person eight grey strips and nothing about which task, which row, or
    what the Status column said. `scan_quests.py` cuts the whole window for
    this, and the reading cites the fullest one -- the shot with the most
    objectives revealed, because the client reveals them as you go and the
    last photograph is the one that shows the most.
    """
    with open(args.draft, encoding="utf-8") as fh:
        doc = json.load(fh)
    name = (doc.get("task") or "").strip()
    if not name:
        raise SystemExit(
            "import-tasks: the draft has no `task`. It is the name the log "
            "uses --\n  `You have been assigned the task 'X'` -- and nothing "
            "on the crop states it."
        )
    steps = [s for s in doc.get("steps", []) if (s.get("text") or "").strip()]
    if not steps:
        raise SystemExit("import-tasks: the draft has no objectives in it")

    sys.path.insert(0, os.path.join(args.ocr, "ocr"))
    try:
        import site_api
    except ImportError as e:
        raise SystemExit(
            f"import-tasks: cannot import the site client from {args.ocr} ({e}).\n"
            f"  --ocr should point at the eql-items tree."
        )

    crops = args.crops or doc.get("crops") or ""
    win = fullest_window(crops, steps)
    if win is None and not args.no_picture:
        raise SystemExit(
            f"import-tasks: no window crop in {crops or '(nowhere -- the draft '\
            'names no crops directory)'}.\n"
            "  `scan_quests.py` writes one per shot as `quest<tag>_<shot>_win.png`;\n"
            "  re-run it, or pass --no-picture to send the text alone."
        )

    record = {
        "task": name,
        "steps": [{
            "text": s["text"].strip(),
            "zone": (s.get("zone") or "").strip(),
            "done": bool(s.get("done")),
            "score": s.get("score"),
        } for s in steps],
    }
    if args.dry_run:
        print(f"would send {len(steps)} objective(s) for {name!r}"
              + (f" with {os.path.basename(win)}" if win else " with no picture"))
        return 0

    shot_key, fmt = "", "png"
    if win:
        with open(win, "rb") as fh:
            png = fh.read()
        # The picture first, so a reading is never stored citing a picture
        # that failed to arrive. Same order and same reason as `ocr/push.py`.
        _st, shot = site_api.call(
            "POST", "/api/shots", body=png, ctype="application/octet-stream",
            headers={"X-Shot-Name": os.path.basename(win)})
        shot_key = shot.get("shot_key") or ""
        fmt = shot.get("format") or "png"

    reading = {
        # The window this was read off, which is also the key the site
        # deduplicates on: pushing the same journal window twice hands
        # somebody a task they have already written up.
        "crop": os.path.basename(win) if win else f"{store_key(name)}.txt",
        "record": record,
        # Per-objective confidence, in the shape the site renders: the step's
        # own number as the field name, because an objective has no name of
        # its own and its position is what identifies it.
        "fields": {
            f"step{s['step']}": {
                "value": s["text"],
                "score": s.get("score"),
                "raw": s.get("as_read", s["text"]),
                "accept": bool(s.get("agreed")),
            } for s in steps
        },
        "needs_review": [f"step{s['step']}" for s in steps if not s.get("agreed")],
        "shot_key": shot_key,
        "shot_format": fmt,
        "read_at": int(time.time()),
    }
    try:
        _st, out = site_api.call("POST", "/api/readings", body=reading,
                                 query={"again": "1"} if args.again else None)
    except site_api.Refused as e:
        raise SystemExit(f"import-tasks: the site refused it: {e}")
    print(f"{out.get('status', 'queued')}: {len(steps)} objective(s) for {name!r}")
    print(f"  {site_api.base()}/quest-builder — it is waiting there, with the "
          f"picture above the form")
    return 0


def store_key(name):
    """The site's own folding, for the one place this needs a filename."""
    return "".join(c if c.isalnum() else "_" for c in name.lower())[:60] or "task"


def fullest_window(crops, steps):
    """-> the path of the window crop showing the most objectives, or None.

    The client reveals a task's steps as you finish them, so the shot with
    the most rows is the most complete picture of the window -- and it is the
    one whose bottom rows nothing else photographed.
    """
    if not crops or not os.path.isdir(crops):
        return None
    wins = sorted(n for n in os.listdir(crops) if n.endswith("_win.png"))
    if not wins:
        return None
    # How many objectives each shot contributed, from the evidence the draft
    # carries: `questt_004413_03_obj.png` names its shot.
    rows = {}
    for s in steps:
        for ev in s.get("evidence", []):
            m = CELL.match(ev)
            if m:
                rows[m["shot"]] = rows.get(m["shot"], 0) + 1
    def score(n):
        shot = n.split("_")[1] if "_" in n else ""
        return (rows.get(shot, 0), shot)
    return os.path.join(crops, max(wins, key=score))


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = ap.add_subparsers(dest="cmd", required=True)

    d = sub.add_parser("draft", help="decode the cells into a file to correct")
    d.add_argument("--crops", required=True, help="a crops_quests_* directory")
    d.add_argument("--ocr", default=os.path.expanduser("~/Pictures/eql-items"),
                   help="the eql-items tree, for its reader")
    d.add_argument("--task", default="", help="the task's name, if you know it")
    d.add_argument("-o", "--out", default="tasks.draft.json")
    d.set_defaults(fn=draft)

    s = sub.add_parser("send", help="hand a corrected draft to the daemon")
    s.add_argument("draft")
    s.add_argument("--daemon", default=DAEMON)
    s.set_defaults(fn=send)

    u = sub.add_parser("push", help="put a corrected draft on the site to be "
                                    "written up as a quest")
    u.add_argument("draft")
    u.add_argument("--crops", default="", help="the crops directory, if the "
                                               "draft does not name it")
    u.add_argument("--ocr", default=os.path.expanduser("~/Pictures/eql-items"),
                   help="the eql-items tree, for its site client")
    u.add_argument("--again", action="store_true",
                   help="replace a reading the site already holds, where "
                        "nobody has written it up yet")
    u.add_argument("--no-picture", action="store_true",
                   help="send the objectives without the window they came off")
    u.add_argument("--dry-run", action="store_true")
    u.set_defaults(fn=push)

    args = ap.parse_args()
    return args.fn(args)


if __name__ == "__main__":
    raise SystemExit(main())
