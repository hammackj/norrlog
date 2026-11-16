//! Updating an installed Norrlog from its GitHub releases.
//!
//! On launch, an installed copy (the AppImage, the Windows installer's, the
//! macOS app) asks the daemon for the newest release, and when it is newer
//! than this build a bar offers it. Nothing installs without the person
//! choosing Update, and nothing installs that is not signed with the
//! project's release key: the download is checked here, against the public
//! key this build carries, not by the daemon that fetched it. A build run
//! from a source checkout never replaces itself; Check now still says what
//! is out.
//!
//! The release workflow signs every file with minisign, with the trusted
//! comment `norrlog <tag> <file>`. That comment is signed too, and is
//! checked, so a signed file of another release, or another file of this
//! one, cannot pass for the one offered.

use crate::api::Api;
use crate::parts::hint;
use crate::theme::px;
use crate::theme::*;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Disableable, Sizable, h_flex};
use norrlog_api::{FetchedUpdate, LatestRelease, Release, ReleaseAsset};
use std::path::{Path, PathBuf};

/// The release key's public half: the second line of the `.pub` file that
/// `packaging/release-key.sh` writes. Empty until a key is made, and while
/// it is, no update is ever installed.
const PUBLIC_KEY: &str = "";

/// The key updates must be signed with. A build can be given another at
/// compile time (`NORRLOG_UPDATE_PUBKEY`), which is how the updater is tested
/// without the release key; a shipped build is made without it.
fn public_key() -> Option<&'static str> {
    option_env!("NORRLOG_UPDATE_PUBKEY")
        .or(Some(PUBLIC_KEY))
        .filter(|k| !k.trim().is_empty())
}

/// This build's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How this copy was installed, which decides whether it can replace itself
/// and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// The AppImage at this path; replaced in place.
    AppImage(PathBuf),
    /// Installed by the Windows installer; the new installer is run.
    #[cfg_attr(not(windows), allow(dead_code))]
    Windows,
    /// The app bundle at this path; swapped for the new one.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    MacApp(PathBuf),
    /// Run from a build directory. Updates come from git, not from here.
    Source,
}

impl Install {
    pub fn detect() -> Install {
        #[cfg(target_os = "linux")]
        if let Some(p) = std::env::var_os("APPIMAGE")
            .map(PathBuf::from)
            .filter(|p| p.is_file())
        {
            return Install::AppImage(p);
        }
        let Some(exe) = std::env::current_exe().ok() else {
            return Install::Source;
        };
        let _ = &exe;
        #[cfg(windows)]
        if exe
            .parent()
            .is_some_and(|d| d.join("unins000.exe").is_file())
        {
            return Install::Windows;
        }
        #[cfg(target_os = "macos")]
        if let Some(bundle) = exe
            .parent()
            .filter(|d| d.ends_with("Contents/MacOS"))
            .and_then(|d| d.parent()?.parent())
            .filter(|b| b.extension().is_some_and(|e| e == "app"))
        {
            return Install::MacApp(bundle.to_path_buf());
        }
        Install::Source
    }

    pub fn updates_itself(&self) -> bool {
        !matches!(self, Install::Source)
    }

    /// Whether `name` is this kind of install's file in a release.
    fn wants(&self, name: &str) -> bool {
        let arch = std::env::consts::ARCH;
        match self {
            Install::AppImage(_) => name.ends_with(".AppImage") && name.contains(arch),
            Install::Windows => name.ends_with("-windows-setup.exe") && name.contains(arch),
            Install::MacApp(_) => name.ends_with(".dmg") && name.contains(arch),
            Install::Source => false,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Install::AppImage(p) => format!("the AppImage at {}", p.display()),
            Install::Windows => "installed by the Windows installer".into(),
            Install::MacApp(p) => format!("the app at {}", p.display()),
            Install::Source => "a source build".into(),
        }
    }
}

/// `v1.2.3` or `1.2.3` as numbers; anything after the third number (a
/// pre-release suffix) is ignored.
fn version_of(tag: &str) -> Option<(u64, u64, u64)> {
    let t = tag.trim().trim_start_matches(['v', 'V']);
    let mut parts = t.splitn(3, '.');
    let num = |s: &str| {
        let d: String = s.chars().take_while(char::is_ascii_digit).collect();
        d.parse().ok()
    };
    Some((
        num(parts.next()?)?,
        num(parts.next()?)?,
        num(parts.next()?)?,
    ))
}

