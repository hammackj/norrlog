//! From `Taking a screenshot...` to a window in eqdb's review queue.
//!
//! # The log is the notification
//!
//! The client writes `Taking a screenshot...` to the character log in the same
//! second it writes the file — checked against a day of shots, the log line
//! and the file's mtime agree exactly. We are already following that log, so
//! there is no directory to poll and no watch to place on what is often a
//! network or a wine-mapped mount. It is also better evidence than a new file
//! would be: it says *the game* took this picture, which a file appearing does
//! not. [`Engine::shots_taken`] is the queue.
//!
//! [`Engine::shots_taken`]: crate::engine::Engine::shots_taken
//!
//! # Reading the picture is somebody else's job
//!
//! The decoder is 900 lines of glyph matching and the item grammar another
//! 1,700, tuned against 1,147 crops to an abstention model that accepts three
//! fields in five and is right about them 99.85% of the time. That is not a
//! thing to reimplement in order to have one binary; a port that lost a
//! quarter of a percent would cost more than it saved, and nothing here would
//! notice it had.
//!
//! So this shells out to `ocr/oneshot.py`, whose whole contract is: a
//! screenshot path in, JSON out naming the item windows in it. That is a
//! narrow enough boundary to replace later — the day the decoder is Rust, one
//! function changes and nothing else does.
//!
//! It is optional in the strong sense. A daemon with no OCR tree next to it
//! says so once and runs exactly as it did before; every other feature is
//! untouched.
//!
//! # What is sent, and what is not
//!
//! Not every window. The queue is a person's time, and the rules are the ones
//! `ocr/push.py` already applies:
//!
//!   * A **graded** window is never sent. Graded stats are derived by the site
//!     from a base reading, so a `+4` copy is an output of that rule rather
//!     than an input to it. `oneshot.py` says so in `held`.
//!   * An item eqdb **wants** is always sent. That is the point of this.
//!   * An item eqdb has **never heard of** is sent, because a new item is
//!     exactly what nobody can have asked for.
//!   * An item eqdb holds and does not want is **not** sent, and the daemon
//!     says which item it was rather than staying quiet about it.
//!   * Except a **Lore** window for an item eqdb has no lore for, which is
//!     sent even though the item is covered. The wanted list is built from
//!     `facts = 0 OR base_facts = 0 OR icons = 0` -- an item's stats reading
//!     and its icon. Lore is in neither term, so an item leaves the list the
//!     moment its Description tab is photographed, and the Lore tab is a
//!     *second* photograph of a *second* tab, almost always taken next. Every
//!     one of those was arriving here as "covered" and going no further.
//!     Measured 5 September against production: 143 of the 146 items whose
//!     lore is on file were no longer wanted when it was taken, and the site
//!     holds 1,865 items with stats against 161 with lore.
//!
//! A sent window is `state = review` on the site and changes no item page.
//! `ocr/verdicts.py` and the ordinary publish are still what carry a reading
//! into the data.

use crate::engine::Engine;
use crate::locks::Guarded;
use serde::Deserialize;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// How often the drain runs.
const TICK: Duration = Duration::from_secs(2);

/// How long a shot notice waits for its file before being given up on.
///
/// The file lands in the same second as the log line, so this is not about
/// latency — it is about a shot taken while the disk is busy, and about the
/// archiver that moves files out of the inbox seconds after they arrive. Ten
/// seconds is long enough for the first and short enough that a picture the
/// archiver won a race for is reported as missed rather than waited on.
const WAIT_FOR_FILE: i64 = 10;

/// How far before the log line a file's mtime may sit and still be its file.
///
/// One second, for a clock that rounds the other way. Wider would let the
/// previous screenshot answer for this one, which is worse than missing it:
/// the queue would show a person the wrong picture beside the right reading.
const MTIME_SLACK: i64 = 1;

/// How long the reader is given before it is assumed to be stuck.
const READ_TIMEOUT: Duration = Duration::from_secs(90);

/// How many outcomes to keep for the status endpoint.
const KEEP_RECENT: usize = 40;

/// How many cut windows to keep on disk.
///
/// These are a cache and not a dataset: the screenshot they came from is
/// still in the client's folder, the reading is on the site, and the crop can
/// be cut again from either. Something has to bound them, though — a window
/// is ~150 KB and a session can be dozens — and nothing else here would ever
/// delete one. They live under the daemon's own data directory rather than in
/// the OCR tree for exactly this reason: pruning somebody's dataset would be a
/// different and much worse thing to do.
const KEEP_CROPS: usize = 500;

/// A shot the daemon has finished with, for `/api/shots/recent`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Outcome {
    /// The screenshot's filename.
    pub shot: String,
    /// When the client said it took it, in the log's clock — the same domain
    /// as every other timestamp this daemon serves, and *not* a Unix epoch on
    /// this machine. The client writes local wall time with no zone on it.
    pub at: i64,
    /// The item, when a window was read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// `queued`, `held`, `covered`, `no window`, `missed`, or a failure.
    pub verdict: String,
    /// Why, in a sentence, for the verdicts where why is the interesting part.
    #[serde(skip_serializing_if = "str::is_empty")]
    pub detail: String,
}

/// The shot reader's own state: what it is still waiting for, what it did.
#[derive(Default)]
pub struct Log {
    recent: VecDeque<Outcome>,
    /// Shots seen but whose file has not turned up yet, with the time they
    /// were first asked about.
    waiting: usize,
}

impl Log {
    fn note(&mut self, o: Outcome) {
        self.recent.push_back(o);
        while self.recent.len() > KEEP_RECENT {
            self.recent.pop_front();
        }
    }

    /// Newest first, which is the order a person reads them in.
    pub fn recent(&self) -> Vec<Outcome> {
        self.recent.iter().rev().cloned().collect()
    }

    pub fn waiting(&self) -> usize {
        self.waiting
    }
}

/// What `oneshot.py` answers with.
#[derive(Debug, Deserialize)]
struct Read {
    shots: Vec<Shot>,
}

