# Contributing to Norrlog

Bug reports, fixes and new parser rules are all welcome. This page covers how
to build it, what a change should come with, and how to report a problem.

## Reporting a bug

[Open an issue](https://github.com/hammackj/norrlog/issues) and say:

- your OS, and how the game runs (Windows, or Wine/CrossOver on Linux or macOS);
- which game: **EverQuest** or **EverQuest Legends**;
- what you expected, and what Norrlog showed instead;
- the log lines involved, if it is about a number or a fight.

Windows and macOS support is new and experimental, so reports from there are
especially useful.

Before pasting log lines, take out anything you would rather not publish:
tells, other players' names, guild chat. A few lines around the problem are
worth more than a whole log, and a parser bug almost always comes down to one
line it read wrong or did not read at all. `norrlog-coverage` (below) lists the
lines nothing understood.

## Building

You need stable [Rust](https://rustup.rs). On Linux, the app also needs the
system libraries listed in the [README](README.md#install).

```sh
cargo build --workspace
cargo test --workspace
```

To run the app against a log without touching your own history, give it a
home of its own. On Linux and macOS, Norrlog and the daemon it starts keep
their state under `$HOME`; on Windows it is `%APPDATA%`. On Linux,
`XDG_CONFIG_HOME` and `XDG_DATA_HOME` win over `$HOME` when set, so unset
them:

```sh
env -u XDG_CONFIG_HOME -u XDG_DATA_HOME HOME=/tmp/norrlog-test \
  ./target/debug/norrlog --port 8830
```

Use a spare port too. A daemon already running on the default port 8777 is
attached to rather than replaced.

## What a change comes with

- **Formatting by rustfmt.** Run `cargo fmt --all` before committing. The code
  uses rustfmt's defaults, with no `rustfmt.toml`, and CI fails on anything
  else. A whole-tree reformat is listed in `.git-blame-ignore-revs`. To make
  `git blame` skip it:
  `git config blame.ignoreRevsFile .git-blame-ignore-revs`.
- **A test for a fix.** The test should reproduce the bug and fail without
  the fix. Most parser tests feed a few log lines to the engine and check what
  it made of them. The existing ones in `crates/norrlogd/src/engine.rs` and
  `crates/norrlog-core` show the pattern.
- **Both games in mind.** EverQuest and EverQuest Legends share a log format,
  but not everything in it. Whatever differs between them lives in
  `norrlog_core::profile`. A rule that holds for one game is checked against
  the other before it changes what the parser counts.
- **Honest numbers.** When a figure can't be known exactly, say so rather than
  guess. A count over kills is "seen per kill", not a drop rate, and a missing
  value is shown as missing, not as zero.
  [Notes on accuracy](docs/how-it-works.md#notes-on-accuracy) explains how far
  each number goes.

## Growing the parser

```sh
./target/release/norrlog-coverage <logfile> [top-n] [game-dir]
```

This prints what fraction of a log's lines are understood, and a histogram of
the ones that are not. The top of that histogram is where a new rule pays off
most. The rule table is in `crates/norrlog-core/src/parser.rs`.

## Finding your way around

[How it works](docs/how-it-works.md) covers the code layout, how each part
reads the log, and the daemon's HTTP and WebSocket API. In short:

- **norrlogd** follows the log and keeps the history;
- **norrlog** is the desktop app that draws what the daemon reports;
- **norrlog-core** is the parsing itself.

`gui/` is the earlier Electron app, being retired. New work goes in the Rust
app.

## Licence

Norrlog is MIT-licensed. By contributing, you agree that your contribution is
released under the same licence.