/// Whether `candidate` is a later version than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (version_of(candidate), version_of(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// The file in `release` for this install, if it has one that is signed.
fn pick<'a>(install: &Install, release: &'a Release) -> Option<&'a ReleaseAsset> {
    release.assets.iter().find(|a| {
        install.wants(&a.name)
            && release
                .assets
                .iter()
                .any(|s| s.name == format!("{}.minisig", a.name))
    })
}

/// Check `path` against `signature`: made with the release key, over these
/// bytes, and saying it is `asset` of `tag`.
pub fn verify(path: &Path, signature: &str, tag: &str, asset: &str) -> Result<(), String> {
    verify_with(public_key(), path, signature, tag, asset)
}

fn verify_with(
    key: Option<&str>,
    path: &Path,
    signature: &str,
    tag: &str,
    asset: &str,
) -> Result<(), String> {
    use std::io::Read;
    let key = key.ok_or("this build has no release key, so it installs no updates")?;
    let key = minisign_verify::PublicKey::from_base64(key.trim())
        .map_err(|e| format!("the release key is unreadable: {e}"))?;
    let sig = minisign_verify::Signature::decode(signature)
        .map_err(|e| format!("the signature is unreadable: {e}"))?;
    let expected = format!("norrlog {tag} {asset}");
    if sig.trusted_comment() != expected {
        return Err(format!(
            "the signature is for {:?}, not {expected:?}",
            sig.trusted_comment()
        ));
    }
    let mut check = key
        .verify_stream(&sig)
        .map_err(|e| format!("not signed with the release key: {e}"))?;
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        check.update(&buf[..n]);
    }
    check
        .finalize()
        .map_err(|e| format!("the download does not match its signature: {e}"))
}

/// Put the verified `file` in place of this install, and return the command
/// that starts the new one. Nothing is changed when this fails.
fn install(install: &Install, file: &Path) -> Result<std::process::Command, String> {
    match install {
        Install::AppImage(target) => {
            // Beside the old one and renamed over it, so the replacement is
            // whole or not at all, and launchers pointing at the path keep
            // working.
            let name = target
                .file_name()
                .ok_or("the AppImage has no file name")?
                .to_string_lossy();
            let tmp = target.with_file_name(format!(".{name}.new"));
            std::fs::copy(file, &tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
                    .map_err(|e| e.to_string())?;
            }
            if let Err(e) = std::fs::rename(&tmp, target) {
                let _ = std::fs::remove_file(&tmp);
                return Err(format!("{}: {e}", target.display()));
            }
            let mut cmd = std::process::Command::new(target);
            cmd.env_remove("APPIMAGE");
            Ok(cmd)
        }
        Install::Windows => {
            // The installer closes Norrlog and the daemon, installs over the
            // same place (it finds it by AppId), and starts the new one when
            // asked to with /RELAUNCH=1.
            let mut cmd = std::process::Command::new(file);
            cmd.args([
                "/VERYSILENT",
                "/SUPPRESSMSGBOXES",
                "/NORESTART",
                "/RELAUNCH=1",
            ]);
            Ok(cmd)
        }
        Install::MacApp(bundle) => {
            let parent = bundle.parent().ok_or("the app has no folder")?;
            let mount = file.with_extension("mount");
            let _ = std::fs::create_dir_all(&mount);
            let run = |c: &mut std::process::Command| -> Result<(), String> {
                let out = c.output().map_err(|e| e.to_string())?;
                if out.status.success() {
                    Ok(())
                } else {
                    Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
                }
            };
            run(std::process::Command::new("hdiutil")
                .args([
                    "attach",
                    "-nobrowse",
                    "-readonly",
                    "-noautoopen",
                    "-mountpoint",
                ])
                .arg(&mount)
                .arg(file))?;
            let fresh = parent.join(".Norrlog.app.new");
            let _ = std::fs::remove_dir_all(&fresh);
            let copied = run(std::process::Command::new("ditto")
                .arg(mount.join("Norrlog.app"))
                .arg(&fresh));
            let _ = run(std::process::Command::new("hdiutil")
                .args(["detach", "-quiet"])
                .arg(&mount));
            copied?;
            let old = parent.join(".Norrlog.app.old");
            let _ = std::fs::remove_dir_all(&old);
            std::fs::rename(bundle, &old).map_err(|e| format!("{}: {e}", bundle.display()))?;
            if let Err(e) = std::fs::rename(&fresh, bundle) {
                let _ = std::fs::rename(&old, bundle);
                return Err(format!("{}: {e}", bundle.display()));
            }
            let _ = std::fs::remove_dir_all(&old);
            let mut cmd = std::process::Command::new("open");
            cmd.arg("-n").arg(bundle);
            Ok(cmd)
        }
        Install::Source => Err("a source build does not replace itself".into()),
    }
}

