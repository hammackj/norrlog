//! Embeds norrlogd.exe's version resource on Windows; see
//! packaging/windows/versioninfo.rs.

include!("../../packaging/windows/versioninfo.rs");

fn main() {
    embed_version_info("Norrlog daemon", "norrlogd.exe", None);
}
