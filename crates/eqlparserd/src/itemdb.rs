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

/// Percent-encode a query value. Small by hand rather than pulling a crate in
/// for one call: spell names carry spaces, apostrophes and backticks.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
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
    /// Origins to try, in order. See [`ItemDb::new`].
    bases: Vec<String>,
    /// Whichever origin last answered, so the caller can say where a card's
    /// facts came from and so icons are fetched from the same place.
    live: Mutex<Option<String>>,
    client: reqwest::Client,
    cache: Mutex<HashMap<String, (Instant, Cached)>>,
    icons: Mutex<HashMap<String, (Instant, Option<(String, Vec<u8>)>)>>,
}

impl ItemDb {
    /// `bases` are origins to try in order, e.g. the local instance followed
    /// by the published site.
    ///
    /// A list rather than one origin so that a local instance can be put in
    /// front of the published site without a rebuild. The default is the
    /// published site alone: an origin that answers differently depending on
    /// what happens to be listening on the developer's machine is not a data
    /// source, it is a coin toss.
    ///
    /// The cost of an origin that cannot be reached is a refused connection,
    /// measured in microseconds, and the next one in the list is tried.
    pub fn new(bases: Vec<String>) -> Self {
        ItemDb {
            bases: bases
                .into_iter()
                .map(|b| b.trim_end_matches('/').to_string())
                .filter(|b| !b.is_empty())
                .collect(),
            live: Mutex::new(None),
            client: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .unwrap_or_default(),
            cache: Mutex::new(HashMap::new()),
            icons: Mutex::new(HashMap::new()),
        }
    }

    /// The origin that last answered, or the first configured one before
    /// anything has.
    pub fn base(&self) -> String {
        self.live
            .lock()
            .ok()
            .and_then(|l| l.clone())
            .or_else(|| self.bases.first().cloned())
            .unwrap_or_default()
    }

    /// Fetch an icon sheet through us rather than letting the window fetch it.
    ///
    /// The window's content-security policy would otherwise have to name every
    /// origin the site might ever be served from, and it is about to move from
    /// localhost to a public domain. Proxying means the policy names only this
    /// daemon and never changes again.
    ///
    /// `path` is the site-relative path eqdb gave us. Only its own static
    /// assets are fetched: this must not become a way to ask the daemon to
    /// retrieve arbitrary URLs on the caller's behalf.
    pub async fn icon(&self, path: &str) -> Option<(String, Vec<u8>)> {
        if !path.starts_with("/static/") || path.contains("..") {
            return None;
        }
        if let Some(hit) = self.cached_icon(path) {
            return hit;
        }
        let mut found = None;
        for base in self.origins() {
            let url = format!("{base}{path}");
            if let Ok(r) = self.client.get(&url).send().await {
                if r.status().is_success() {
                    let ct = r
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("image/png")
                        .to_string();
                    if let Ok(bytes) = r.bytes().await {
                        found = Some((ct, bytes.to_vec()));
                        break;
                    }
                }
            }
        }
        if let Ok(mut c) = self.icons.lock() {
            c.insert(path.to_string(), (Instant::now(), found.clone()));
        }
        found
    }

    fn cached_icon(&self, path: &str) -> Option<Option<(String, Vec<u8>)>> {
        let c = self.icons.lock().ok()?;
        let (at, entry) = c.get(path)?;
        let ttl = if entry.is_some() { HIT_TTL } else { MISS_TTL };
        if at.elapsed() > ttl {
            return None;
        }
        Some(entry.clone())
    }

    /// The origins to try, most-recently-successful first — so once one has
    /// answered, the others are not dialled again on every lookup.
    fn origins(&self) -> Vec<String> {
        let live = self.live.lock().ok().and_then(|l| l.clone());
        let mut out: Vec<String> = Vec::with_capacity(self.bases.len());
        if let Some(l) = live {
            out.push(l);
        }
        for b in &self.bases {
            if !out.contains(b) {
                out.push(b.clone());
            }
        }
        out
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
        let mut found = None;
        for base in self.origins() {
            let url = format!("{base}/api/item?name={key}");
            let got = match self.client.get(&url).send().await {
                Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
                _ => continue,
            };
            // The site answers 200 with `count: 0` for a name it does not
            // know. That is an answer from a reachable site, so the origin is
            // live even though the item is a miss — remember it either way, or
            // every unknown name re-dials the whole list.
            if let Ok(mut l) = self.live.lock() {
                *l = Some(base.clone());
            }
            found = got.filter(|v| v.get("count").and_then(Value::as_u64).unwrap_or(0) > 0);
            break;
        }
        self.remember(key, found.clone());
        found
    }