#[derive(Debug, Deserialize)]
struct Shot {
    #[serde(default)]
    windows: Vec<Window>,
    #[serde(default)]
    error: Option<String>,
}

/// One item window, as the reader saw it. Everything but `crop_path` and
/// `held` is passed through to eqdb untouched — this is deliberately not a
/// re-modelling of the reading, which `eqldb-core` owns.
#[derive(Debug, Deserialize)]
struct Window {
    crop: String,
    crop_path: String,
    #[serde(default)]
    held: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    record: serde_json::Value,
    #[serde(default)]
    fields: serde_json::Value,
    #[serde(default)]
    needs_review: Vec<String>,
    #[serde(default)]
    lines: serde_json::Value,
}

impl Window {
    fn item(&self) -> String {
        self.record
            .get("item")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    fn unread(&self) -> bool {
        self.error.is_some() || self.needs_review.iter().any(|n| n == "__error__")
    }

    /// The lore this window read, empty if it read none.
    ///
    /// `parse.py` files a window's body text under `lore` only when the tab
    /// row says `Lore`; on the Description tab the same text is a `note`. So
    /// this is empty on every window that is not a Lore window, and on a Lore
    /// window whose text did not come out.
    fn lore(&self) -> String {
        self.record
            .get("lore")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    /// Whether this is a reading of the item's Lore tab.
    ///
    /// The tab, not the text. A Lore window whose lore did not decode is
    /// still a photograph of the lore, and a person reading what the decoder
    /// could not is the entire point of the review queue -- suppressing those
    /// would drop exactly the windows most in need of eyes. `parse.py` flags
    /// them itself: "the Lore tab is selected and no lore text was read".
    ///
    /// The second arm is for a window whose tab row was missed but whose lore
    /// came out anyway; `parse.py` cannot produce it today, and it costs one
    /// comparison to not depend on that staying true.
    fn is_lore(&self) -> bool {
        self.record.get("tab").and_then(serde_json::Value::as_str) == Some("Lore")
            || !self.lore().is_empty()
    }
}

/// Whether an `/api/item` payload already carries lore for the item.
///
/// A free function over the payload rather than a method, so the rule can be
/// tested without a site to ask.
///
/// `all`, not `any`: a name can match twins, and lore on one of them is not
/// lore on the other. A match with no facts fails its term, and an empty or
/// absent `matches` is false -- every uncertain answer lands on "not on file",
/// because the cost of being wrong that way is a duplicate in the queue and
/// the cost of being wrong the other way is a photograph nobody can retake
/// without going back to the zone.
/// Whether a window is sent, given where its item stands with eqdb.
///
/// The three rules in one place and out of `handle`, which cannot be called
/// without a site, a subprocess and an engine. This is the whole of the
/// decision and it is a table.
fn sends(wanted: bool, known: bool, teaches_lore: bool) -> bool {
    wanted || !known || teaches_lore
}

fn lore_on_file(v: &serde_json::Value) -> bool {
    let Some(matches) = v.get("matches").and_then(serde_json::Value::as_array) else {
        return false;
    };
    !matches.is_empty()
        && matches.iter().all(|m| {
            m.get("facts")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|facts| {
                    facts.iter().any(|f| {
                        f.get("lore")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|l| !l.trim().is_empty())
                    })
                })
        })
}

/// Everything the reader needs, or nothing and it does not run.
pub struct Reader {
    /// `~/Pictures/eql-items`, holding `ocr/oneshot.py`.
    ocr: PathBuf,
    /// The client's `Screenshots` folder.
    shots: PathBuf,
    /// Where cut windows are written. Under the daemon's own data directory:
    /// the OCR tree is somebody's dataset and this is a cache.
    crops: PathBuf,
    python: String,
    itemdb: Arc<crate::itemdb::ItemDb>,
    http: reqwest::Client,
    site: String,
    token: String,
    pub log: Arc<Mutex<Log>>,
}

/// Where the token may come from, in order.
///
/// Never a flag. `/proc/<pid>/cmdline` is world-readable and the environment
/// is not, so a token passed as an argument is a token every process on the
/// machine can read — the same rule the eqldb binaries enforce by refusing a
/// database URL on `--db`.
fn token_from(config: Option<&Path>) -> Option<String> {
    if let Ok(t) = std::env::var("EQLDB_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    let path = config?.join("eqldb-token");
    let meta = std::fs::metadata(&path).ok()?;
    // A token any account on this machine can read is not a secret, and
    // quietly using it would teach that it was one.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            eprintln!(
                "[eqlparserd] {} is readable by other accounts (mode {:o}); \
                 chmod 600 it. Not using it.",
                path.display(),
                meta.permissions().mode() & 0o7777
            );
            return None;
        }
    }
    let _ = meta;
    let t = std::fs::read_to_string(&path).ok()?.trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// What the reader would be built from, kept so it can be built again.
///
/// Held apart from `Reader` because a token can arrive after the daemon has
/// started — somebody pastes one into the settings — and the alternative to
/// rebuilding is telling them to restart the app to use the thing they just
/// configured.
pub struct Setup {
    pub ocr: Option<PathBuf>,
    pub shots: Option<PathBuf>,
    pub game_dir: Option<PathBuf>,
    pub itemdb: Arc<crate::itemdb::ItemDb>,
    /// `--no-shots`. Kept here rather than checked by the caller so that
    /// "off because you asked" and "off because a piece is missing" come out
    /// of one place and read the same way.
    pub off: bool,
}

