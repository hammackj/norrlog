# Norrlog

A log parser, DPS meter and stream overlay for **EverQuest** and **EverQuest
Legends**. It reads the log file the game writes as you play and turns it into
fights, damage and healing meters, buff timers, loot and drop tables,
experience, tradeskills and class levels, live and across every session you
have logged. It was called eqlparser until November 2026.

It is two programs: **norrlogd**, a small daemon that follows the log and keeps
the history, and **Norrlog**, the desktop app that shows it. The app starts the
daemon for you. The same daemon serves a web page you can add to OBS as an
overlay.

## What it shows

- **Fights.** Every fight, found automatically, with damage, DPS, healing,
  damage taken, crits and a per-ability breakdown for you, your group, your pets
  and the creatures you fought. Search and filter the history; open any fight
  for its timeline.
- **Live meters.** For the current fight, the whole session or the zone.
- **Buff timers.** Real durations from the game's own spell file, not guesses.
- **Loot and drops.** What dropped from what, taken or left on the corpse, with
  the kills behind every count. A count over kills is shown as "seen per kill",
  never passed off as a drop rate.
- **Experience and levels.** Per class, per zone, and alternate advancement.
- **Tradeskills.** Combines, skill-ups, what each recipe cost to raise.
- **Classes.** Read from what the game states (`/who`, the class channel) and
  from the spells you cast.
- A **Quest Tracker** tab with Plane of Sky quest progress, for EverQuest Legends. Settings › Quest Tracker turns it off.
- **Fight detail.** Each fight's breakdown per ability, who cast what, the
  hits that landed before you died, and a one-line parse to paste into chat.
- **Tells and alerts.** A tell log, and alerts built from any line: a sound,
  speech, a banner on the overlay, and optionally a countdown there. The OBS
  overlay shows the banners and countdowns too (`alerts=0` hides them). GINA
  triggers import from a `.gtp` package or GINA's XML, and your alerts export
  as a `.gtp` package for someone using GINA.
- **Log search.** Any line in the log, newest first, as plain text or a
  regular expression, read from the file as it is now.

## Two games

The two games share a client and a log format, and nearly everything reads
both the same way. What differs is in one place, `norrlog_core::profile`,
chosen per log from the install path (or `--game eq|eql`):

| | Legends | live EverQuest |
|---|---|---|
| Classes held | three at once, the loadout changes | one at a time; a character can switch, each class levelled on its own |
| Level cap | 50 | 130 |
| An unplayed class | level 10 | unknown until something states it |
| Where the class comes from | `/who`, spell sets, the spellbook | `/who`, the auto-joined class chat channel, the spellbook |
| Plane of Sky, lockouts, motes | yes | no (the tabs and panels are hidden) |
| Auto-sell on loot | yes | no — most drops are left on the corpse, and counted as seen |
| Item and quest site | eqldb | eqdb |

The spell file differs too (174 fields against 166), which the loader tells
apart by shape. Live EverQuest support began as a fork, eqparser, and was
folded back in.

## Install