/// Start `cmd` on its own, so it outlives this process.
fn detach(mut cmd: std::process::Command) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(DETACHED_PROCESS);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

// ---- the bar ---------------------------------------------------------------

enum Bar {
    Hidden,
    /// A newer release this install can take.
    Offer(Release, ReleaseAsset),
    /// A newer release this install cannot take itself (a source build, or
    /// one with no signed file for it).
    Tell(Release, String),
    Working(String),
    Failed(String),
    /// The answer to Check now when there is nothing newer.
    Note(String),
}

pub struct UpdateBar {
    api: Option<Api>,
    install: Install,
    /// Whether the launch check has been made. Once per run.
    asked: bool,
    bar: Bar,
    /// Kept apart from any rolling task list: a download takes minutes.
    task: Option<Task<()>>,
}

impl UpdateBar {
    pub fn new() -> Self {
        UpdateBar {
            api: None,
            install: Install::detect(),
            asked: false,
            bar: Bar::Hidden,
            task: None,
        }
    }

    /// The daemon is there; ask once, if asking on launch is wanted.
    pub fn attached(&mut self, api: Api, cx: &mut Context<Self>) {
        self.api = Some(api);
        if self.asked {
            return;
        }
        self.asked = true;
        let wanted = crate::config::ParserConfig::load()
            .check_for_updates
            .unwrap_or(true);
        // A source build says nothing on launch; Check now still answers.
        if wanted && self.install.updates_itself() {
            self.check(false, cx);
        }
    }

    /// Ask for the newest release. `by_hand` reports "nothing newer" too.
    pub fn check(&mut self, by_hand: bool, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else {
            if by_hand {
                self.bar =
                    Bar::Failed("The daemon is not answering, so nothing can be asked.".into());
                cx.notify();
            }
            return;
        };
        if by_hand {
            self.bar = Bar::Working("Checking for a newer release…".into());
            cx.notify();
        }
        self.task = Some(cx.spawn(async move |this, cx| {
            let got = api.get::<LatestRelease>("/api/update/latest".into()).await;
            let _ = this.update(cx, |b, cx| {
                b.bar = b.answer(got, by_hand);
                cx.notify();
            });
        }));
    }

    fn answer(&self, got: Result<LatestRelease, String>, by_hand: bool) -> Bar {
        let latest = match got {
            Ok(l) => l,
            Err(e) if by_hand => return Bar::Failed(format!("Could not check: {e}")),
            Err(_) => return Bar::Hidden,
        };
        let Some(release) = latest.release else {
            return match (by_hand, latest.error) {
                (false, _) => Bar::Hidden,
                (true, Some(e)) => Bar::Failed(format!("Could not check: {e}")),
                (true, None) => Bar::Note("No release has been published yet.".into()),
            };
        };
        if !is_newer(&release.tag, VERSION) {
            return if by_hand {
                Bar::Note(format!("Norrlog {VERSION} is the newest release."))
            } else {
                Bar::Hidden
            };
        }
        if !self.install.updates_itself() {
            return Bar::Tell(
                release,
                "This is a source build, which does not replace itself.".into(),
            );
        }
        if public_key().is_none() {
            return Bar::Tell(
                release,
                "This build has no release key, so it installs no updates.".into(),
            );
        }
        match pick(&self.install, &release).cloned() {
            Some(asset) => Bar::Offer(release, asset),
            None => Bar::Tell(release, "It has no signed download for this system.".into()),
        }
    }