impl Reader {
    /// Assemble one, or explain in a sentence why there is not one.
    ///
    /// Every miss is a `Err(String)` rather than a `None`, because "the shot
    /// reader is off" and "the shot reader is off *because*" are different
    /// amounts of help and the second costs nothing.
    pub fn configure(
        ocr: Option<PathBuf>,
        shots: Option<PathBuf>,
        game_dir: Option<&Path>,
        itemdb: Arc<crate::itemdb::ItemDb>,
        // The outcomes seen so far. Passed in rather than made here so that
        // swapping the reader -- which is what pasting a token does -- keeps
        // the list a person is looking at.
        log: Arc<Mutex<Log>>,
    ) -> Result<Reader, String> {
        let ocr = ocr
            .or_else(|| std::env::var_os("EQL_OCR_DIR").map(PathBuf::from))
            .or_else(|| home().map(|h| h.join("Pictures/eql-items")))
            .ok_or("no OCR tree; pass --ocr <dir>")?;
        if !ocr.join("ocr").join("oneshot.py").is_file() {
            return Err(format!(
                "{} holds no ocr/oneshot.py, so screenshots are not read",
                ocr.display()
            ));
        }
        let shots = shots
            .or_else(|| std::env::var_os("EQL_SHOTS_DIR").map(PathBuf::from))
            .or_else(|| game_dir.map(|g| g.join("Screenshots")))
            .ok_or("no Screenshots folder; pass --shots <dir> or --game-dir")?;
        if !shots.is_dir() {
            return Err(format!("{} is not a directory", shots.display()));
        }
        let token = token_from(crate::paths::config_dir().as_deref()).ok_or(
            "no eqdb token: set EQLDB_TOKEN, or put one in \
             ~/.config/eqlparser/eqldb-token with mode 600",
        )?;
        let crops = crate::paths::data_dir()
            .ok_or("nowhere to write cut windows")?
            .join("crops");
        std::fs::create_dir_all(&crops).map_err(|e| format!("{}: {e}", crops.display()))?;
        Ok(Reader {
            ocr,
            shots,
            crops,
            python: std::env::var("EQL_PYTHON").unwrap_or_else(|_| "python3".into()),
            site: itemdb.base(),
            itemdb,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(|e| e.to_string())?,
            token,
            log,
        })
    }

