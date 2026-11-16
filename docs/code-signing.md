# Code signing policy

Norrlog's downloads carry two kinds of signature.

- **minisign, on every download.** Every file on the
  [releases page](https://github.com/hammackj/norrlog/releases) has a
  `.minisig` beside it, made by the release workflow with the project's key.
  Installed copies check it before they update themselves. You can check it
  yourself; the README shows how.
- **Authenticode, on the Windows programs and installer.** Windows reads this
  signature when you run a program. Norrlog has applied for it through the
  SignPath Foundation's free programme for open-source projects. Until the
  application is accepted, the Windows downloads are not Authenticode-signed
  and Windows warns before running them; the README says how to get past that.

Once it is in place:

Free code signing provided by [SignPath.io](https://signpath.io), certificate
by [SignPath Foundation](https://signpath.org).

The macOS app is not signed with an Apple Developer ID, so macOS asks before
opening it the first time.

## What gets signed, and how

Only files built by GitHub Actions from this repository, by the
[release workflow](../.github/workflows/release.yml), are signed. Nothing built
on a personal machine is.

1. A version tag starts the release workflow on GitHub's own runners.
2. It builds `norrlog.exe` and `norrlogd.exe` and checks their version
   resources: product name "Norrlog" and the release's version.
3. It sends the two programs to SignPath, which checks that they came from
   this repository's workflow and carry that product name and version.
4. A person approves the request in SignPath (below). Nothing is signed
   without that.
5. The workflow builds the portable zip and the installer from the signed
   programs, and the installer goes through steps 3 and 4 itself.
6. The workflow checks every signature, installs and uninstalls the installer,
   and attaches the files to a draft release. Then minisign signs every file.

The SignPath artifact configurations are in
[`packaging/windows/signpath/`](../packaging/windows/signpath).

## Team roles

Norrlog has one maintainer, who holds every role.

| Role | Who |
| --- | --- |
| Committer and reviewer | [hammackj](https://github.com/hammackj) |
| Approver | [hammackj](https://github.com/hammackj) |

- **Committers** can change the source in this repository directly.
- **Reviewers** review every change from anyone else (pull requests) before it
  is merged. `main` is protected: changes from others come only through pull
  requests that pass CI.
- **Approvers** approve each signing request, after checking it was built from
  a tagged commit on `main`.

Everyone with any of these roles uses multi-factor authentication on GitHub
and on SignPath.

## Privacy

Norrlog reads your EverQuest log and keeps what it finds on your own computer.
It never sends your log, your characters, your fights or anything it has
worked out from them anywhere.

It talks to the network in exactly three ways.

1. **The local server.** The background program, `norrlogd`, serves the app
   and the OBS overlay from `127.0.0.1` (port 8777 by default). That address
   only accepts connections from the same computer.
2. **Item, spell and zone details** for tooltips. When you look at an item,
   spell or zone, `norrlogd` asks the companion site for that game for it by
   name: `eqldb.hammackj.com` for EverQuest Legends, `eqdb.hammackj.com` for
   EverQuest. The request carries the name asked about, and like any web
   request it shows the site your IP address. Switch it off in **Settings →
   Item info from eqldb → Off**, or start the daemon with `--eqdb off`; the
   data bundled with Norrlog is then used instead.
3. **The update check.** An installed copy asks GitHub
   (`api.github.com`, about `hammackj/norrlog`) whether there is a newer
   release, when it starts and when you press Check now. A new version is
   downloaded from GitHub only after you agree to it. Switch the check off in
   **Settings → Updates**. A copy built from source checks only when you
   press Check now.

Those are the only systems Norrlog contacts. It has no analytics, telemetry or
crash reporting. GitHub's own privacy statement covers requests made to
GitHub.

## Third-party components

The programs are built from Rust crates published on crates.io. Each package
carries `THIRD-PARTY-LICENSES.txt`, listing every component and its licence.
Every one of them is open source, and they are used unmodified.