    fn update_now(&mut self, release: Release, asset: ReleaseAsset, cx: &mut Context<Self>) {
        let Some(api) = self.api.clone() else { return };
        let install = self.install.clone();
        self.bar = Bar::Working(format!(
            "Downloading Norrlog {} ({:.0} MB)…",
            release.tag,
            asset.size as f64 / 1_048_576.
        ));
        cx.notify();
        let port = api.client.port();
        let rt = api.rt.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let body = serde_json::json!({ "tag": release.tag, "asset": asset.name });
            let fetched = api
                .post_for::<FetchedUpdate>("/api/update/fetch".into(), body)
                .await;
            let fetched = match fetched {
                Ok(f) => f,
                Err(e) => {
                    let _ = this.update(cx, |b, cx| {
                        b.bar = Bar::Failed(format!("The download failed: {e}"));
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update(cx, |b, cx| {
                b.bar = Bar::Working("Checking the signature and installing…".into());
                cx.notify();
            });
            let tag = release.tag.clone();
            let name = asset.name.clone();
            let done = cx
                .background_executor()
                .spawn(async move {
                    let path = PathBuf::from(&fetched.path);
                    verify(&path, &fetched.signature, &tag, &name)?;
                    let cmd = install_for(&install, &path)?;
                    Ok::<_, String>(cmd)
                })
                .await;
            match done {
                Ok(cmd) => {
                    // The daemon is stopped so the new one starts its own;
                    // a running old one would otherwise be attached to.
                    let _ = rt.spawn(crate::daemon::stop_port(port)).await;
                    let started = detach(cmd);
                    let _ = this.update(cx, |b, cx| match started {
                        Ok(()) => cx.quit(),
                        Err(e) => {
                            b.bar = Bar::Failed(format!(
                                "Installed, but the new version could not be started ({e}). Start Norrlog again."
                            ));
                            cx.notify();
                        }
                    });
                }
                Err(e) => {
                    let _ = this.update(cx, |b, cx| {
                        b.bar = Bar::Failed(format!("Not installed: {e}"));
                        cx.notify();
                    });
                }
            }
        }));
    }
}

/// [`install`], reachable from a background task.
fn install_for(i: &Install, file: &Path) -> Result<std::process::Command, String> {
    install(i, file)
}

impl Render for UpdateBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let row = || {
            h_flex()
                .gap_3()
                .px_4()
                .py(px(6.))
                .bg(wash(ACCENT, 0.12))
                .border_b_1()
                .border_color(hex(BORDER))
                .text_sm()
        };
        let later = |id: &'static str| {
            Button::new(id)
                .label("Later")
                .small()
                .ghost()
                .on_click(cx.listener(|b, _, _, cx| {
                    b.bar = Bar::Hidden;
                    cx.notify();
                }))
        };
        let notes = |r: &Release| {
            let url = r.url.clone();
            Button::new("update-notes")
                .label("What's new")
                .small()
                .ghost()
                .when(url.is_empty(), |b| b.disabled(true))
                .on_click(move |_, _, cx| cx.open_url(&url))
        };
        match &self.bar {
            Bar::Hidden => div().into_any_element(),
            Bar::Offer(r, a) => {
                let (r2, a2) = (r.clone(), a.clone());
                row()
                    .child(div().flex_1().child(format!(
                        "Norrlog {} is available. You have {VERSION}.",
                        r.tag
                    )))
                    .child(notes(r))
                    .child(
                        Button::new("update-now")
                            .label("Update")
                            .small()
                            .primary()
                            .on_click(cx.listener(move |b, _, _, cx| {
                                b.update_now(r2.clone(), a2.clone(), cx)
                            })),
                    )
                    .child(later("update-later"))
                    .into_any_element()
            }
            Bar::Tell(r, why) => row()
                .child(
                    div()
                        .flex_1()
                        .child(format!("Norrlog {} is out. {why}", r.tag)),
                )
                .child(notes(r))
                .child(later("update-later"))
                .into_any_element(),
            Bar::Working(what) => row()
                .child(div().flex_1().child(what.clone()))
                .into_any_element(),
            Bar::Failed(why) => row()
                .child(div().flex_1().text_color(hex(WARN)).child(why.clone()))
                .child(later("update-dismiss"))
                .into_any_element(),
            Bar::Note(what) => row()
                .child(div().flex_1().child(what.clone()))
                .child(later("update-dismiss"))
                .into_any_element(),
        }
    }
}

