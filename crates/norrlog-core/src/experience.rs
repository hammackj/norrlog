//! Experience tracking and level-progression analysis.
//!
//! EverQuest reports experience as a percentage of the *current* level
//! ("You gain experience! (1.585%)"), never as an absolute figure. That turns
//! out to be enough: percentages accumulated between two level-up messages sum
//! to ~100% (measured across 47 levels of real log: 99–102%), so the reported
//! value is directly interpretable as progress through the level you are in.
//!
//! Because the unit is per-level, absolute figures are **not comparable across
//! levels** — 1% at level 50 is far more raw experience than 1% at level 10.
//! Everything here therefore reports in "% of a level" and its derivatives
//! (levels/hour, time to next level), which stay meaningful at any level.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// How far apart an experience message and its kill may be and still be
/// considered the same event.
///
/// **The experience message comes first.** Measured on a real 595k-line log:
/// the gain precedes "You have slain X!" 2,179 times and follows it 4 times.
/// Attribution therefore buffers unclaimed experience and attaches it to the
/// *next* kill, while still handling the rare reverse order. Getting this
/// backwards leaves almost every kill uncredited and makes experience-per-kill
/// meaningless.
const KILL_ATTRIBUTION_WINDOW: i64 = 5;

/// Window used for the "current rate" figure.
const RATE_WINDOW_SECONDS: i64 = 30 * 60;

/// A base, undoubled alternate advancement award.
///
/// Normal gain is one point; an experience potion doubles it to two. The award
/// size is the only potion signal the client gives, since it logs no duration
/// and no expiry message.
pub const AA_BASE_AWARD: u32 = 1;

/// Doubled awards granted by one experience potion.
///
/// Experience potions are **charge-based, not timed**: each grants five doubled
/// awards, and charges stack if another is drunk before the first is spent.
///
/// Verified against a real 595k-line log: seeding the pool with the four
/// charges left over from a potion drunk before logging began, all 79 doubled
/// awards consumed a charge, no doubled award occurred without one, no normal
/// award occurred while charges remained, and the pool finished at exactly
/// zero. Peak holding was nine, which is what confirms stacking.
pub const AA_POTION_CHARGES: u32 = 5;

/// Recognise an experience potion by spell name.
///
/// Matched on substring rather than an exact list so ranks and variants are
/// picked up without needing the table updated. The client names the item
/// "Bottle of Alternate Adventure".
pub fn is_aa_potion(spell_name: &str) -> bool {
    spell_name.to_lowercase().contains("alternate adventure")
}

/// One completed (or in-progress) level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelRecord {
    /// The level being progressed *through*. `None` for the stretch before the
    /// first level-up, where the starting level is unknowable from the log.
    pub level: Option<u32>,
    pub started: i64,
    /// When the level-up happened; `None` while still in progress.
    pub ended: Option<i64>,
    /// Percentage points accumulated. A full level is ~100.
    pub percent: f64,
    /// Number of experience messages seen.
    pub events: u32,
    /// Kills that yielded experience during this level.
    pub kills: u32,
    /// Seconds spent, wall clock.
    pub seconds: i64,
    /// Whether the record covers a whole level. The first and last records are
    /// partial, and reporting their rates as if complete would mislead.
    pub complete: bool,
    /// Where the experience for this level came from, most productive first.
    ///
    /// Percentage points per zone. Tracked per level rather than only in
    /// aggregate so a level can be read back as "this is where I ground it",
    /// which is the question when comparing one level to the next.
    #[serde(default)]
    pub zones: Vec<ZoneShare>,
    /// The class trio in effect for this level, e.g. "WAR/DRU/BRD".
    ///
    /// Filled in by the caller, which owns the class detector. Empty when the
    /// build could not be determined.
    #[serde(default)]
    pub build: String,
    /// Of `percent`, how much the client marked `(with a bonus)`, and how many
    /// of `events` carried the mark.
    ///
    /// Held apart rather than folded in, and *not* subtracted from `percent`:
    /// a bonused percentage is still a real percentage of a real level, so the
    /// sums that make this module work are unaffected. What it is not is
    /// comparable -- levels/hour measured through a bonus event is not a rate
    /// anybody can plan by, and before this the question could not even be
    /// asked, because the lines stating it did not parse.
    ///
    /// The multiplier is deliberately not inferred. The client states that a
    /// bonus applied and never states its size, and a figure derived by
    /// comparing this level to the last would be reading a guess back as a
    /// measurement.
    #[serde(default)]
    pub bonus_percent: f64,
    #[serde(default)]
    pub bonus_events: u32,
}

/// Experience earned in one zone during one level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneShare {
    pub zone: String,
    pub percent: f64,
}

impl LevelRecord {
    /// Experience per kill, in percentage points.
    pub fn percent_per_kill(&self) -> f64 {
        if self.kills == 0 {
            0.0
        } else {
            self.percent / self.kills as f64
        }
    }

    /// Percentage points per hour.
    pub fn percent_per_hour(&self) -> f64 {
        if self.seconds <= 0 {
            0.0
        } else {
            self.percent * 3600.0 / self.seconds as f64
        }
    }
}

/// Experience earned in one zone or from one mob type.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExpSource {
    pub name: String,
    pub percent: f64,
    pub kills: u32,
    /// Seconds during which experience was being earned here. Only meaningful
    /// for zones, where time spent is well defined.
    pub seconds: i64,
    /// Alternate advancement points earned here.
    pub aa: u32,
}

impl ExpSource {
    pub fn percent_per_kill(&self) -> f64 {
        if self.kills == 0 {
            0.0
        } else {
            self.percent / self.kills as f64
        }
    }

    pub fn percent_per_hour(&self) -> f64 {
        if self.seconds <= 0 {
            0.0
        } else {
            self.percent * 3600.0 / self.seconds as f64
        }
    }

    /// Alternate advancement points per hour.
    pub fn aa_per_hour(&self) -> f64 {
        if self.seconds <= 0 {
            0.0
        } else {
            self.aa as f64 * 3600.0 / self.seconds as f64
        }
    }
}

/// One alternate advancement purchase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AaPurchase {
    /// The ability as the player knows it. For a toggle this is the base name,
    /// with the `: Enabled` / `: Disabled` halves already folded together.
    pub ability: String,
    /// Highest rank reached, as the player counts them.
    pub rank: u32,
    /// Highest rank reached as the *client* counts them, which for a toggle is
    /// twice the real rank. Kept because it is what the log literally says, and
    /// because the `/alt list` ladder is numbered this way.
    #[serde(default)]
    pub client_rank: u32,
    /// Whether this ability is a toggle, and if so whether the last rank seen
    /// left it switched on. `None` for an ordinary ability.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Total points sunk into this ability across every rank seen.
    pub spent: u32,
    pub last_seen: i64,
    /// Ranks below the one held whose purchase never appeared in the log.
    ///
    /// Holding rank 4 means ranks 1 to 3 were bought, whether or not this log
    /// saw it happen. Each one here is a purchase that certainly occurred and
    /// certainly is not counted in `spent`.
    #[serde(default)]
    pub unlogged_ranks: Vec<u32>,
    /// The most those unlogged ranks can have cost.
    ///
    /// Rank costs within an ability never decrease, so a missing rank cost no
    /// more than the cheapest rank seen above it. That bounds the gap without
    /// inventing a figure — the exact cost is not recoverable, only stated.
    #[serde(default)]
    pub unlogged_cost_max: u32,
}

