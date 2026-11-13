//! Environment variables, under the name they have now and the one they had.
//!
//! The project was called eqlparser, and its variables were `EQLPARSER_*`
//! and `EQL_*`. Anybody who set one in a shell profile or a launcher keeps
//! getting it honoured under the old name, with the new name winning when
//! both are set. The old names are for one release, then they go.

use std::ffi::OsString;

/// `NORRLOG_<name>`, else the name it had before the rename.
pub fn var_os(name: &str) -> Option<OsString> {
    std::env::var_os(format!("NORRLOG_{name}"))
        .or_else(|| old_name(name).and_then(std::env::var_os))
}

/// [`var_os`], as a string; `None` when unset or not valid UTF-8.
pub fn var(name: &str) -> Option<String> {
    var_os(name).and_then(|v| v.into_string().ok())
}

fn old_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "GAME_DIR" => "EQLPARSER_GAME_DIR",
        "EQDB" => "EQLPARSER_EQDB",
        "DAEMON" => "EQLPARSER_DAEMON",
        "SHOTS_DIR" => "EQL_SHOTS_DIR",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_old_name_is_one_the_project_used() {
        for n in ["GAME_DIR", "EQDB", "DAEMON", "SHOTS_DIR"] {
            let old = super::old_name(n).unwrap();
            assert!(
                old.starts_with("EQLPARSER_") || old.starts_with("EQL_"),
                "{old}"
            );
            assert!(old.ends_with(n), "{old} is not {n}");
        }
    }
}