/// What Settings says about updates, under its toggle.
pub fn settings_hint(install: &Install) -> Div {
    hint(format!(
        "This is Norrlog {VERSION}, {}. {}",
        install.describe(),
        if install.updates_itself() {
            "A newer release found on launch is offered in a bar at the top; nothing is installed until you choose Update, and only a download signed with the project's release key is."
        } else {
            "It does not replace itself: update it with git and a rebuild. Check now still says whether a newer release is out."
        }
    ))
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that brings GPUI's own `test` attribute along.
    use super::{Install, Release, ReleaseAsset, is_newer, pick, verify_with};

    #[test]
    fn versions_compare_as_numbers() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.0.9", "0.1.0"));
        assert!(is_newer("v0.2.0-rc1", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"));
    }

    fn release(names: &[&str]) -> Release {
        Release {
            tag: "v0.2.0".into(),
            assets: names
                .iter()
                .map(|n| ReleaseAsset {
                    name: n.to_string(),
                    size: 1,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn each_install_takes_its_own_signed_file() {
        let r = release(&[
            "Norrlog-0.2.0-x86_64.AppImage",
            "Norrlog-0.2.0-x86_64.AppImage.minisig",
            "norrlog-0.2.0-x86_64-windows-setup.exe",
            "norrlog-0.2.0-x86_64-windows-setup.exe.minisig",
            "norrlog-0.2.0-x86_64-windows.zip",
            "norrlog-0.2.0-aarch64-macos.dmg",
        ]);
        let app = Install::AppImage("/x".into());
        if std::env::consts::ARCH == "x86_64" {
            assert_eq!(
                pick(&app, &r).map(|a| a.name.as_str()),
                Some("Norrlog-0.2.0-x86_64.AppImage")
            );
            assert_eq!(
                pick(&Install::Windows, &r).map(|a| a.name.as_str()),
                Some("norrlog-0.2.0-x86_64-windows-setup.exe")
            );
        }
        // No signature beside it: not offered.
        assert_eq!(pick(&Install::MacApp("/A.app".into()), &r), None);
        assert_eq!(pick(&Install::Source, &r), None);
    }

    // A key and signatures made in minisign's format by the test signer in
    // the scratch tools; the secret half is not kept anywhere.
    const TEST_KEY: &str = include_str!("../tests/update/key.pub.b64");
    const BODY: &[u8] = include_bytes!("../tests/update/body.bin");
    const SIG_OK: &str = include_str!("../tests/update/body.bin.minisig");
    const SIG_OTHER_NAME: &str = include_str!("../tests/update/other-name.minisig");
    const SIG_OTHER_KEY: &str = include_str!("../tests/update/other-key.minisig");

    fn check(sig: &str, tag: &str, asset: &str, body: &[u8]) -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("norrlog-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("body.bin");
        std::fs::write(&p, body).unwrap();
        let got = verify_with(Some(TEST_KEY), &p, sig, tag, asset);
        let _ = std::fs::remove_dir_all(&dir);
        got
    }

    #[test]
    fn only_a_file_signed_as_this_release_passes() {
        let asset = "Norrlog-0.2.0-x86_64.AppImage";
        assert_eq!(check(SIG_OK, "v0.2.0", asset, BODY), Ok(()));
        let mut tampered = BODY.to_vec();
        tampered[0] ^= 1;
        assert!(
            check(SIG_OK, "v0.2.0", asset, &tampered).is_err(),
            "changed bytes"
        );
        assert!(
            check(SIG_OK, "v0.3.0", asset, BODY).is_err(),
            "another release"
        );
        assert!(
            check(SIG_OTHER_NAME, "v0.2.0", asset, BODY).is_err(),
            "signed as another file"
        );
        assert!(
            check(SIG_OTHER_KEY, "v0.2.0", asset, BODY).is_err(),
            "another key"
        );
        assert!(check("not a signature", "v0.2.0", asset, BODY).is_err());
        let dir = std::env::temp_dir();
        assert!(
            verify_with(None, &dir, SIG_OK, "v0.2.0", asset)
                .unwrap_err()
                .contains("no release key")
        );
    }
}
