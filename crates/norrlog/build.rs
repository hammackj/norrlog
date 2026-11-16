//! Embeds the icon Explorer and the taskbar show for norrlog.exe, and its
//! version resource. Nothing to do anywhere else: Linux takes its icon from
//! the desktop entry, macOS from the app bundle.
//!
//! Checks the target, not `cfg!`: a build script runs on the host.

include!("../../packaging/windows/versioninfo.rs");

fn main() {
    embed_version_info("Norrlog", "norrlog.exe", Some("norrlog.ico"));
}
