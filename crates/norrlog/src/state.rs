//! What the window remembers between runs: the open tab and where it was.
//! This app's own file (`~/.config/norrlog/state.json`), not the shared
//! config; losing it costs a window position and nothing else.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub tab: String,
    pub bounds: Option<Rect>,
    pub maximized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

fn path() -> Option<PathBuf> {
    crate::config::config_home().map(|d| d.join("norrlog/state.json"))
}

impl State {
    pub fn load() -> Self {
        path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        // Written beside and renamed over, so a crash mid-write leaves the
        // old file rather than half of a new one.
        let tmp = p.with_extension("json.tmp");
        if let Ok(b) = serde_json::to_vec_pretty(self)
            && std::fs::write(&tmp, b).is_ok()
        {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}