    /// Look a spell up, by the site's own id when we know it.
    ///
    /// The id is the precise question — ranks share a display name and the
    /// site cannot tell "Puma Maw II" from "Puma Maw III" by text — so the
    /// caller resolves it against the local spell file first and only falls
    /// back to asking by name.
    ///
    /// Note the asymmetry with items, which are looked up by slug: spells are
    /// asked for by their display name exactly as the client writes it. That
    /// is the site's shape, not ours.
    pub async fn spell(&self, id: Option<u32>, name: &str) -> Option<Value> {
        let key = match id {
            Some(i) => format!("spell:{i}"),
            None => format!("spell:name:{}", name.to_lowercase()),
        };
        if let Some(v) = self.cached(&key) {
            return v;
        }
        let mut found = None;
        for base in self.origins() {
            let url = match id {
                Some(i) => format!("{base}/api/spell/{i}"),
                None => format!("{base}/api/spell?name={}", urlencode(name)),
            };
            let got = match self.client.get(&url).send().await {
                Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
                _ => continue,
            };
            if let Ok(mut l) = self.live.lock() {
                *l = Some(base.clone());
            }
            // By id the payload is the spell itself; by name it is a match
            // list, which is a miss when empty.
            found = got.filter(|v| {
                if id.is_some() {
                    v.get("id").is_some()
                } else {
                    v.get("count").and_then(Value::as_u64).unwrap_or(0) > 0
                }
            });
            break;
        }
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
        assert_eq!(
            slug("Staff of Elemental Mastery: Air"),
            "staff_of_elemental_mastery_air"
        );
        // Leading and trailing punctuation leaves no stray underscore.
        assert_eq!(slug("  Fine Steel Long Sword  "), "fine_steel_long_sword");
        assert_eq!(slug("!!!"), "");
        assert_eq!(slug(""), "");
    }

    #[test]
    fn query_values_are_escaped() {
        assert_eq!(urlencode("Puma Maw"), "Puma%20Maw");
        assert_eq!(urlencode("Al`Kabor's Cap"), "Al%60Kabor%27s%20Cap");
        assert_eq!(urlencode("Selo's Chords"), "Selo%27s%20Chords");
        assert_eq!(urlencode("plain"), "plain");
    }

    /// The origin list is tried in order, and only the paths eqdb serves its
    /// own assets from are ever fetched.
    #[test]
    fn the_icon_proxy_refuses_anything_that_is_not_a_site_asset() {
        let db = ItemDb::new(vec!["http://127.0.0.1:1".into()]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Not under /static/, so it is not asked for at all — this must not
        // become a way to have the daemon fetch arbitrary URLs.
        assert!(rt.block_on(db.icon("/etc/passwd")).is_none());
        assert!(rt.block_on(db.icon("/api/item?name=x")).is_none());
        assert!(rt.block_on(db.icon("/static/../secret")).is_none());
        assert!(rt.block_on(db.icon("")).is_none());
        // A legitimate path is attempted, and fails only because nothing is
        // listening.
        assert!(rt.block_on(db.icon("/static/items/042.png")).is_none());
    }

    /// Configuring one origin must not leave the defaults behind it: having
    /// named a site, silently falling back to another is not what was meant.
    #[test]
    fn base_reports_the_first_configured_origin_before_anything_answers() {
        let db = ItemDb::new(vec![
            "http://localhost:8788/".into(),
            "https://eqldb.hammackj.com".into(),
        ]);
        assert_eq!(db.base(), "http://localhost:8788", "trailing slash trimmed");
        // The shipped default is the published site and nothing else.
        assert_eq!(
            ItemDb::new(vec!["https://eqldb.hammackj.com".into()]).base(),
            "https://eqldb.hammackj.com"
        );
        let empty = ItemDb::new(vec![]);
        assert_eq!(empty.base(), "");
    }

    /// A graded drop is the same item to the site as it is to the tracker.
    #[test]
    fn a_grade_is_not_part_of_the_slug() {
        use eqlparser_core::drops::canonical_item;
        assert_eq!(
            slug(canonical_item("Woven Skull Cap +1")),
            "woven_skull_cap"
        );
        assert_eq!(slug(canonical_item("Silvery Ring +2")), "silvery_ring");
    }
}
