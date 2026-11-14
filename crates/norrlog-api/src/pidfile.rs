//! Which process holds a port, for a daemon that cannot say so yet.
//!
//! A daemon binds its port before its initial parse and answers nothing
//! until that is done, which on a long log takes minutes. A restart asked
//! for in that window (another log, the other game) needs the pid to stop
//! it, and `/api/hello` is where the pid comes from. So the daemon writes it
//! here too, the moment it has the port, and a restart that meets a busy
//! daemon reads it from here.
//!
//! The file is in the temp directory and named for the port, which, like
//! the port, is shared by everything on the machine. A pid read from it is
//! used only while the port is held, and only when that process is a
//! norrlogd: a stale file can name a pid since reused by anything.

use std::path::PathBuf;

/// Where the daemon on `port` writes its pid.
pub fn path(port: u16) -> PathBuf {
    std::env::temp_dir().join(format!("norrlogd-{port}.pid"))
}

/// Record this process as the daemon on `port`.
pub fn write(port: u16) -> std::io::Result<()> {
    std::fs::write(path(port), std::process::id().to_string())
}

/// The norrlogd recorded for `port`, if the file names one that is running.
pub fn holder(port: u16) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(path(port))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    is_norrlogd(pid).then_some(pid)
}

fn named_norrlogd(name: &str) -> bool {
    let name = name.trim().trim_end_matches(".exe");
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    name == crate::IDENTITY || name == crate::OLD_IDENTITY
}

#[cfg(target_os = "linux")]
fn is_norrlogd(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| named_norrlogd(&c))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn is_norrlogd(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .stdin(std::process::Stdio::null())
        .output()
        .is_ok_and(|o| named_norrlogd(&String::from_utf8_lossy(&o.stdout)))
}

#[cfg(windows)]
fn is_norrlogd(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    // `"norrlogd.exe","1234",...`, or a line saying there is no such task.
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .stdin(std::process::Stdio::null())
        .creation_flags(0x0800_0000)
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split(',')
                .next()
                .is_some_and(|n| named_norrlogd(n.trim_matches('"')))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_norrlogd_is_named() {
        assert!(named_norrlogd("norrlogd\n"));
        assert!(named_norrlogd("/usr/bin/norrlogd"));
        assert!(named_norrlogd("C:\\Program Files\\Norrlog\\norrlogd.exe"));
        assert!(named_norrlogd("eqlparserd"));
        assert!(!named_norrlogd("norrlog"));
        assert!(!named_norrlogd("firefox"));
        assert!(!named_norrlogd(""));
    }

    #[test]
    fn a_file_naming_some_other_process_is_ignored() {
        // A port nobody uses in tests; the pid is this test runner's own.
        let port = 1;
        write(port).unwrap();
        assert_eq!(holder(port), None);
        std::fs::write(path(port), "not a pid").unwrap();
        assert_eq!(holder(port), None);
        let _ = std::fs::remove_file(path(port));
    }
}