/// The most recent kill and whether it has already been counted.
///
/// `credited` is tracked explicitly rather than inferred from whether the mob
/// appears in the tally: on the second kill of a mob type the entry already
/// exists, so presence would wrongly imply the kill was counted and the count
/// would stall at one.
#[derive(Debug, Clone)]
struct LastKill {
    at: i64,
    mob: String,
    credited: bool,
}

/// Accumulates experience events and derives progression statistics.
pub struct ExpTracker {
    current_level: Option<u32>,
    completed: Vec<LevelRecord>,
    current: LevelRecord,
    by_zone: HashMap<String, ExpSource>,
    /// Experience by zone within the level currently in progress.
    level_zones: HashMap<String, f64>,
    by_mob: HashMap<String, ExpSource>,
    zone: Option<String>,
    /// Start of the current zone visit, for time-in-zone accounting.
    zone_since: Option<i64>,
    /// Most recent kill, used for the rare case of experience arriving after
    /// the kill message.
    last_kill: Option<LastKill>,
    /// Experience seen but not yet attached to a kill. The usual case: the
    /// gain is logged just before the death message.
    pending: Vec<(i64, f64)>,
    /// Kills that yielded experience.
    kills_with_exp: u32,
    /// Kills the logging character made in total, credited or not.
    kills_total: u32,
    /// Rolling (timestamp, percent) for the current-rate figure.
    recent: Vec<(i64, f64)>,
    /// Kills that produced no experience message, e.g. grey mobs.
    kills_without_exp: u32,
    /// Alternate advancement points earned in total.
    aa_earned: u32,
    /// Unspent balance, as last reported by the client.
    aa_unspent: u32,
    /// (timestamp, points) for rate calculation.
    aa_events: Vec<(i64, u32)>,
    /// Purchases, keyed by lowercased ability name.
    aa_spent: HashMap<String, AaPurchase>,
    /// Rank to cost, per ability, for every purchase the log actually showed.
    aa_ranks_seen: HashMap<String, std::collections::BTreeMap<u32, u32>>,
    /// Experience potions consumed.
    aa_potions_used: u32,
    /// When the last potion took effect.
    last_potion_at: Option<i64>,
    /// Awards larger than the base amount, i.e. doubled by a potion.
    aa_gains_doubled: u32,
    /// Awards at the base amount.
    aa_gains_normal: u32,
    /// Whether the most recent award was doubled.
    last_gain_doubled: bool,
    /// Remaining doubled awards across all potions drunk. Charges stack.
    aa_potion_charges: u32,
    /// Doubled awards seen with no known charge, meaning a potion was already
    /// running when the log began.
    aa_doubled_unaccounted: u32,
    /// Whether `current.started` holds a real timestamp yet.
    ///
    /// An explicit flag rather than treating 0 as "unset": a zero timestamp is
    /// a legitimate value, and using it as a sentinel silently moved the start
    /// of the level to the *second* event.
    current_started: bool,
    /// The current record began at a level change the log did not ding for
    /// (a loadout switch), so where in the level it began is unknown and it
    /// is never a whole level, even if it ends with a level-up.
    current_from_switch: bool,
}

impl Default for ExpTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ExpTracker {
    pub fn new() -> ExpTracker {
        ExpTracker {
            current_level: None,
            completed: Vec::new(),
            current: LevelRecord {
                level: None,
                started: 0,
                ended: None,
                percent: 0.0,
                events: 0,
                kills: 0,
                seconds: 0,
                complete: false,
                zones: Vec::new(),
                build: String::new(),
                bonus_percent: 0.0,
                bonus_events: 0,
            },
            by_zone: HashMap::new(),
            level_zones: HashMap::new(),
            by_mob: HashMap::new(),
            zone: None,
            zone_since: None,
            last_kill: None,
            pending: Vec::new(),
            kills_with_exp: 0,
            kills_total: 0,
            recent: Vec::new(),
            kills_without_exp: 0,
            aa_earned: 0,
            aa_unspent: 0,
            aa_events: Vec::new(),
            aa_spent: HashMap::new(),
            aa_ranks_seen: HashMap::new(),
            aa_potions_used: 0,
            last_potion_at: None,
            aa_gains_doubled: 0,
            aa_gains_normal: 0,
            last_gain_doubled: false,
            aa_potion_charges: 0,
            aa_doubled_unaccounted: 0,
            current_started: false,
            current_from_switch: false,
        }
    }

    pub fn current_level(&self) -> Option<u32> {
        self.current_level
    }

    /// Progress through the current level, in percentage points.
    pub fn current_percent(&self) -> f64 {
        self.current.percent
    }

    pub fn completed_levels(&self) -> &[LevelRecord] {
        &self.completed
    }

    pub fn current_record(&self) -> &LevelRecord {
        &self.current
    }

