# eqlparser

A log parser, DPS meter, and stream overlay for **EverQuest Legends**.

Rust does the parsing and analysis; Electron provides the desktop GUI; the
overlay is a plain web page served over localhost so OBS and the desktop can
consume the same thing.

A 48 MB / 595,000-line log parses in **~1.4 seconds**, so there is no database:
the daemon re-reads the log on start and keeps everything in memory. After that
it follows the file live, picking up new lines within **~250 ms** (measured:
225–258 ms) and pushing them to the GUI and overlay over WebSocket.

---

## What it does

**Real-time following.** The daemon reads the whole log for history, then polls
every 250 ms for appends. Partial lines are withheld until their terminator
arrives (the client flushes mid-line), CRLF is handled, and truncation or
replacement — a new session reusing the same filename — is detected and
restarted from the top rather than seeking past the end. Every update is pushed
to connected clients over WebSocket, so the overlay and GUI never poll.

**DPS by encounter.** Combat is segmented into encounters automatically (a
12-second lull ends a fight; zoning always does). Per-actor damage, DPS, HPS,
crit rate, accuracy, damage taken, deaths, and a full per-ability breakdown with
average/max/crit for every spell, song, and melee skill.

**Charm tracking.** Charmed creatures are identified from the pet-attack
announcement (`… told you, 'Attacking … Master.'` — only something you control
calls you Master) and released when the charm song wears off. Their damage is
credited to you *while charmed only*, reported separately from your personal
damage so neither number is misleading.

