//! Item lookups against the eqdb companion site.
//!
//! # Why through the daemon
//!
//! eqdb serves `/api/item?name=<slug>` with everything a tooltip wants —
//! stats, icon, where it drops, who sells it, what it is used in. The renderer
//! could fetch that directly, but going through here buys three things: the
//! window's content-security policy stays closed to remote origins, the
//! overlay gets the same data without a second implementation, and one cache
//! serves both rather than each panel re-asking for the same item.
//!
//! # What happens when it is not there
//!
//! The site is a separate program that may not be running, and until it is
//! published it will not be reachable from anywhere but this machine. So a
//! lookup that fails is not an error: it falls back to the item data shipped
//! inside this binary, which is smaller but always present. A tooltip that
//! cannot be drawn is simply not drawn.
//!
//! Failures are cached too, briefly. Without that, a page listing forty items
//! the site has never heard of asks forty times, every time it is drawn.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a successful lookup is reused.
///
/// Long, because item data changes when the site is rebuilt, not while you are
/// playing. The cost of being stale is a tooltip an hour out of date; the cost
/// of being eager is a request per hover.
const HIT_TTL: Duration = Duration::from_secs(3600);

/// How long a miss is remembered. Short enough that publishing the site, or
/// filling in a gap, shows up without a restart.
const MISS_TTL: Duration = Duration::from_secs(60);

/// Give up quickly. This sits in front of a hover; a tooltip that takes three
/// seconds has already failed at its job.
const TIMEOUT: Duration = Duration::from_secs(2);

/// The site's own slug form: lowercase, and every run of anything that is not
/// alphanumeric becomes a single underscore.
///
/// Derived here rather than asked for, because the parser knows items by the
/// name the client writes — "Djarn's Amethyst Ring" — and the site knows them
/// by `djarn_s_amethyst_ring`. Both ends must agree without a lookup table.
pub fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending = false;
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            if pending && !out.is_empty() {
                out.push('_');
            }
            pending = false;
            out.extend(c.to_lowercase());
        } else {
            pending = true;
        }
    }
    out
}

enum Cached {
    Hit(Value),
    Miss,
}

/// Looks items up on the site, remembering what it finds.
pub struct ItemDb {
    base: String,
    client: reqwest::Client,
    cache: Mutex<HashMap<String, (Instant, Cached)>>,
}

impl ItemDb {
    /// `base` is the site's origin, e.g. `http://localhost:8788`.
    pub fn new(base: String) -> Self {
        ItemDb {
            base: base.trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .unwrap_or_default(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// Look one item up by the name the client writes.
    ///
    /// `None` means the site does not know it, or could not be reached — the
    /// caller cannot tell those apart and does not need to, since the answer
    /// in both cases is to fall back.
    pub async fn item(&self, name: &str) -> Option<Value> {
        let key = slug(name);
        if key.is_empty() {
            return None;
        }
        if let Some(v) = self.cached(&key) {
            return v;
        }
        let url = format!("{}/api/item?name={}", self.base, key);
        let found = match self.client.get(&url).send().await {
            Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
            _ => None,
        };
        // The site answers 200 with `count: 0` for a name it does not know,
        // which is a miss and not a hit with nothing in it.
        let found = found.filter(|v| v.get("count").and_then(Value::as_u64).unwrap_or(0) > 0);
        self.remember(key, found.clone());
        found
    }

    fn cached(&self, key: &str) -> Option<Option<Value>> {
        let cache = self.cache.lock().ok()?;
        let (at, entry) = cache.get(key)?;
        let ttl = match entry {
            Cached::Hit(_) => HIT_TTL,
            Cached::Miss => MISS_TTL,
        };
        if at.elapsed() > ttl {
            return None;
        }
        Some(match entry {
            Cached::Hit(v) => Some(v.clone()),
            Cached::Miss => None,
        })
    }

    fn remember(&self, key: String, found: Option<Value>) {
        if let Ok(mut cache) = self.cache.lock() {
            let entry = match found {
                Some(v) => Cached::Hit(v),
                None => Cached::Miss,
            };
            cache.insert(key, (Instant::now(), entry));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client's spelling and the site's slug have to meet without a table
    /// mapping one to the other.
    #[test]
    fn slugs_match_the_sites_own_form() {
        assert_eq!(slug("Void-Touched Potential"), "void_touched_potential");
        assert_eq!(slug("Amulet of the Void"), "amulet_of_the_void");
        // Punctuation collapses rather than doubling the separator.
        assert_eq!(slug("Djarn's Amethyst Ring"), "djarn_s_amethyst_ring");
        assert_eq!(slug("Spiritualist`s Ring"), "spiritualist_s_ring");
        assert_eq!(slug("Staff of Elemental Mastery: Air"), "staff_of_elemental_mastery_air");
        // Leading and trailing punctuation leaves no stray underscore.
        assert_eq!(slug("  Fine Steel Long Sword  "), "fine_steel_long_sword");
        assert_eq!(slug("!!!"), "");
        assert_eq!(slug(""), "");
    }

    /// A graded drop is the same item to the site as it is to the tracker.
    #[test]
    fn a_grade_is_not_part_of_the_slug() {
        use eqlparser_core::drops::canonical_item;
        assert_eq!(slug(canonical_item("Woven Skull Cap +1")), "woven_skull_cap");
        assert_eq!(slug(canonical_item("Silvery Ring +2")), "silvery_ring");
    }
}