    /// The screenshot the client wrote at `at`, if it is on disk yet.
    ///
    /// `at` is in *this machine's* clock, not the log's. The client writes
    /// local wall time into the log with no zone on it, so a log timestamp
    /// read back is out by the local offset — five hours, on the machine this
    /// was written on — and matching it against an mtime without crossing
    /// that looks into the past and finds nothing, every time. The caller
    /// crosses it with [`Engine::clock_offset`], which the engine measures
    /// anyway for buff timers.
    ///
    /// [`Engine::clock_offset`]: crate::engine::Engine::clock_offset
    ///
    /// Matched on mtime rather than on the newest file, because "newest" is
    /// wrong twice: two shots in the same second are both newest, and a shot
    /// whose file the archiver has already moved would be answered for by the
    /// *previous* one. A window shown beside somebody else's reading is worse
    /// than a window not shown.
    fn file_at(&self, at: i64) -> Option<PathBuf> {
        let mut best: Option<(i64, PathBuf)> = None;
        for e in std::fs::read_dir(&self.shots).ok()?.flatten() {
            let p = e.path();
            if !p.is_file() || !is_picture(&p) {
                continue;
            }
            let Ok(m) = e.metadata() else { continue };
            let Some(secs) = mtime_secs(&m) else { continue };
            if secs < at - MTIME_SLACK || secs > at + WAIT_FOR_FILE {
                continue;
            }
            // Closest to the moment claimed wins.
            let d = (secs - at).abs();
            if best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                best = Some((d, p));
            }
        }
        best.map(|(_, p)| p)
    }

    /// Run the reader over one picture.
    async fn read(&self, path: &Path) -> Result<Vec<Window>, String> {
        let script = self.ocr.join("ocr").join("oneshot.py");
        let out = tokio::time::timeout(
            READ_TIMEOUT,
            tokio::process::Command::new(&self.python)
                .arg(&script)
                .arg("--out")
                .arg(&self.crops)
                .arg(path)
                .current_dir(&self.ocr)
                // The reader prints its own diagnostics; they belong on our
                // stderr, where the rest of this daemon's are.
                .stdin(std::process::Stdio::null())
                .output(),
        )
        .await
        .map_err(|_| format!("the reader did not finish within {}s", READ_TIMEOUT.as_secs()))?
        .map_err(|e| format!("could not run {}: {e}", script.display()))?;
        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "the reader failed: {}",
                why.trim().lines().next_back().unwrap_or("no output")
            ));
        }
        let parsed: Read = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("the reader's answer did not parse: {e}"))?;
        let mut windows = Vec::new();
        for s in parsed.shots {
            if let Some(e) = s.error {
                return Err(e);
            }
            windows.extend(s.windows);
        }
        Ok(windows)
    }

    /// Whether eqdb wants this item photographed, and whether it knows it at
    /// all. `(wanted, known)`.
    async fn standing(&self, item: &str) -> (bool, bool) {
        let key = crate::itemdb::match_key(item);
        let wanted = match self.itemdb.wanted(false).await {
            Some((v, _)) => v
                .get("items")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|a| {
                    a.iter().any(|i| {
                        i.get("item")
                            .or_else(|| i.get("name"))
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|n| crate::itemdb::match_key(n) == key)
                    })
                }),
            // No answer is not "not wanted". An unreachable site must not
            // silently turn every window into one nobody needs.
            None => return (true, true),
        };
        if wanted {
            return (true, true);
        }
        (false, self.itemdb.item(item).await.is_some())
    }

    /// Whether eqdb already holds lore for this item.
    ///
    /// False when it holds the item but no lore, when it does not hold the
    /// item, and when it could not be asked. Only a positive answer may
    /// suppress a window, so an unreachable site sends rather than discards --
    /// the same way [`Self::standing`] treats no answer as "wanted".
    ///
    /// `standing` asked `/api/item` for this same name a moment ago and
    /// `ItemDb` caches the answer, so on the path that calls both this is not
    /// a second request.
    async fn lore_held(&self, item: &str) -> bool {
        self.itemdb.item(item).await.as_ref().is_some_and(lore_on_file)
    }

    /// The picture, then the reading citing it — the order `ocr/push.py` uses,
    /// so a reading is never stored pointing at a picture that did not arrive.
    async fn send(&self, w: &Window) -> Result<String, String> {
        let png = tokio::fs::read(&w.crop_path)
            .await
            .map_err(|e| format!("{}: {e}", w.crop_path))?;
        let shot: serde_json::Value = self
            .post(
                "/api/shots",
                reqwest::Body::from(png),
                "application/octet-stream",
                &[("X-Shot-Name", w.crop.as_str())],
            )
            .await?;
        let reading = serde_json::json!({
            "crop": w.crop,
            "record": w.record,
            "original": w.record,
            "fields": w.fields,
            "needs_review": w.needs_review,
            "lines": w.lines,
            "shot_key": shot.get("shot_key").and_then(serde_json::Value::as_str).unwrap_or(""),
            "shot_format": shot.get("format").and_then(serde_json::Value::as_str).unwrap_or("png"),
            "reader": "eqlparserd",
            "read_at": now(),
        });
        let body = serde_json::to_vec(&reading).map_err(|e| e.to_string())?;
        let out: serde_json::Value = self
            .post("/api/readings", reqwest::Body::from(body), "application/json", &[])
            .await?;
        Ok(out
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("queued")
            .to_string())
    }

    async fn post(
        &self,
        path: &str,
        body: reqwest::Body,
        ctype: &str,
        extra: &[(&str, &str)],
    ) -> Result<serde_json::Value, String> {
        let mut req = self
            .http
            .post(format!("{}{path}", self.site.trim_end_matches('/')))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", ctype);
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        let r = req.body(body).send().await.map_err(|e| e.to_string())?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        if !status.is_success() {
            // The site's own words. Its refusals name the reason — "already
            // held", a quota, a 403 on the contributor name — and paraphrasing
            // them into "upload failed" would throw away the only part worth
            // reading.
            return Err(format!(
                "{} {}",
                status.as_u16(),
                serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v
                        .get("detail")
                        .or_else(|| v.get("status"))
                        .and_then(|d| d.as_str().map(str::to_string)))
                    .unwrap_or_else(|| text.chars().take(160).collect())
            ));
        }
        Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
    }

    /// One screenshot, all the way through.
    async fn handle(&self, at: i64, path: PathBuf, engine: &Arc<Mutex<Engine>>) {
        let shot = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let windows = match self.read(&path).await {
            Ok(w) => w,
            Err(e) => {
                self.note(Outcome { shot, at, item: None, verdict: "unread".into(), detail: e });
                return;
            }
        };
        if windows.is_empty() {
            self.note(Outcome {
                shot,
                at,
                item: None,
                verdict: "no window".into(),
                detail: String::new(),
            });
            return;
        }
        for w in &windows {
            let item = w.item();
            let mut out = Outcome {
                shot: shot.clone(),
                at,
                item: (!item.is_empty()).then(|| item.clone()),
                verdict: String::new(),
                detail: String::new(),
            };
            if w.unread() {
                out.verdict = "unread".into();
                out.detail = w.error.clone().unwrap_or_else(|| "the reader could not read it".into());
            } else if item.is_empty() {
                out.verdict = "no window".into();
            } else if let Some(why) = w.held.clone() {
                out.verdict = "held".into();
                out.detail = why;
            } else {
                // Ticked whatever eqdb goes on to say. The person did take the
                // picture, and the overlay must stop asking for it the moment
                // they did — that is what the mark is for, and it is not a
                // claim that the reading was any good.
                //
                // Not gated on the mark already being set, deliberately. A
                // second photograph of the same item is usually somebody
                // re-shooting one that came out badly, and the costs are not
                // symmetric: a duplicate in the queue is one click, and a
                // re-shot refused is a photograph they have to go and take
                // again — of an item eqdb is still asking for. The mark can
                // also predate this feature, having been ticked by hand.
                engine.locked("engine").set_screenshot_mark(&item, true);
                let (wanted, known) = self.standing(&item).await;
                // A Lore window for an item whose lore eqdb does not have is
                // sent even though the item itself is covered. The wanted
                // list cannot express this gap: it is computed from the
                // stats reading and the icon, so it goes quiet about an item
                // the moment its Description tab lands -- which is one tab
                // and a few seconds before its Lore tab is photographed.
                let teaches_lore = w.is_lore() && !self.lore_held(&item).await;
                if !sends(wanted, known, teaches_lore) {
                    out.verdict = "covered".into();
                    out.detail = "eqdb holds this item and is not asking for it".into();
                } else {
                    match self.send(w).await {
                        Ok(status) => {
                            out.verdict = status;
                            // Ordered so each arm is true where it is
                            // reached: the last is only reachable when the
                            // item is known and unwanted, which in this
                            // branch means `teaches_lore`.
                            out.detail = if wanted {
                                "eqdb was asking for this one".into()
                            } else if !known {
                                "eqdb has never heard of this item".into()
                            } else {
                                "eqdb holds this item with no lore, and this window has it".into()
                            };
                        }
                        Err(e) => {
                            out.verdict = "not sent".into();
                            out.detail = e;
                        }
                    }
                }
            }
            eprintln!(
                "[eqlparserd] shot {}: {} — {}{}",
                out.shot,
                out.item.clone().unwrap_or_else(|| "no item window".into()),
                out.verdict,
                if out.detail.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", out.detail)
                }
            );
            self.note(out);
        }
        self.prune();
    }

    fn note(&self, o: Outcome) {
        if let Ok(mut l) = self.log.lock() {
            l.note(o);
        }
    }

    /// Drop the oldest cut windows once there are too many.
    ///
    /// Oldest by mtime, which for a file this wrote is when it wrote it.
    /// Errors are ignored throughout: failing to tidy a cache is not a reason
    /// to stop reading screenshots, and there is nobody to tell.
    fn prune(&self) {
        let Ok(dir) = std::fs::read_dir(&self.crops) else {
            return;
        };
        let mut files: Vec<(i64, PathBuf)> = dir
            .flatten()
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                m.is_file().then(|| (mtime_secs(&m).unwrap_or(0), e.path()))
            })
            .collect();
        if files.len() <= KEEP_CROPS {
            return;
        }
        files.sort_by_key(|(t, _)| *t);
        for (_, p) in files.iter().take(files.len() - KEEP_CROPS) {
            let _ = std::fs::remove_file(p);
        }
    }

    /// The folder new screenshots are expected in.
    pub fn watching(&self) -> &Path {
        &self.shots
    }
}

