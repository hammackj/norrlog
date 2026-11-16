# Security

Please report a security problem privately, not in a public issue: use
[Report a vulnerability](https://github.com/hammackj/norrlog/security/advisories/new)
on GitHub. Say what you found, how to reproduce it, and which version and
system you were on.

## What is in scope

- **The updater.** An installed copy downloads releases and replaces itself.
  It installs only files signed with the project's minisign key (public key
  `RWS3or1Ui5F6hhknu3WooKg9KcrGHkPeVT3cR1ufbk2WbcZuXf12d2HL`), checked against
  a signed comment naming the release and file. Anything that gets an
  unsigned or mismatched file installed is a security bug.
- **The daemon's HTTP server.** It listens on 127.0.0.1 only. Anything that
  lets another machine reach it, or lets a web page you visit act on it, is a
  security bug.
- **Reading game logs.** Norrlog only reads the game's files. Anything that
  makes it write to, or run, something it read is a security bug.

Only the latest release is supported.
