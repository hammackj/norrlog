//! Embeds the icon Explorer and the taskbar show for norrlog.exe. Nothing to
//! do anywhere else: Linux takes its icon from the desktop entry, macOS from
//! the app bundle.
//!
//! Checks the target, not `cfg!`: a build script runs on the host.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=../../packaging/windows");
        embed_resource::compile("../../packaging/windows/norrlog.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("embedding the Windows icon");
    }
}