/// The reader, and the ability to not have one.
///
/// Every piece the reader needs can arrive after the daemon has started: the
/// OCR tree can be installed, the Screenshots folder can be created by the
/// first screenshot, and the token is pasted into a settings box. Building
/// once at startup meant each of those was "restart the app" -- which for the
/// token is asking somebody to restart in order to use the thing they have
/// just set up.
///
/// So the loop always runs and the reader is a slot in it. `why` is what is
/// missing, in a sentence, and is what `/api/shots/recent` reports when there
/// is nothing there.
pub struct Shots {
    setup: Setup,
    reader: RwLock<Option<Arc<Reader>>>,
    /// Outcomes, held here rather than on the reader so they survive a swap.
    log: Arc<Mutex<Log>>,
    why: RwLock<String>,
}

impl Shots {
    pub fn new(setup: Setup) -> Arc<Shots> {
        let shots = Arc::new(Shots {
            setup,
            reader: RwLock::new(None),
            log: Arc::new(Mutex::new(Log::default())),
            why: RwLock::new(String::new()),
        });
        // The failure is not dropped, it is *stored* -- `rebuild` puts the
        // reason in `why`, which is where every caller reads it from.
        let _ = shots.rebuild();
        shots
    }

    /// Try again with whatever is on disk now. Says what it found, for the
    /// startup line and for the settings box that just changed something.
    pub fn rebuild(&self) -> Result<String, String> {
        if self.setup.off {
            self.put(None, "not read (--no-shots)");
            return Err("--no-shots".into());
        }
        match Reader::configure(
            self.setup.ocr.clone(),
            self.setup.shots.clone(),
            self.setup.game_dir.as_deref(),
            Arc::clone(&self.setup.itemdb),
            Arc::clone(&self.log),
        ) {
            Ok(r) => {
                let where_ = r.watching().display().to_string();
                let site = self.setup.itemdb.base();
                self.put(Some(Arc::new(r)), "");
                Ok(format!(
                    "reading {where_} and sending item windows to {site}"
                ))
            }
            Err(why) => {
                self.put(None, &why);
                Err(why)
            }
        }
    }

    fn put(&self, reader: Option<Arc<Reader>>, why: &str) {
        if let Ok(mut slot) = self.reader.write() {
            *slot = reader;
        }
        if let Ok(mut w) = self.why.write() {
            *w = why.to_string();
        }
    }

    fn reader(&self) -> Option<Arc<Reader>> {
        self.reader.read().ok().and_then(|r| r.clone())
    }

    pub fn running(&self) -> bool {
        self.reader().is_some()
    }

    /// What it is doing, for the startup line. Empty when it is not running,
    /// where [`Shots::why`] is the sentence instead.
    pub fn why_running(&self) -> String {
        match self.reader() {
            Some(r) => format!(
                "reading {} and sending item windows to {}",
                r.watching().display(),
                self.setup.itemdb.base()
            ),
            None => String::new(),
        }
    }

    pub fn why(&self) -> String {
        self.why.read().map(|w| w.clone()).unwrap_or_default()
    }

    pub fn log(&self) -> &Arc<Mutex<Log>> {
        &self.log
    }

    /// Where a token would be kept, if this machine has somewhere to keep it.
    pub fn token_path() -> Option<PathBuf> {
        crate::paths::config_dir().map(|d| d.join("eqldb-token"))
    }

    /// Whether a token is set, and where it came from.
    ///
    /// Never the value. This endpoint is reachable by anything that can talk
    /// to localhost, and a credential that can be written but not read back is
    /// strictly better than one that can be done both to: the worst a stray
    /// local page can do here is break the reader, which is visible, rather
    /// than take the token, which is not.
    pub fn token_state(&self) -> (bool, &'static str) {
        if std::env::var("EQLDB_TOKEN").is_ok_and(|t| !t.trim().is_empty()) {
            return (true, "EQLDB_TOKEN");
        }
        match Self::token_path() {
            Some(p) if p.is_file() => (true, "file"),
            _ => (false, "none"),
        }
    }