    /// Zone breakdown for the level in progress, most productive first.
    fn zone_shares(&self) -> Vec<ZoneShare> {
        let mut v: Vec<ZoneShare> = self
            .level_zones
            .iter()
            .map(|(z, p)| ZoneShare {
                zone: z.clone(),
                percent: *p,
            })
            .collect();
        v.sort_by(|a, b| {
            b.percent
                .partial_cmp(&a.percent)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    }

    /// Note a zone change so time-in-zone can be attributed.
    pub fn on_zone(&mut self, timestamp: i64, zone: &str) {
        self.flush_zone_time(timestamp);
        self.zone = Some(zone.to_string());
        self.zone_since = Some(timestamp);
    }

    /// Credit elapsed time in the current zone up to `now`.
    ///
    /// Called on every timestamped event, not only when the zone changes. A
    /// zone that is settled up only on the way out accrues no time at all
    /// while it is being camped, which is precisely when its rate is worth
    /// watching: the best-zones ranking would sit unchanged for a whole
    /// evening and then move all at once on the zone line.
    fn flush_zone_time(&mut self, now: i64) {
        if let (Some(z), Some(since)) = (self.zone.clone(), self.zone_since) {
            if now > since {
                self.by_zone
                    .entry(z.clone())
                    .or_insert_with(|| ExpSource {
                        name: z,
                        ..Default::default()
                    })
                    .seconds += now - since;
            }
        }
        // Never rewind. Log lines can share a second and occasionally arrive
        // out of order; moving the mark backwards would credit the span it
        // covers a second time.
        self.zone_since = Some(match self.zone_since {
            Some(since) => since.max(now),
            None => now,
        });
    }

    /// Note a kill by the logging character.
    ///
    /// Claims any experience logged in the moments just before, which is the
    /// normal ordering.
    pub fn on_kill(&mut self, timestamp: i64, mob: &str) {
        self.flush_zone_time(timestamp);
        self.kills_total += 1;

        let cutoff = timestamp - KILL_ATTRIBUTION_WINDOW;
        let claimed: f64 = self
            .pending
            .iter()
            .filter(|(t, _)| *t >= cutoff)
            .map(|(_, p)| p)
            .sum();
        let had_any = self.pending.iter().any(|(t, _)| *t >= cutoff);
        self.pending.clear();

        if had_any {
            self.credit_kill(mob, claimed);
        } else {
            self.kills_without_exp += 1;
        }
        // Retained so experience arriving *after* the kill can still attach.
        self.last_kill = Some(LastKill {
            at: timestamp,
            mob: mob.to_string(),
            credited: had_any,
        });
    }

    /// Record a kill and the experience it produced against mob and zone.
    fn credit_kill(&mut self, mob: &str, percent: f64) {
        self.kills_with_exp += 1;
        self.current.kills += 1;
        if let Some(z) = self.zone.clone() {
            self.by_zone
                .entry(z.clone())
                .or_insert_with(|| ExpSource {
                    name: z,
                    ..Default::default()
                })
                .kills += 1;
        }
        let e = self
            .by_mob
            .entry(mob.to_lowercase())
            .or_insert_with(|| ExpSource {
                name: mob.to_string(),
                ..Default::default()
            });
        e.percent += percent;
        e.kills += 1;
    }

    /// Note alternate advancement points earned.
    ///
    /// `unspent` is the client's running balance, which falls when points are
    /// spent, so lifetime earned is accumulated separately from the per-event
    /// amounts rather than read off that figure.
    pub fn on_ability_points(&mut self, timestamp: i64, points: u32, unspent: u32) {
        self.flush_zone_time(timestamp);
        // Award size is the only potion signal the client gives.
        self.last_gain_doubled = points > AA_BASE_AWARD;
        if self.last_gain_doubled {
            self.aa_gains_doubled += 1;
            if self.aa_potion_charges > 0 {
                self.aa_potion_charges -= 1;
            } else {
                // A potion was already running before we started reading.
                self.aa_doubled_unaccounted += 1;
            }
        } else {
            self.aa_gains_normal += 1;
            // A normal award proves nothing is left, whatever we thought.
            self.aa_potion_charges = 0;
        }
        self.aa_earned += points;
        self.aa_unspent = unspent;
        self.aa_events.push((timestamp, points));
        if let Some(z) = self.zone.clone() {
            self.by_zone
                .entry(z.clone())
                .or_insert_with(|| ExpSource {
                    name: z,
                    ..Default::default()
                })
                .aa += points;
        }
    }

    /// Note points spent on an alternate advancement.
    pub fn on_ability_spent(&mut self, timestamp: i64, ability: &str, rank: u32, cost: u32) {
        // A toggle arrives as two abilities sharing one ladder, so fold it back
        // into the one thing the player bought. Left apart, Symphonic Aura
        // reads as ": Disabled" at rank 9 for 12 points beside ": Enabled" at
        // rank 10 for nothing — two entries, neither of them a rank the player
        // has, and the 12 points attached to the half that sounds switched off.
        let (name, toggled_on) = match crate::aa::toggle_parts(ability) {
            Some((base, enabled)) => (base.to_string(), Some(enabled)),
            None => (ability.to_string(), None),
        };
        let key = name.to_lowercase();
        let e = self
            .aa_spent
            .entry(key.clone())
            .or_insert_with(|| AaPurchase {
                ability: name.clone(),
                rank: 0,
                client_rank: 0,
                enabled: None,
                spent: 0,
                last_seen: timestamp,
                unlogged_ranks: Vec::new(),
                unlogged_cost_max: 0,
            });
        self.aa_ranks_seen
            .entry(key)
            .or_default()
            .insert(rank, cost);
        // The state is whichever half owns the highest rank seen: the last
        // rank bought is the one you are sitting on.
        if toggled_on.is_some() && rank >= e.client_rank {
            e.enabled = toggled_on;
        }
        e.client_rank = e.client_rank.max(rank);
        e.rank = crate::aa::real_rank(e.client_rank, toggled_on.is_some() || e.enabled.is_some());
        e.spent += cost;
        e.last_seen = timestamp;
        // Keep the balance current. The client only restates it on a *gain*,
        // so without this any purchase after the last gain leaves the displayed
        // balance stale and higher than reality.
        self.aa_unspent = self.aa_unspent.saturating_sub(cost);
    }

    /// Note that an experience potion took effect.
    pub fn on_aa_potion(&mut self, timestamp: i64) {
        self.aa_potions_used += 1;
        self.last_potion_at = Some(timestamp);
        self.aa_potion_charges += AA_POTION_CHARGES;
    }

    /// Doubled awards still available from potions drunk.
    pub fn aa_potion_charges(&self) -> u32 {
        self.aa_potion_charges
    }

    /// Doubled awards seen with no charge on the books, i.e. from a potion
    /// that was already active when the log began.
    pub fn aa_doubled_unaccounted(&self) -> u32 {
        self.aa_doubled_unaccounted
    }

    pub fn aa_potions_used(&self) -> u32 {
        self.aa_potions_used
    }

    pub fn last_potion_at(&self) -> Option<i64> {
        self.last_potion_at
    }

    pub fn aa_gains_doubled(&self) -> u32 {
        self.aa_gains_doubled
    }

    pub fn aa_gains_normal(&self) -> u32 {
        self.aa_gains_normal
    }

    /// Share of awards that were doubled, 0.0–1.0.
    pub fn aa_doubled_share(&self) -> f64 {
        let total = self.aa_gains_doubled + self.aa_gains_normal;
        if total == 0 {
            0.0
        } else {
            self.aa_gains_doubled as f64 / total as f64
        }
    }

    /// Average points per award — the realised benefit of potion coverage.
    ///
    /// 1.0 means no potion was ever up; 2.0 means every award was doubled.
    pub fn aa_effective_multiplier(&self) -> f64 {
        let events = self.aa_events.len();
        if events == 0 {
            0.0
        } else {
            self.aa_earned as f64 / events as f64
        }
    }

    /// Whether a potion is currently working.
    ///
    /// Charges are authoritative once any potion has been seen: spending the
    /// last one leaves the most recent award doubled *and* the potion finished,
    /// so the last-award hint must not override a zero balance. That hint only
    /// applies when no potion has ever been observed, i.e. the log began
    /// part-way through one.
    pub fn aa_potion_likely_active(&self) -> bool {
        if self.aa_potion_charges > 0 {
            return true;
        }
        self.aa_potions_used == 0 && self.last_gain_doubled
    }

    pub fn aa_earned(&self) -> u32 {
        self.aa_earned
    }

    pub fn aa_unspent(&self) -> u32 {
        self.aa_unspent
    }

    /// Total points spent, across every purchase seen in the log.
    ///
    /// Ranks bought before logging began are invisible to us, so this is a
    /// floor on the character's lifetime spend rather than the figure the game
    /// shows. On this character the two differ by a constant 2, which is
    /// exactly what that gap means: two ranks predate the log.
    pub fn aa_spent_total(&self) -> u32 {
        self.aa_spent.values().map(|p| p.spent).sum()
    }

    /// Ranks held whose purchase this log never recorded, and the most they
    /// can have cost. The lower bound is zero: some ranks genuinely are free.
    pub fn aa_unlogged(&self) -> (Vec<AaPurchase>, u32) {
        let mut v: Vec<AaPurchase> = self
            .aa_purchases()
            .into_iter()
            .filter(|p| !p.unlogged_ranks.is_empty())
            .collect();
        v.sort_by(|a, b| b.unlogged_cost_max.cmp(&a.unlogged_cost_max));
        let bound = v.iter().map(|p| p.unlogged_cost_max).sum();
        (v, bound)
    }

    /// Ranks held but never seen bought, and what they can have cost at most.
    ///
    /// `held` is the *client* rank, because that is what `aa_ranks_seen` is
    /// keyed by. A toggle's two halves are folded into one entry before they
    /// get here, so its ladder is continuous — asking this in real ranks would
    /// compare 5 against a map holding 1 to 10 and call half of it missing.
    fn unlogged_for(&self, key: &str, held: u32) -> (Vec<u32>, u32) {
        let Some(seen) = self.aa_ranks_seen.get(key) else {
            return (Vec::new(), 0);
        };
        let missing: Vec<u32> = (1..=held).filter(|r| !seen.contains_key(r)).collect();
        // Costs never fall as rank rises, so the cheapest rank seen above a
        // missing one is a ceiling on it.
        let bound = missing
            .iter()
            .map(|m| {
                seen.range((m + 1)..)
                    .map(|(_, c)| *c)
                    .min()
                    .unwrap_or_default()
            })
            .sum();
        (missing, bound)
    }

    /// Points spent, highest first.
    pub fn aa_purchases(&self) -> Vec<AaPurchase> {
        let mut v: Vec<AaPurchase> = self
            .aa_spent
            .iter()
            .map(|(key, p)| {
                let (unlogged_ranks, unlogged_cost_max) = self.unlogged_for(key, p.client_rank);
                AaPurchase {
                    unlogged_ranks,
                    unlogged_cost_max,
                    ..p.clone()
                }
            })
            .collect();
        v.sort_by(|a, b| b.spent.cmp(&a.spent).then(b.last_seen.cmp(&a.last_seen)));
        v
    }

    /// Alternate advancement points per hour over the given window.
    ///
    /// A longer default window than experience: points arrive as whole units
    /// and far less often, so a 30-minute view is usually just zero.
    pub fn aa_per_hour(&self, now: i64, window_seconds: i64) -> f64 {
        let cutoff = now - window_seconds;
        let recent: Vec<&(i64, u32)> = self
            .aa_events
            .iter()
            .filter(|(t, _)| *t >= cutoff)
            .collect();
        if recent.is_empty() {
            return 0.0;
        }
        let total: u32 = recent.iter().map(|(_, p)| p).sum();
        // Measure from the window start, not the first event, so a quiet
        // stretch correctly drags the rate down.
        let span = window_seconds.max(1);
        total as f64 * 3600.0 / span as f64
    }

    /// Points per hour across the whole span in which points were earned.
    pub fn aa_lifetime_per_hour(&self) -> f64 {
        if self.aa_events.len() < 2 {
            return 0.0;
        }
        let first = self.aa_events.first().unwrap().0;
        let last = self.aa_events.last().unwrap().0;
        let span = (last - first).max(1);
        self.aa_earned as f64 * 3600.0 / span as f64
    }

    /// Zones ranked by alternate advancement points per hour.
    pub fn zones_by_aa_rate(&self, min_seconds: i64) -> Vec<ExpSource> {
        let mut v: Vec<ExpSource> = self
            .by_zone
            .values()
            .filter(|z| z.seconds >= min_seconds && z.aa > 0)
            .cloned()
            .collect();
        v.sort_by(|a, b| {
            b.aa_per_hour()
                .partial_cmp(&a.aa_per_hour())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    }

    /// Total kills seen, whether or not they yielded experience.
    pub fn kills_total(&self) -> u32 {
        self.kills_total
    }

    /// Kills that produced no experience, e.g. grey cons.
    pub fn kills_without_exp(&self) -> u32 {
        self.kills_without_exp
    }

    /// Note an experience gain.
    pub fn on_experience(&mut self, timestamp: i64, percent: f64, bonus: bool) {
        self.flush_zone_time(timestamp);
        if !self.current_started {
            self.current.started = timestamp;
            self.current_started = true;
        }
        self.current.percent += percent;
        self.current.events += 1;
        if bonus {
            self.current.bonus_percent += percent;
            self.current.bonus_events += 1;
        }
        if let Some(z) = self.zone.clone() {
            *self.level_zones.entry(z).or_insert(0.0) += percent;
        }
        self.current.seconds = (timestamp - self.current.started).max(0);

        self.recent.push((timestamp, percent));
        let cutoff = timestamp - RATE_WINDOW_SECONDS;
        self.recent.retain(|(t, _)| *t >= cutoff);

        // Attribute to the zone we are in.
        if let Some(z) = self.zone.clone() {
            let e = self.by_zone.entry(z.clone()).or_insert_with(|| ExpSource {
                name: z,
                ..Default::default()
            });
            e.percent += percent;
        }

        // The rare ordering: the kill was already logged, so attach directly.
        let recent_kill = self
            .last_kill
            .as_ref()
            .filter(|k| timestamp - k.at <= KILL_ATTRIBUTION_WINDOW)
            .cloned();

        match recent_kill {
            Some(k) if k.credited => {
                // Already counted — a solo and a party message can both fire
                // for one kill, so add the amount without counting again.
                if let Some(e) = self.by_mob.get_mut(&k.mob.to_lowercase()) {
                    e.percent += percent;
                }
            }
            Some(k) => {
                // The kill looked unrewarded when it was logged; correct that.
                self.kills_without_exp = self.kills_without_exp.saturating_sub(1);
                self.credit_kill(&k.mob, percent);
                if let Some(l) = self.last_kill.as_mut() {
                    l.credited = true;
                }
            }
            // The usual ordering: hold it until the kill arrives.
            None => {
                self.pending.push((timestamp, percent));
                let cutoff = timestamp - KILL_ATTRIBUTION_WINDOW;
                self.pending.retain(|(t, _)| *t >= cutoff);
            }
        }
    }

    /// Note a level-up. The level just reached closes the previous record.
    /// The game stated your level (`/who`). In Legends the level belongs to
    /// the loadout, so changing classes changes it with no level-up line:
    /// Tharn went from 50 to 10, and the 50's progress carried on into the
    /// 10's, reading 120% of a level. A different level closes the record
    /// as partial and starts another; the same level changes nothing.
    /// You changed class: a Legends loadout, or a live EverQuest persona,
    /// which has its own level. The level now is unknown until the game says
    /// it, so the record starts with none; a `/who` or a level-up fills it.
    pub fn on_class_switch(&mut self, timestamp: i64) {
        self.restart_at(timestamp, None);
        // A level-up after this closes a record that began mid-level.
        self.current_level = None;
    }

    pub fn on_level_stated(&mut self, timestamp: i64, level: u32) {
        // Compared with the record's level, not `current_level`: that one
        // says a level-up has been seen, which is what makes the next record
        // whole, and a statement is not a level-up.
        let Some(was) = self.current.level else {
            self.current.level = Some(level);
            return;
        };
        if was == level {
            return;
        }
        self.restart_at(timestamp, Some(level));
        self.current_level = Some(level);
    }

    /// Close the current record as partial, and start another at `level`.
    fn restart_at(&mut self, timestamp: i64, level: Option<u32>) {
        let was = self.current.level;
        if self.current.events > 0 {
            self.current.ended = Some(timestamp);
            self.current.level = was;
            self.current.complete = false;
            self.current.zones = self.zone_shares();
            self.completed.push(self.current.clone());
        }
        self.current = LevelRecord {
            level,
            started: timestamp,
            ended: None,
            percent: 0.0,
            events: 0,
            kills: 0,
            seconds: 0,
            complete: false,
            zones: Vec::new(),
            build: String::new(),
            bonus_percent: 0.0,
            bonus_events: 0,
        };
        self.level_zones.clear();
        // Where in the level this starts is unknown, so its clock waits for
        // the first gain rather than claiming the switch as a start.
        self.current_started = false;
        self.current_from_switch = true;
    }

    pub fn on_level_up(&mut self, timestamp: i64, level: u32) {
        self.current.ended = Some(timestamp);
        if !self.current_started {
            // A level completed without any experience message of its own
            // (possible for the first, partial record).
            self.current.started = timestamp;
        }
        self.current.seconds = (timestamp - self.current.started).max(0);
        // The record that just closed covers the level below the one reached.
        self.current.level = Some(level.saturating_sub(1));
        // Only a record that both started and ended with a level-up covers a
        // whole level; the first one begins wherever the log happens to start.
        self.current.complete = self.current_level.is_some() && !self.current_from_switch;
        self.current_from_switch = false;
        self.current.zones = self.zone_shares();
        self.completed.push(self.current.clone());

        self.current_level = Some(level);
        self.current = LevelRecord {
            level: Some(level),
            started: timestamp,
            ended: None,
            percent: 0.0,
            events: 0,
            kills: 0,
            seconds: 0,
            complete: false,
            zones: Vec::new(),
            build: String::new(),
            bonus_percent: 0.0,
            bonus_events: 0,
        };
        self.level_zones.clear();
        // The new level's clock starts at the level-up, not at its first
        // experience message.
        self.current_started = true;
    }

    /// Percentage points per hour over the recent window.
    pub fn current_rate_per_hour(&self, now: i64) -> f64 {
        let cutoff = now - RATE_WINDOW_SECONDS;
        let recent: Vec<&(i64, f64)> = self.recent.iter().filter(|(t, _)| *t >= cutoff).collect();
        if recent.len() < 2 {
            return 0.0;
        }
        let first = recent.first().unwrap().0;
        let span = (now - first).max(1);
        let total: f64 = recent.iter().map(|(_, p)| p).sum();
        total * 3600.0 / span as f64
    }

    /// Seconds until the next level at the current rate, if it is progressing.
    pub fn seconds_to_next_level(&self, now: i64) -> Option<i64> {
        let rate = self.current_rate_per_hour(now);
        if rate <= 0.0 {
            return None;
        }
        let remaining = (100.0 - self.current.percent).max(0.0);
        Some((remaining / rate * 3600.0) as i64)
    }

    /// Zones ranked by experience per hour. Only zones with meaningful time are
    /// included, since a brief pass through a zone yields a meaningless rate.
    pub fn zones_by_rate(&self, min_seconds: i64) -> Vec<ExpSource> {
        let mut v: Vec<ExpSource> = self
            .by_zone
            .values()
            .filter(|z| z.seconds >= min_seconds && z.percent > 0.0)
            .cloned()
            .collect();
        v.sort_by(|a, b| {
            b.percent_per_hour()
                .partial_cmp(&a.percent_per_hour())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    }

    /// Mobs ranked by experience per kill, filtered to those killed often
    /// enough for the average to mean anything.
    pub fn mobs_by_exp_per_kill(&self, min_kills: u32) -> Vec<ExpSource> {
        let mut v: Vec<ExpSource> = self
            .by_mob
            .values()
            .filter(|m| m.kills >= min_kills)
            .cloned()
            .collect();
        v.sort_by(|a, b| {
            b.percent_per_kill()
                .partial_cmp(&a.percent_per_kill())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    }

    /// Average time to complete a level, over whole levels only.
    pub fn average_seconds_per_level(&self) -> Option<i64> {
        let whole: Vec<&LevelRecord> = self.completed.iter().filter(|r| r.complete).collect();
        if whole.is_empty() {
            return None;
        }
        let total: i64 = whole.iter().map(|r| r.seconds).sum();
        Some(total / whole.len() as i64)
    }

    /// Average kills to complete a level, over whole levels only.
    pub fn average_kills_per_level(&self) -> Option<f64> {
        let whole: Vec<&LevelRecord> = self.completed.iter().filter(|r| r.complete).collect();
        if whole.is_empty() {
            return None;
        }
        let total: u32 = whole.iter().map(|r| r.kills).sum();
        Some(total as f64 / whole.len() as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A loadout switch in Legends changes the level with no level-up line.
    #[test]
    fn a_stated_level_change_starts_a_new_record() {
        let mut t = ExpTracker::new();
        t.on_level_up(50, 50);
        t.on_experience(100, 60.0, false);
        t.on_level_stated(200, 50);
        assert!(
            (t.current_percent() - 60.0).abs() < 1e-9,
            "same level: no change"
        );
        t.on_level_stated(300, 10);
        assert_eq!(t.current_level(), Some(10));
        assert_eq!(t.current_percent(), 0.0);
        t.on_experience(310, 19.0, false);
        assert!((t.current_percent() - 19.0).abs() < 1e-9, "not 79");
        let last = t.completed_levels().last().unwrap();
        assert_eq!(last.level, Some(50));
        assert!(!last.complete, "a level left, not finished");
        t.on_level_up(400, 11);
        assert!(
            !t.completed_levels().last().unwrap().complete,
            "began mid-level at the switch, so not a whole level"
        );
        t.on_experience(410, 1.0, false);
        t.on_level_up(500, 12);
        assert!(t.completed_levels().last().unwrap().complete, "this one is");
    }

    /// A live EverQuest persona swap: the class channel changes, no level is
    /// given. The Warrior's progress must not run on into the Shaman's.
    #[test]
    fn a_class_switch_starts_a_record_of_unknown_level() {
        let mut t = ExpTracker::new();
        t.on_level_up(10, 61);
        t.on_experience(20, 40.0, false);
        t.on_class_switch(30);
        assert_eq!(t.current_percent(), 0.0);
        assert_eq!(t.current_record().level, None);
        t.on_experience(40, 5.0, false);
        t.on_level_stated(50, 130);
        assert_eq!(t.current_record().level, Some(130), "the /who fills it in");
        assert!(
            (t.current_percent() - 5.0).abs() < 1e-9,
            "and does not restart"
        );
        let gone = t.completed_levels().last().unwrap();
        assert_eq!((gone.level, gone.complete), (Some(61), false));
    }

    /// A statement at the start of a log is not a level-up: the first
    /// record still begins wherever the log does.
    #[test]
    fn a_stated_level_does_not_make_the_first_record_whole() {
        let mut t = ExpTracker::new();
        t.on_level_stated(10, 20);
        t.on_experience(20, 30.0, false);
        t.on_level_up(30, 21);
        assert!(!t.completed_levels()[0].complete);
        assert_eq!(t.completed_levels()[0].level, Some(20));
    }

    #[test]
    fn percent_accumulates_within_a_level() {
        let mut t = ExpTracker::new();
        t.on_experience(100, 1.5, false);
        t.on_experience(110, 2.5, false);
        assert!((t.current_percent() - 4.0).abs() < 1e-9);
        assert_eq!(t.current_record().events, 2);
    }

    /// A bonused gain counts, and says it was bonused.
    ///
    /// Both halves matter. The percentage is a real percentage of a real
    /// level, so it belongs in `percent` and the level still sums to ~100 --
    /// dropping it or discounting it by a guessed multiplier would break the
    /// one property this whole module rests on. And it is held separately as
    /// well, because levels/hour measured through a bonus event is not a rate
    /// anybody can plan the next level by.
    #[test]
    fn a_bonused_gain_is_counted_and_marked() {
        let mut t = ExpTracker::new();
        t.on_experience(100, 1.0, false);
        t.on_experience(110, 3.0, true);
        t.on_experience(120, 2.0, true);
        let cur = t.current_record();
        assert!(
            (cur.percent - 6.0).abs() < 1e-9,
            "the bonused gains were dropped"
        );
        assert_eq!(cur.events, 3);
        assert!((cur.bonus_percent - 5.0).abs() < 1e-9);
        assert_eq!(cur.bonus_events, 2);
    }

    #[test]
    fn level_up_closes_the_record_for_the_level_below() {
        let mut t = ExpTracker::new();
        t.on_experience(100, 50.0, false);
        t.on_level_up(200, 10);
        // Reaching 10 means we just finished 9.
        assert_eq!(t.completed_levels()[0].level, Some(9));
        assert_eq!(t.current_level(), Some(10));
        assert_eq!(t.current_percent(), 0.0, "progress resets on level up");
    }

    /// The first record starts wherever the log does, so its rate would be a
    /// lie if treated as a whole level.
    #[test]
    fn first_partial_level_is_marked_incomplete() {
        let mut t = ExpTracker::new();
        t.on_experience(100, 15.0, false);
        t.on_level_up(200, 10); // first level-up: partial
        t.on_experience(300, 100.0, false);
        t.on_level_up(400, 11); // second: a whole level
        assert!(!t.completed_levels()[0].complete);
        assert!(t.completed_levels()[1].complete);
        assert_eq!(t.average_kills_per_level(), Some(0.0));
    }

    #[test]
    fn averages_use_only_whole_levels() {
        let mut t = ExpTracker::new();
        t.on_experience(0, 10.0, false);
        t.on_level_up(100, 10); // partial, 100s
        t.on_experience(150, 100.0, false);
        t.on_level_up(300, 11); // whole, 200s
        t.on_experience(350, 100.0, false);
        t.on_level_up(700, 12); // whole, 400s
        assert_eq!(
            t.average_seconds_per_level(),
            Some(300),
            "partial first level must be excluded"
        );
    }

    /// The ordering that actually occurs in EverQuest logs: the experience
    /// message is written *before* "You have slain X!". Attributing only
    /// backwards left virtually every kill uncredited.
    #[test]
    fn experience_logged_before_the_kill_is_attributed() {
        let mut t = ExpTracker::new();
        t.on_experience(100, 2.0, false);
        t.on_kill(100, "a fire giant warrior");
        t.on_experience(200, 2.4, false);
        t.on_kill(201, "a fire giant warrior");

        let mobs = t.mobs_by_exp_per_kill(1);
        assert_eq!(mobs.len(), 1);
        assert_eq!(mobs[0].kills, 2, "both kills must be credited");
        assert!((mobs[0].percent - 4.4).abs() < 1e-9);
        assert_eq!(t.current_record().kills, 2);
        assert_eq!(t.kills_total(), 2);
        assert_eq!(t.kills_without_exp(), 0);
    }

    /// Repeated kills of the same mob type must keep incrementing. Inferring
    /// "already counted" from the tally's contents stalled this at one.
    #[test]
    fn repeat_kills_of_one_mob_type_all_count() {
        let mut t = ExpTracker::new();
        for i in 0..5 {
            let ts = 100 + i * 100;
            t.on_experience(ts, 1.0, false);
            t.on_kill(ts, "gnomish miner");
        }
        let mobs = t.mobs_by_exp_per_kill(1);
        assert_eq!(mobs[0].kills, 5);
        assert!((mobs[0].percent_per_kill() - 1.0).abs() < 1e-9);
    }

    /// A grey con yields nothing; it should count as a kill but not drag the
    /// experience-per-kill average down to zero.
    #[test]
    fn kills_without_experience_are_tracked_separately() {
        let mut t = ExpTracker::new();
        t.on_experience(100, 3.0, false);
        t.on_kill(100, "a fire giant");
        t.on_kill(500, "a grey rat"); // no experience at all
        assert_eq!(t.kills_total(), 2);
        assert_eq!(t.kills_without_exp(), 1);
        assert_eq!(t.current_record().kills, 1, "only rewarded kills count");
        assert!(t
            .mobs_by_exp_per_kill(1)
            .iter()
            .all(|m| m.name != "a grey rat"));
    }

    #[test]
    fn experience_is_attributed_to_the_mob_just_killed() {
        let mut t = ExpTracker::new();
        t.on_kill(100, "a fire giant warrior");
        t.on_experience(101, 2.0, false);
        t.on_kill(200, "a lesser mummy");
        t.on_experience(201, 0.5, false);
        t.on_kill(300, "a fire giant warrior");
        t.on_experience(301, 2.2, false);

        let mobs = t.mobs_by_exp_per_kill(1);
        assert_eq!(mobs[0].name, "a fire giant warrior");
        assert_eq!(mobs[0].kills, 2);
        assert!((mobs[0].percent - 4.2).abs() < 1e-9);
        assert!((mobs[0].percent_per_kill() - 2.1).abs() < 1e-9);
        // The weaker mob ranks last.
        assert_eq!(mobs.last().unwrap().name, "a lesser mummy");
    }

    /// A kill long before the experience message is not its cause.
    #[test]
    fn stale_kill_is_not_credited() {
        let mut t = ExpTracker::new();
        t.on_kill(100, "a rat");
        t.on_experience(1000, 5.0, false);
        assert!(
            t.mobs_by_exp_per_kill(1).is_empty(),
            "experience far from a kill should not be attributed to it"
        );
        assert_eq!(t.current_record().kills, 0);
    }

    /// Solo and party experience can both fire for one kill; the kill must be
    /// counted once.
    #[test]
    fn one_kill_is_not_double_counted_by_two_messages() {
        let mut t = ExpTracker::new();
        t.on_kill(100, "a rat");
        t.on_experience(101, 1.0, false);
        t.on_experience(101, 0.2, false);
        let mobs = t.mobs_by_exp_per_kill(1);
        assert_eq!(mobs[0].kills, 1, "one kill despite two messages");
        assert_eq!(t.current_record().kills, 1);
        // Both messages still count toward the total.
        assert!((t.current_percent() - 1.2).abs() < 1e-9);
    }

    #[test]
    fn zone_time_and_rate_are_tracked() {
        let mut t = ExpTracker::new();
        t.on_zone(0, "Nagafen's Lair");
        t.on_kill(10, "a fire giant");
        t.on_experience(11, 5.0, false);
        t.on_kill(20, "a fire giant");
        t.on_experience(21, 5.0, false);
        // Leaving after an hour credits the time.
        t.on_zone(3600, "West Commonlands");

        let zones = t.zones_by_rate(0);
        let lair = zones.iter().find(|z| z.name == "Nagafen's Lair").unwrap();
        assert!((lair.percent - 10.0).abs() < 1e-9);
        assert_eq!(lair.kills, 2);
        assert_eq!(lair.seconds, 3600);
        assert!((lair.percent_per_hour() - 10.0).abs() < 0.01);
    }

    /// The zone being camped right now must accrue time as the fight goes on,
    /// not only when it is finally left. Otherwise the ranking sits empty for
    /// the whole session it is meant to describe.
    #[test]
    fn the_current_zone_accrues_time_without_leaving_it() {
        let mut t = ExpTracker::new();
        t.on_zone(0, "Chardok");
        // Half an hour of killing, still standing in the zone.
        for i in 0..30 {
            let at = i * 60;
            t.on_kill(at, "an iksar");
            t.on_experience(at + 1, 1.0, false);
        }

        let zones = t.zones_by_rate(10 * 60);
        let chardok = zones
            .iter()
            .find(|z| z.name == "Chardok")
            .expect("the zone being camped should be ranked");
        assert_eq!(chardok.kills, 30);
        assert_eq!(chardok.seconds, 29 * 60 + 1, "time up to the last event");
        assert!((chardok.percent - 30.0).abs() < 1e-9);
    }

    /// The kill count must survive a kill logged before any experience in the
    /// zone, which is the ordering that leaves `by_zone` with no entry yet.
    #[test]
    fn a_kill_before_any_zone_entry_is_still_counted() {
        let mut t = ExpTracker::new();
        t.on_zone(100, "Sebilis");
        t.on_kill(100, "a froglok");
        t.on_experience(100, 2.0, false);

        let zones = t.zones_by_rate(0);
        let seb = zones.iter().find(|z| z.name == "Sebilis").unwrap();
        assert_eq!(seb.kills, 1);
    }

    #[test]
    fn zones_below_the_time_threshold_are_excluded() {
        let mut t = ExpTracker::new();
        t.on_zone(0, "Passing Through");
        t.on_experience(5, 0.1, false);
        t.on_zone(10, "Somewhere Else");
        assert!(
            t.zones_by_rate(600).is_empty(),
            "a ten second visit yields a meaningless rate"
        );
    }

    #[test]
    fn rate_and_eta_are_derived_from_recent_gains() {
        let mut t = ExpTracker::new();
        // 10% over 10 minutes => 60%/hour.
        t.on_experience(0, 5.0, false);
        t.on_experience(600, 5.0, false);
        let rate = t.current_rate_per_hour(600);
        assert!((rate - 60.0).abs() < 0.1, "rate was {rate}");
        // 90% remaining at 60%/hour = 1.5 hours.
        let eta = t.seconds_to_next_level(600).unwrap();
        assert!((eta - 5400).abs() < 60, "eta was {eta}");
    }

    #[test]
    fn no_recent_gains_means_no_eta() {
        let t = ExpTracker::new();
        assert_eq!(t.seconds_to_next_level(1000), None);
        assert_eq!(t.current_rate_per_hour(1000), 0.0);
    }

    #[test]
    fn ability_points_accumulate_and_track_balance() {
        let mut t = ExpTracker::new();
        t.on_ability_points(0, 2, 2);
        t.on_ability_points(100, 2, 4);
        // Spending drops the balance but not lifetime earned.
        t.on_ability_spent(150, "Mnemonic Retention", 2, 3);
        t.on_ability_points(200, 1, 2);
        assert_eq!(t.aa_earned(), 5, "lifetime earned keeps climbing");
        assert_eq!(t.aa_unspent(), 2, "balance follows the client's figure");
    }

    /// The client only restates the balance when points are *gained*, so a
    /// purchase after the final gain would otherwise leave it overstated.
    #[test]
    fn spending_after_the_last_gain_updates_the_balance() {
        let mut t = ExpTracker::new();
        t.on_ability_points(0, 6, 6);
        t.on_ability_spent(100, "Jam Fest", 1, 4);
        assert_eq!(t.aa_unspent(), 2, "balance must drop as points are spent");
        assert_eq!(
            t.aa_spent_total(),
            4,
            "every purchase counts toward the total"
        );
        assert_eq!(t.aa_earned(), 6, "earned is unaffected by spending");
        // Never underflow, even if a purchase predates the balance we saw.
        t.on_ability_spent(200, "Something", 1, 99);
        assert_eq!(t.aa_unspent(), 0);
    }

    /// Normal award is one point; a potion doubles it. The award size is the
    /// only potion signal the client provides.
    #[test]
    fn doubled_awards_are_distinguished_from_normal_ones() {
        let mut t = ExpTracker::new();
        t.on_ability_points(0, 1, 1); // no potion
        t.on_ability_points(100, 2, 3); // potion up
        t.on_ability_points(200, 2, 5);
        t.on_ability_points(300, 2, 7);

        assert_eq!(t.aa_gains_normal(), 1);
        assert_eq!(t.aa_gains_doubled(), 3);
        assert!((t.aa_doubled_share() - 0.75).abs() < 1e-9);
        // 7 points over 4 awards.
        assert!((t.aa_effective_multiplier() - 1.75).abs() < 1e-9);
        assert!(t.aa_potion_likely_active(), "last award was doubled");
    }

    #[test]
    fn a_normal_award_clears_the_potion_hint() {
        let mut t = ExpTracker::new();
        t.on_ability_points(0, 2, 2);
        assert!(t.aa_potion_likely_active());
        t.on_ability_points(100, 1, 3);
        assert!(
            !t.aa_potion_likely_active(),
            "an undoubled award means the potion has lapsed"
        );
    }

    /// A potion grants exactly five doubled awards, consumed one at a time.
    #[test]
    fn a_potion_grants_five_charges() {
        let mut t = ExpTracker::new();
        t.on_aa_potion(0);
        assert_eq!(t.aa_potion_charges(), 5);
        for i in 1..=5 {
            t.on_ability_points(i * 100, 2, i as u32 * 2);
            assert_eq!(t.aa_potion_charges(), 5 - i as u32);
        }
        assert!(!t.aa_potion_likely_active(), "charges exhausted");
        assert_eq!(t.aa_doubled_unaccounted(), 0);
    }

    /// Drinking before the previous potion is spent stacks the charges — the
    /// real log peaks at nine held at once.
    #[test]
    fn charges_stack_when_potions_overlap() {
        let mut t = ExpTracker::new();
        t.on_aa_potion(0);
        t.on_ability_points(100, 2, 2); // 4 left
        t.on_aa_potion(200); // +5 => 9
        assert_eq!(t.aa_potion_charges(), 9);
    }

    /// A log that begins mid-potion sees doubled awards with no charge on the
    /// books. They must be recorded, not silently dropped or made negative.
    #[test]
    fn doubled_awards_before_any_potion_are_flagged() {
        let mut t = ExpTracker::new();
        t.on_ability_points(0, 2, 2);
        t.on_ability_points(100, 2, 4);
        assert_eq!(t.aa_potion_charges(), 0, "must not underflow");
        assert_eq!(t.aa_doubled_unaccounted(), 2);
        assert!(
            t.aa_potion_likely_active(),
            "a doubled award still proves a potion is up"
        );
    }

    /// A normal award proves the potion has lapsed, so any charges we thought
    /// remained were wrong — trust the observation over the model.
    #[test]
    fn a_normal_award_clears_remaining_charges() {
        let mut t = ExpTracker::new();
        t.on_aa_potion(0);
        assert_eq!(t.aa_potion_charges(), 5);
        t.on_ability_points(100, 1, 1);
        assert_eq!(
            t.aa_potion_charges(),
            0,
            "an undoubled award overrides the charge count"
        );
        assert!(!t.aa_potion_likely_active());
    }

    #[test]
    fn potion_consumption_is_counted() {
        let mut t = ExpTracker::new();
        assert_eq!(t.aa_potions_used(), 0);
        t.on_aa_potion(500);
        t.on_aa_potion(4000);
        assert_eq!(t.aa_potions_used(), 2);
        assert_eq!(t.last_potion_at(), Some(4000));
    }

    #[test]
    fn aa_potion_is_recognised_by_name() {
        assert!(is_aa_potion("Bottle of Alternate Adventure"));
        assert!(is_aa_potion("bottle of alternate adventure ii"));
        assert!(!is_aa_potion("Potion of Accuracy"));
        assert!(!is_aa_potion("Elixir of Greater Concentration"));
    }

    #[test]
    fn multiplier_is_zero_before_any_award() {
        let t = ExpTracker::new();
        assert_eq!(t.aa_effective_multiplier(), 0.0);
        assert_eq!(t.aa_doubled_share(), 0.0);
        assert!(!t.aa_potion_likely_active());
    }

    #[test]
    fn ability_purchases_are_tallied_per_ability() {
        let mut t = ExpTracker::new();
        t.on_ability_spent(100, "Combat Fury", 1, 1);
        t.on_ability_spent(200, "Combat Fury", 2, 2);
        t.on_ability_spent(300, "Jam Fest", 1, 5);
        let p = t.aa_purchases();
        assert_eq!(p[0].ability, "Jam Fest");
        assert_eq!(p[0].spent, 5);
        let fury = p.iter().find(|x| x.ability == "Combat Fury").unwrap();
        assert_eq!(fury.rank, 2, "highest rank reached");
        assert_eq!(fury.spent, 3, "costs accumulate across ranks");
    }

    /// The real Symphonic Aura sequence from the Tharn log, whose costs match
    /// the wiki's `0/0/3/0/3/0/3/0/3/0` exactly.
    ///
    /// The client sells one ability as two that share a ladder: the odd ranks
    /// arrive as `: Disabled` and carry the cost, the even ones as `: Enabled`
    /// and are free. Ten client ranks are five real ranks — five aura songs —
    /// for twelve points, ending switched on.
    #[test]
    fn a_toggle_ability_is_folded_into_one_purchase() {
        let mut t = ExpTracker::new();
        let seq: [(&str, u32, u32); 10] = [
            ("Symphonic Aura: Disabled", 1, 0),
            ("Symphonic Aura: Enabled", 2, 0),
            ("Symphonic Aura: Disabled", 3, 3),
            ("Symphonic Aura: Enabled", 4, 0),
            ("Symphonic Aura: Disabled", 5, 3),
            ("Symphonic Aura: Enabled", 6, 0),
            ("Symphonic Aura: Disabled", 7, 3),
            ("Symphonic Aura: Enabled", 8, 0),
            ("Symphonic Aura: Disabled", 9, 3),
            ("Symphonic Aura: Enabled", 10, 0),
        ];
        for (i, (name, rank, cost)) in seq.iter().enumerate() {
            t.on_ability_spent(100 + i as i64, name, *rank, *cost);
        }

        let p = t.aa_purchases();
        let aura: Vec<_> = p
            .iter()
            .filter(|x| x.ability.contains("Symphonic"))
            .collect();
        assert_eq!(aura.len(), 1, "one ability, not two halves");
        let a = aura[0];
        assert_eq!(a.ability, "Symphonic Aura");
        assert_eq!(a.rank, 5, "five ranks, for five aura songs");
        assert_eq!(a.client_rank, 10, "the client counts the flips too");
        assert_eq!(a.spent, 12, "every point lands on the one ability");
        assert_eq!(a.enabled, Some(true), "the last rank bought left it on");
        assert!(
            a.unlogged_ranks.is_empty(),
            "the folded ladder is continuous, so nothing is missing"
        );
    }

    /// Buying the paid half last leaves it switched off.
    #[test]
    fn a_toggle_reports_the_state_of_its_highest_rank() {
        let mut t = ExpTracker::new();
        t.on_ability_spent(100, "Symphonic Aura: Disabled", 1, 0);
        t.on_ability_spent(200, "Symphonic Aura: Enabled", 2, 0);
        t.on_ability_spent(300, "Symphonic Aura: Disabled", 3, 3);
        let a = &t.aa_purchases()[0];
        assert_eq!(a.rank, 2, "rank 3 of the ladder is the second real rank");
        assert_eq!(a.enabled, Some(false));
        assert_eq!(a.spent, 3);
    }

    /// An ordinary ability keeps counting ranks the way it always did.
    #[test]
    fn a_plain_ability_is_not_treated_as_a_toggle() {
        let mut t = ExpTracker::new();
        t.on_ability_spent(100, "Combat Fury", 1, 1);
        t.on_ability_spent(200, "Combat Fury", 2, 2);
        let a = &t.aa_purchases()[0];
        assert_eq!(a.rank, 2);
        assert_eq!(a.client_rank, 2);
        assert_eq!(a.enabled, None);
    }

    #[test]
    fn aa_rate_is_measured_over_the_window() {
        let mut t = ExpTracker::new();
        // 6 points in an hour.
        t.on_ability_points(0, 2, 2);
        t.on_ability_points(1800, 2, 4);
        t.on_ability_points(3600, 2, 6);
        let rate = t.aa_per_hour(3600, 3600);
        assert!((rate - 6.0).abs() < 0.01, "rate was {rate}");
    }

    /// A quiet stretch must pull the rate down, not leave it at its old value.
    #[test]
    fn aa_rate_decays_when_nothing_is_earned() {
        let mut t = ExpTracker::new();
        t.on_ability_points(0, 4, 4);
        // An hour later, nothing earned in the last hour.
        assert_eq!(t.aa_per_hour(7200, 3600), 0.0);
    }

    #[test]
    fn aa_is_attributed_to_the_zone() {
        let mut t = ExpTracker::new();
        t.on_zone(0, "The Ruins of Old Guk");
        t.on_ability_points(600, 3, 3);
        t.on_zone(3600, "Nagafen's Lair");
        t.on_ability_points(4000, 1, 4);

        let zones = t.zones_by_aa_rate(0);
        let guk = zones
            .iter()
            .find(|z| z.name == "The Ruins of Old Guk")
            .unwrap();
        assert_eq!(guk.aa, 3);
        assert!((guk.aa_per_hour() - 3.0).abs() < 0.01);
    }

    #[test]
    fn percent_per_hour_on_a_level_record() {
        let mut t = ExpTracker::new();
        t.on_experience(0, 0.0, false);
        t.on_experience(1800, 50.0, false);
        t.on_level_up(3600, 20);
        let r = &t.completed_levels()[0];
        assert_eq!(r.seconds, 3600);
        assert!((r.percent_per_hour() - 50.0).abs() < 0.1);
    }

    /// Regression: zero is a legitimate timestamp. Treating it as "unset"
    /// moved the level's start to the second event and halved its duration.
    #[test]
    fn a_zero_timestamp_starts_the_level() {
        let mut t = ExpTracker::new();
        t.on_experience(0, 10.0, false);
        t.on_experience(100, 10.0, false);
        assert_eq!(
            t.current_record().started,
            0,
            "the first event defines the start, even at t=0"
        );
        assert_eq!(t.current_record().seconds, 100);
    }

    /// After a level-up the clock runs from the level-up itself, so a level
    /// spent travelling before the first kill is not reported as instant.
    #[test]
    fn level_duration_runs_from_the_level_up() {
        let mut t = ExpTracker::new();
        t.on_experience(0, 100.0, false);
        t.on_level_up(1000, 30);
        // Ten minutes of travel, then experience.
        t.on_experience(1600, 5.0, false);
        assert_eq!(
            t.current_record().started,
            1000,
            "the new level starts when it was reached"
        );
        assert_eq!(t.current_record().seconds, 600);
    }
}