**Pets, and whose they are.** A summoned pet answers that same announcement
word for word, so the line proves ownership but not the kind. Three things
resolve it: the spell database confirming a charm actually landed, the naming
convention (`Brennagh`s warder`), and `/pet leader`, whose reply — `Xabarn
says, 'My leader is Brennagh.'` — is the only line that attributes a pet
outright. That last one also covers other players' pets, so a group meter can
name the owner instead of listing unfamiliar creatures. A pet with a generated
name that has never been asked is labelled just **pet**: it fights for you and
its damage counts, but nothing in the log says how it came to be yours, and
claiming a charm would invent a spell that was never cast. Type `/pet leader`
once and it is pinned down for the rest of the session.

All of a summoned pet's damage counts toward its owner, because it was never
anything else; a charmed mob only counts for the time it was held.

**Buff timers.** Real durations, not guesses. The game ships `spells_us.txt`
(duration formula + cap per spell) and `spells_us_str.txt` (the flavour text
shown when an effect lands and expires), so the parser works backwards from
"Your feet move faster." to the actual spell and computes its duration for your
current level.

**Loot and drops.** Every looted item with its source mob, zone, quantity, and
sale value, aggregated into drop tallies. All four loot line forms are handled,
plus coin from corpses.

**Class detection.** EverQuest Legends characters hold three classes at once,
and the combination changes over a character's life. Classes are inferred from
the spellbook — the game's own data records which of the 16 classes may cast
each spell, so a single-class spell is conclusive — plus class-defining skills,
which is the only way pure-melee classes (warrior, rogue, monk, berserker) can
be detected at all. Evidence is timestamped and scored within a window, so a
class you have since dropped does not linger.

**Historical browser.** Search and filter every encounter by mob, zone, date, or
minimum duration; open any fight for its full breakdown.

**Fight timeline.** Totals say what you did; the timeline says when. Every one
of your actions is kept in order for the most recent fights and drawn as one
lane per ability — hits, crits, misses and resists on a time axis — so a burn
window, a proc chain firing together, or a dot resisted halfway through is a
shape rather than a number. Long fights state how much they had to drop.

**Fight, session, or zone.** The same meter, healing and damage-taken panels
answer for one pull, for everything fought since you logged in, or for the
current zone. Rates in the wider scopes are per second *of combat* — an
evening is mostly running about, and dividing by wall clock describes nothing.

**Sound alerts.** Rules that watch the log and make a noise: a substring of a
line, a regular expression, a typed happening (a charm broke, a raid target
died, someone sent you a tell), or a buff crossing a threshold. Build one by
clicking a line you just watched scroll past. Sounds are synthesised rather
than shipped, so there is nothing to download, and anything can also be spoken
aloud. Nothing fires while the backlog is being read, so starting the app is
silent no matter what is in the log.

---

## Building

```sh
cargo build --release          # the daemon and CLI tools
cd gui && npm install          # the desktop app
```

## Running

The desktop app starts the daemon for itself:

```sh
cd gui && npm start
```

Set the log file and game directory in **Settings** on first run. The game
directory is what enables buff timers and class detection — it needs
`spells_us.txt`.

To run the daemon alone:

```sh
./target/release/eqlparserd \
  --log  "…/EverQuest Legends/Logs/eqlog_<Character>_<server>.txt" \
  --game-dir "…/EverQuest Legends"
```

Then open <http://127.0.0.1:8777>.

## Overlay

Add an **OBS Browser Source** pointing at:

```
http://127.0.0.1:8777/overlay
```

About 360×420, transparent background. The page paints no background of its
own, so it composites cleanly. **Show overlay** in the GUI opens the same page
as a frameless, always-on-top, click-through desktop window.

| Parameter | Effect |
|---|---|
| `?panels=meter,buffs` | Which panels to show |
| `?rows=6` | Max damage meter rows |
| `?buffs=8` | Max buff timers |
| `?scale=1.25` | Zoom, for 4K captures |
| `?width=340` | Width in pixels |
| `?self=1` | Show only your own row |
| `?enemies=0` | Hide enemy rows |
| `?crit=0` | Hide the crit % column in the meter |
| `?scope=session` | Meter the whole session instead of the current fight (`zone` for the current zone) |
| `?idle=1` | Keep the last fight on screen between pulls |

## CLI tools

```sh
# Encounter report: top fights, ability breakdowns, charm contribution, totals
./target/release/eqlreport <logfile> [game-dir]

# Parser coverage: what fraction of lines are understood, and what is not.
# The unmatched histogram is the tool for growing the rule table.
./target/release/eqlcoverage <logfile> [top-n] [game-dir]
```

## API

| Endpoint | Purpose |
|---|---|
| `GET /api/snapshot` | Current state: encounter, meter, buffs, classes, totals |
| `GET /api/encounters` | History — `?search=`, `?zone=`, `?since=`, `?until=`, `?min_duration=`, `?limit=`, `?offset=` |
| `GET /api/encounters/:id` | Full breakdown for one fight |
| `GET /api/loot` | Aggregated loot |
| `GET /api/classes` | Detected classes with evidence |
| `GET /api/session` | Every fight of the session as one meter — `?scope=zone` for the current zone |
| `GET /api/alerts` · `POST` | Alert rules; `POST /api/alerts/delete` removes one |
| `GET /api/lines` | Recent log lines — `?since=` a sequence number |
| `WS /ws` | Live snapshot stream |

---

## Layout

```
crates/eqlparser-core/    parsing, encounters, spells, class inference
  src/event.rs              typed event model
  src/parser.rs             regex rule table (RegexSet dispatch)
  src/encounter.rs          segmentation + combat aggregation
  src/spell.rs              spell database, duration formulas, buff resolver
  src/class.rs              class inference
crates/eqlparserd/        daemon: follower, HTTP/WebSocket API, overlay assets
gui/                      Electron desktop app
```

## Notes on accuracy

**Parser coverage is ~94%** of all log lines. The remainder is a long tail of
cosmetic strings, none more frequent than 0.05% of lines. Run `eqlcoverage` to
see exactly what is unhandled.

**Buff identification is ambiguous by nature.** The client never names the spell
when an effect lands — it prints flavour text, and ~54,000 spells share only
~13,000 distinct strings ("You feel replenished." maps to 29 different spells).
The resolver scores candidates using the preceding cast line, then the
character's known repertoire from memorised spells, then rank equivalence.
On a real 595k-line log this identifies **93% of landings confidently**; the
rest are flagged with a `?` in both UIs rather than presented as fact.

**Timestamps carry no timezone.** EverQuest writes local time with no offset, so
timestamps are parsed as UTC and compared within that domain. Every use is a
difference or an ordering, so this is exact — and it avoids DST discontinuities
entirely. Displayed times are formatted back in UTC to recover the wall clock
the player actually saw.

**Warriors cast nothing.** A pure-melee class contributes no spell evidence, so
its detection rests entirely on the curated skill table in `class.rs`. That
table deliberately omits skills every melee class trains (Defense, Offense,
Dodge, 1H Slashing) because they carry no signal.