    /// Write a token and start reading with it. An empty one removes the file.
    ///
    /// Mode 600 from the moment it exists, not after: creating it readable and
    /// chmodding it afterwards leaves a window in which any account on the
    /// machine can have it, and that window is all an attacker needs.
    pub fn set_token(&self, token: &str) -> Result<String, String> {
        let path = Self::token_path().ok_or("nowhere to keep a token on this machine")?;
        let token = token.trim();
        if token.is_empty() {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
            let _ = self.rebuild();
            return Ok("The token was removed.".into());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        write_privately(&path, token).map_err(|e| format!("{}: {e}", path.display()))?;
        // Said, not silently overridden. The environment wins in `configure`,
        // so a token pasted here while `EQLDB_TOKEN` is set would be written,
        // ignored, and blamed for whatever the other one does.
        let shadowed = std::env::var("EQLDB_TOKEN").is_ok_and(|t| !t.trim().is_empty());
        match self.rebuild() {
            Ok(what) if shadowed => Ok(format!(
                "Saved, but EQLDB_TOKEN is set in this daemon's environment and \
                 is what will be used. {what}"
            )),
            Ok(what) => Ok(format!("Saved. {what}")),
            Err(why) => Err(format!("Saved, but the reader is still off: {why}")),
        }
    }
}

/// Write a file only its owner can read, and never for an instant otherwise.
fn write_privately(path: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    // `create` keeps the mode of a file that already exists, so an existing
    // one is tightened explicitly. Before the write, so the new value is
    // never in a file somebody else could read.
    #[cfg(unix)]
    if path.exists() {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    let mut f = opts.open(path)?;
    f.write_all(body.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()
}

/// Watch the drained shot notices and put each one through the reader.
///
/// Shots are handled one at a time. The reader is CPU-heavy — a template pass
/// over a 2560px frame, in numpy — and a burst of screenshots taken in one
/// motion would otherwise start five of them at once on the machine that is
/// also running the game.
pub fn spawn(shots: Arc<Shots>, engine: Arc<Mutex<Engine>>) {
    tokio::spawn(async move {
        // Shots whose file has not turned up yet. Both clocks are kept: the
        // log's, because that is the timestamp a person will go and look for
        // in the log, and this machine's, because that is what an mtime is in.
        let mut pending: VecDeque<(i64, i64)> = VecDeque::new();
        loop {
            tokio::time::sleep(TICK).await;
            {
                let mut e = engine.locked("engine");
                // Read once, with the shots it belongs to. The offset is
                // remeasured on every live line, so taking it later would
                // convert this batch with a slightly different number than
                // the one in force when they were seen.
                let off = e.clock_offset().unwrap_or(0);
                for at in e.shots_taken() {
                    pending.push_back((at, at - off));
                }
            }
            // No reader: the notices are dropped rather than banked. A shot
            // taken before a token was pasted in is not one anybody is waiting
            // on, and a queue that survives being off would send a burst of
            // stale windows the moment it came on.
            let Some(reader) = shots.reader() else {
                pending.clear();
                continue;
            };
            let mut again = VecDeque::new();
            while let Some((at, local)) = pending.pop_front() {
                match reader.file_at(local) {
                    Some(p) => reader.handle(at, p, &engine).await,
                    None if now() - local < WAIT_FOR_FILE => again.push_back((at, local)),
                    None => reader.note(Outcome {
                        shot: String::new(),
                        at,
                        item: None,
                        verdict: "missed".into(),
                        detail: "the client said it took a picture and no file \
                                 with that timestamp turned up"
                            .into(),
                    }),
                }
            }
            pending = again;
            // Counted after the drain, not before it. Before, a shot that had
            // just been handled was still reported as waiting until the next
            // tick, which is the one number on that endpoint a person reads as
            // "something is stuck".
            if let Ok(mut l) = shots.log().lock() {
                l.waiting = pending.len();
            }
        }
    });
}

fn home() -> Option<PathBuf> {
    let h = PathBuf::from(std::env::var_os("HOME")?);
    h.is_absolute().then_some(h)
}

fn is_picture(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| matches!(e.as_str(), "jpg" | "jpeg" | "png" | "bmp" | "gif" | "webp"))
}

fn mtime_secs(m: &std::fs::Metadata) -> Option<i64> {
    m.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Reader` with nothing but a folder to look in. Everything else here
    /// is either the network or a subprocess, and `file_at` touches neither.
    pub(super) fn looking_at(dir: &Path) -> Reader {
        Reader {
            ocr: PathBuf::new(),
            shots: dir.to_path_buf(),
            crops: PathBuf::new(),
            python: "python3".into(),
            itemdb: Arc::new(crate::itemdb::ItemDb::new(vec![])),
            http: reqwest::Client::new(),
            site: String::new(),
            token: String::new(),
            log: Arc::new(Mutex::new(Log::default())),
        }
    }

    fn shot_at(dir: &Path, name: &str, at: i64) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, b"not really a picture").unwrap();
        let t = timeval(at);
        // `set_file_mtime` is not in std; go through the one syscall that is
        // reachable without another dependency.
        unsafe {
            let c = std::ffi::CString::new(p.to_string_lossy().as_bytes()).unwrap();
            let times = [t, t];
            assert_eq!(utimes(c.as_ptr(), times.as_ptr()), 0, "utimes failed");
        }
        p
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct Timeval {
        sec: i64,
        usec: i64,
    }

    pub(super) fn timeval(at: i64) -> Timeval {
        Timeval { sec: at, usec: 0 }
    }

    unsafe extern "C" {
        #[link_name = "utimes"]
        pub(super) fn utimes(path: *const std::ffi::c_char, times: *const Timeval) -> i32;
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("eqlparserd-shots-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_picture_written_in_that_second_is_the_one() {
        let d = tmp("exact");
        let want = shot_at(&d, "EQ004129.jpg", 1_772_000_000);
        shot_at(&d, "EQ004128.jpg", 1_772_000_000 - 60);
        assert_eq!(looking_at(&d).file_at(1_772_000_000), Some(want));
    }

    #[test]
    fn the_previous_picture_does_not_answer_for_this_one() {
        // The failure this guards is not "no window shown". It is a person
        // being shown last shot's picture beside this shot's reading, and
        // agreeing with it.
        let d = tmp("stale");
        shot_at(&d, "EQ004128.jpg", 1_772_000_000 - 30);
        assert_eq!(looking_at(&d).file_at(1_772_000_000), None);
    }

    #[test]
    fn a_picture_a_moment_late_is_still_this_shot() {
        // The file lands in the same second in practice, but a busy disk is
        // allowed to be a few behind. Late is fine; early is not.
        let d = tmp("late");
        let want = shot_at(&d, "EQ004129.jpg", 1_772_000_004);
        assert_eq!(looking_at(&d).file_at(1_772_000_000), Some(want));
    }

    #[test]
    fn the_closest_of_two_in_the_same_window_wins() {
        let d = tmp("two");
        let want = shot_at(&d, "EQ004129.jpg", 1_772_000_001);
        shot_at(&d, "EQ004130.jpg", 1_772_000_006);
        assert_eq!(looking_at(&d).file_at(1_772_000_000), Some(want));
    }

    #[test]
    fn a_file_that_is_not_a_picture_is_not_a_screenshot() {
        // The client's folder holds an index.json and the archiver's
        // subdirectories. Neither is the shot just taken.
        let d = tmp("junk");
        shot_at(&d, "index.json", 1_772_000_000);
        std::fs::create_dir_all(d.join("processed")).unwrap();
        assert_eq!(looking_at(&d).file_at(1_772_000_000), None);
    }

    #[test]
    fn a_window_the_reader_threw_on_is_told_from_a_window_with_no_item() {
        // Two different things that used to print the same line: "this
        // screenshot holds no item window", which is ordinary, and "the
        // reader crashed on it", which somebody should see.
        let thrown: Window = serde_json::from_str(
            r#"{"crop":"a.png","crop_path":"/tmp/a.png","needs_review":["__error__"],
                "error":"boom","record":{}}"#,
        )
        .unwrap();
        assert!(thrown.unread());
        let empty: Window =
            serde_json::from_str(r#"{"crop":"b.png","crop_path":"/tmp/b.png","record":{}}"#)
                .unwrap();
        assert!(!empty.unread());
        assert!(empty.item().is_empty());
    }

    #[test]
    fn the_readers_answer_parses_as_the_reader_writes_it() {
        // Pinned against `ocr/oneshot.py`'s actual output for a real window,
        // so a change to its shape fails here rather than at 3am in a
        // deserialise error nobody sees.
        let raw = r#"{"shots":[{"shot":"eq-20260904-141833.jpg","sha":"b4b4","score":8.44,
            "windows":[{"crop":"live_eq_20260904_141833_b4b42231_0.png",
              "crop_path":"/tmp/live_eq_20260904_141833_b4b42231_0.png",
              "box":[1066,250,640,477],"held":null,
              "record":{"item":"Maid's Stockings","slot":"LEGS"},
              "fields":{"item":{"ok":true}},"needs_review":[],
              "lines":[{"y":12,"text":"Maid's Stockings"}]}]}]}"#;
        let r: Read = serde_json::from_str(raw).unwrap();
        let w = &r.shots[0].windows[0];
        assert_eq!(w.item(), "Maid's Stockings");
        assert!(w.held.is_none());
        assert!(!w.unread());
    }
}

#[cfg(test)]
mod prune_tests {
    use super::*;

    #[test]
    fn the_oldest_cut_windows_are_the_ones_dropped() {
        let d = std::env::temp_dir().join(format!("eqlparserd-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for i in 0..(KEEP_CROPS + 5) {
            let p = d.join(format!("crop{i:04}.png"));
            std::fs::write(&p, b"x").unwrap();
            // Ordered by mtime, not by name: the pruner sorts on the former
            // and a test that agreed with it by accident would prove nothing.
            let t = super::tests::timeval(1_700_000_000 + i as i64);
            unsafe {
                let c = std::ffi::CString::new(p.to_string_lossy().as_bytes()).unwrap();
                let times = [t, t];
                super::tests::utimes(c.as_ptr(), times.as_ptr());
            }
        }
        let mut r = super::tests::looking_at(&d);
        r.crops = d.clone();
        r.prune();
        let left: Vec<String> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(left.len(), KEEP_CROPS);
        assert!(!left.contains(&"crop0000.png".to_string()), "the oldest survived");
        assert!(
            left.contains(&format!("crop{:04}.png", KEEP_CROPS + 4)),
            "the newest was dropped"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod token_file_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("eqlparserd-tok-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn a_written_token_is_private_from_the_moment_it_exists() {
        let d = tmp("fresh");
        let p = d.join("eqldb-token");
        write_privately(&p, "abc123").unwrap();
        assert_eq!(mode(&p), 0o600, "a token any account can read is not a secret");
        assert_eq!(std::fs::read_to_string(&p).unwrap().trim(), "abc123");
    }

    #[test]
    fn overwriting_a_readable_token_tightens_it() {
        // `create(true)` keeps the mode of a file that already exists, so
        // without the explicit tighten a token pasted over a world-readable
        // one stays world-readable -- and `configure` would then refuse to
        // use the thing somebody just set, blaming the file they did not
        // make.
        let d = tmp("loose");
        let p = d.join("eqldb-token");
        std::fs::write(&p, "old").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_privately(&p, "new").unwrap();
        assert_eq!(mode(&p), 0o600);
        assert_eq!(std::fs::read_to_string(&p).unwrap().trim(), "new");
    }

    #[test]
    fn a_shorter_token_does_not_leave_the_tail_of_a_longer_one() {
        // Without `truncate`, writing a 8-character token over a 64-character
        // one leaves 56 characters of the old one behind and the file reads
        // as neither.
        let d = tmp("trunc");
        let p = d.join("eqldb-token");
        write_privately(&p, &"a".repeat(64)).unwrap();
        write_privately(&p, "short").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap().trim(), "short");
    }

    #[test]
    fn a_world_readable_token_is_refused_rather_than_used() {
        let d = tmp("refuse");
        let p = d.join("eqldb-token");
        std::fs::write(&p, "sekrit").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        // Read through the same door `configure` uses.
        let before = std::env::var("EQLDB_TOKEN").ok();
        unsafe { std::env::remove_var("EQLDB_TOKEN") };
        let got = token_from(Some(&d));
        if let Some(v) = before {
            unsafe { std::env::set_var("EQLDB_TOKEN", v) };
        }
        assert!(got.is_none(), "a token other accounts can read was used");
    }

    #[test]
    fn a_token_at_the_right_mode_is_read() {
        let d = tmp("ok");
        let p = d.join("eqldb-token");
        write_privately(&p, "  spaced  ").unwrap();
        let before = std::env::var("EQLDB_TOKEN").ok();
        unsafe { std::env::remove_var("EQLDB_TOKEN") };
        let got = token_from(Some(&d));
        if let Some(v) = before {
            unsafe { std::env::set_var("EQLDB_TOKEN", v) };
        }
        // Trimmed: a token pasted out of a web page brings whitespace with it,
        // and a bearer header with a trailing newline is a 401 nobody can see.
        assert_eq!(got.as_deref(), Some("spaced"));
    }
}

/// The Lore tab exception: a window carrying lore eqdb does not have is sent
/// even though the item itself is covered.
///
/// These test the two halves of that decision separately, because between
/// them they are the whole of it and neither needs a site: `lore_on_file`
/// reads an `/api/item` payload, and `Window::is_lore` reads a record.
#[cfg(test)]
mod lore_tests {
    use super::*;
    use serde_json::json;

    fn window(record: serde_json::Value) -> Window {
        Window {
            crop: "c.png".into(),
            crop_path: "/nowhere/c.png".into(),
            held: None,
            error: None,
            record,
            fields: serde_json::Value::Null,
            needs_review: vec![],
            lines: serde_json::Value::Null,
        }
    }

    /// The 1,704-item case, and the reason any of this was written: the site
    /// holds the item, holds its stats, and holds no lore. Measured on
    /// production 5 September -- 1,865 items with facts, 161 with lore.
    #[test]
    fn facts_without_lore_are_not_lore_on_file() {
        let v = json!({"count": 1, "matches": [
            {"facts": [{"item": "Bixie Wing", "lore": "", "icon": 734}]}
        ]});
        assert!(!lore_on_file(&v));
    }

    #[test]
    fn lore_on_any_reading_is_lore_on_file() {
        let v = json!({"count": 1, "matches": [
            {"facts": [{"lore": ""}, {"lore": "Misty Bixie Wings"}]}
        ]});
        assert!(lore_on_file(&v));
    }

    /// Whitespace is not lore. The decoder returns a stripped string, but a
    /// hand-typed field on the review page has whatever the person left in it.
    #[test]
    fn blank_lore_is_not_lore() {
        let v = json!({"count": 1, "matches": [{"facts": [{"lore": "   \n "}]}]});
        assert!(!lore_on_file(&v));
    }

    /// Twins. `Bixie Stinger` names two items and lore on one of them is not
    /// lore on the other, so the window goes to a person to sort out.
    #[test]
    fn one_twin_with_lore_is_not_enough() {
        let v = json!({"count": 2, "matches": [
            {"facts": [{"lore": "From the Sky"}]},
            {"facts": [{"lore": ""}]},
        ]});
        assert!(!lore_on_file(&v));
    }

    /// A match eqdb has never had a reading for cannot be holding its lore.
    #[test]
    fn a_match_with_no_facts_is_not_lore_on_file() {
        assert!(!lore_on_file(&json!({"count": 1, "matches": [{"facts": []}]})));
        assert!(!lore_on_file(&json!({"count": 1, "matches": [{}]})));
    }

    /// `count: 0` -- the shape the site answers with for a name it does not
    /// know, and the shape a caller gets from an origin that is up but empty.
    #[test]
    fn an_unknown_item_is_not_lore_on_file() {
        assert!(!lore_on_file(&json!({"count": 0, "matches": []})));
        assert!(!lore_on_file(&json!({})));
        assert!(!lore_on_file(&serde_json::Value::Null));
    }

    /// The tab, not the text. This is the window `parse.py` flags with "the
    /// Lore tab is selected and no lore text was read" -- the one most worth
    /// a person's eyes, and the one a text-only test would have dropped.
    #[test]
    fn a_lore_tab_that_did_not_decode_is_still_a_lore_window() {
        let w = window(json!({"item": "Ancient Rock", "tab": "Lore"}));
        assert!(w.lore().is_empty());
        assert!(w.is_lore());
    }

    #[test]
    fn a_description_window_is_not_a_lore_window() {
        let w = window(json!({
            "item": "Ancient Rock", "tab": "Description",
            "note": "Ancient piece of earth from Rathe Mountains"
        }));
        assert!(!w.is_lore(), "a note on the Description tab is not lore");
    }

    /// No tab at all is the common shape: most windows never reach the tab
    /// row, and none of them may take this exception.
    #[test]
    fn a_window_with_no_tab_is_not_a_lore_window() {
        assert!(!window(json!({"item": "Ancient Rock"})).is_lore());
    }

    #[test]
    fn lore_text_is_read_and_trimmed() {
        let w = window(json!({"tab": "Lore", "lore": "  Marnek's Wand \n"}));
        assert_eq!(w.lore(), "Marnek's Wand");
        assert!(w.is_lore());
    }

    /// An unreachable site holds no lore, so its windows are sent. `ItemDb`
    /// with no origins cannot answer, which is the same answer.
    #[test]
    fn a_site_that_cannot_be_asked_does_not_hold_lore() {
        let d = std::env::temp_dir();
        let r = tests::looking_at(&d);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(!rt.block_on(r.lore_held("Ancient Rock")));
    }
}

/// The send rule as a table, which is what it is.
#[cfg(test)]
mod send_rule_tests {
    use super::sends;

    /// The line this whole change turns on. Before it, an item eqdb held and
    /// was not asking for was skipped whatever the window carried -- and
    /// since the wanted list never mentions lore, that was every Lore tab of
    /// every item already photographed. 143 of the 146 items whose lore is on
    /// file were in exactly this state when it was taken.
    #[test]
    fn a_covered_item_is_sent_when_the_window_carries_lore_eqdb_lacks() {
        assert!(sends(false, true, true));
    }

    /// And still skipped when it does not. The exception is for the gap, not
    /// for the tab: re-photographing lore already on file is a duplicate in
    /// somebody's review queue.
    #[test]
    fn a_covered_item_is_still_skipped_otherwise() {
        assert!(!sends(false, true, false));
    }

    #[test]
    fn a_wanted_item_is_always_sent() {
        assert!(sends(true, true, false));
        assert!(sends(true, true, true));
    }

    #[test]
    fn an_unknown_item_is_always_sent() {
        assert!(sends(false, false, false));
        assert!(sends(false, false, true));
    }

    /// Every combination, so the rule is pinned rather than sampled. Skipping
    /// is the single case: known, unwanted, teaching nothing.
    #[test]
    fn skipping_is_the_one_case() {
        let mut skipped = vec![];
        for wanted in [false, true] {
            for known in [false, true] {
                for lore in [false, true] {
                    if !sends(wanted, known, lore) {
                        skipped.push((wanted, known, lore));
                    }
                }
            }
        }
        assert_eq!(skipped, vec![(false, true, false)]);
    }
}