There are no prebuilt releases yet. To build from source you need
[Rust](https://rustup.rs) (stable).

On Linux, the app also needs a few system libraries. On Debian or Ubuntu:

```sh
sudo apt install libasound2-dev libfontconfig-dev libfreetype-dev libvulkan-dev \
  libwayland-dev libx11-xcb-dev libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev
```

Then:

```sh
git clone https://github.com/hammackj/norrlog
cd norrlog
cargo build --release
./target/release/norrlog
```

To add it to your desktop's menu, run `packaging/install-launcher.sh`.

Norrlog is used daily on Linux, with the game running under Wine or CrossOver.

**Windows and macOS support is experimental.** The app compiles for both,
the release workflow packages a Windows installer and a macOS disk image, and
neither has been run on real hardware yet. Expect rough edges, the overlay
and spoken alerts above all. Neither package is code-signed:

- **Windows:** SmartScreen may ask you to confirm. Choose **More info**, then
  **Run anyway**.
- **macOS (Apple Silicon):** the app is not notarised. The first time, right-click
  it and choose **Open**, or run
  `xattr -dr com.apple.quarantine /Applications/Norrlog.app`.

If something breaks, please
[open an issue](https://github.com/hammackj/norrlog/issues) with your OS
version and what you saw.

### Updates

An installed copy (the AppImage, the Windows installer's, or the macOS app)
checks GitHub for a newer release when it starts. If there is one, a bar at
the top offers it; nothing is installed until you choose **Update**. The
download is checked against the project's release signing key before it
replaces anything, and Norrlog then restarts as the new version. Turn the
check off, or check by hand, under **Settings → Updates**.

The check is one request to GitHub's releases API, made by the daemon. It
sends nothing about you or your logs. A copy built from source never replaces
itself; update it with `git pull` and a rebuild.

## First run

Open **Settings** and pick your log file and your game folder. The log file is
`Logs/eqlog_<Character>_<server>.txt` inside the game folder; turn logging on in
game with `/log on`. The game folder is what gives buff timers and class
detection, which need the game's `spells_us.txt`.

Norrlog keeps its history in `~/.config/norrlog` (or your platform's usual
place). Nothing leaves your machine except item look-ups: tooltips and icons are
fetched from eqldb.hammackj.com (EverQuest Legends) or eqdb.hammackj.com
(EverQuest). Turn that off in Settings ("Item info"), or start the daemon with
`--eqdb off`; tooltips then use the item data bundled with the app.

To run the daemon on its own, for the overlay alone:

```sh
./target/release/norrlogd --log "<game folder>/Logs/eqlog_<Character>_<server>.txt" \
  --game-dir "<game folder>"
```

## Overlay

**Over the game.** Click **overlay** in the header, or **Show overlay** in
Settings. The overlay is a transparent window that stays above the game, even
a fullscreen one, and lets clicks through to it. Settings chooses its panels,
its position and width, and a second overlay for charm. The height follows
what it shows. On Wayland it needs a compositor with the layer-shell protocol
(KDE, Sway, Hyprland and most others; not GNOME), and on X11 any window
manager.

**In OBS.** Add a **Browser Source** pointing at:

```
http://127.0.0.1:8777/overlay
```

About 360×420, transparent background. The page paints no background of its
own, so it composites cleanly. Both overlays take the same options; in the app
they are typed into Settings, for OBS they go on the URL:

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

Panels: `meter`, `dps`, `outgoing`, `incoming`, `xp`, `heals`, `taken`,
`auras`, `buffs`, `debuffs`, `others`, `fct`, `motes`, `charm`, `sky`, `apps`. Several
hide themselves when they have nothing to say. `others` lists your spells running
on other people and mobs (a heal over time on each groupmate, your dots and
slows) with a countdown per target. The countdown starts from the spell file's
duration, then uses how long each spell was actually seen to last for you.
`fct` is floating combat text: damage you take and heals you get rise on the
left, your hits and your heals on others on the right, crits larger. It shows
only what happens after the overlay opens.

## Command-line tools

```sh
# Encounter report: top fights, ability breakdowns, charm contribution, totals
./target/release/norrlog-report <logfile> [game-dir]

# Parser coverage: what fraction of lines are understood, and what is not.
# The unmatched histogram is the tool for growing the rule table.
./target/release/norrlog-coverage <logfile> [top-n] [game-dir]

# Recipe book: grow it by pasting an EQTraders "Full Recipe List" into a file.
# Merging is additive and deduplicated, so re-importing changes nothing.
tools/import-recipes.py paste.txt [--check]

# Archive recipe data from eqtraders.com item pages, one request at a time
# with a five-second gap. Every page is cached, so resuming is free and a
# rerun costs nothing; touch <cache>/STOP to halt cleanly mid-run.
tools/import-eqtraders.py --items 1-2000
tools/import-eqtraders.py --reparse          # rebuild from the cache, no network
```

## How it works

[docs/how-it-works.md](docs/how-it-works.md) covers how each part reads the
log, the daemon's HTTP and WebSocket API, the code layout, and how far to trust
each number.

## Built with Claude

Norrlog was written with a lot of help from
[Claude Code](https://claude.com/claude-code), Anthropic's coding agent. That
covers the log parser and daemon, the desktop app (first in Electron, now in
Rust and GPUI), and most of the bug fixes since. The maintainer sets the design and
features, and decides what ships in each release.

Fixes come with a test that reproduces the bug and fails without it, and
changes to what the parser counts are checked against real logs from both
games before they are committed. Commits don't carry AI co-author trailers;
this section is the disclosure instead.

## Bundled data

`crates/norrlog-read/atlas` and `crates/norrlog-vision/templates` hold small
glyph and label crops taken from screenshots of the EverQuest client. They let
the reader match the game's own UI text in an item screenshot, and are kept for
that alone; EverQuest and its interface are © Daybreak Game Company. Nothing
else from the game ships: spells and item data are read from your own install
while the daemon runs. The recipe book is not shipped either; see
[How it works](docs/how-it-works.md).

## Contributing

Bug reports and fixes are welcome; [CONTRIBUTING.md](CONTRIBUTING.md) covers
building, testing and what a change comes with.

## Licence

MIT. See [LICENSE](LICENSE).
