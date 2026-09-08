//! The contribution bundle: everything one machine's parses know, in a form
//! another machine can read.
//!
//! A bundle is the whole contract between the parser and the site. It is a
//! single JSON document rather than a stream of updates because that makes it
//! an object you can hold: keep it, diff it, re-ingest it, hand it to someone
//! else, or throw the site away and rebuild from the bundles on file.
//!
//! Two properties are load-bearing:
//!
//! * **A bundle is complete, not incremental.** It states everything its
//!   contributor knows at the moment it was written, because the history it
//!   is drawn from is already cumulative across every log ever parsed on that
//!   machine. Ingest therefore *replaces* a contributor's facts rather than
//!   adding to them — see eqldb-core's `store`. An incremental bundle would need
//!   the two sides to agree on what had already been sent, which is a
//!   synchronisation problem neither side is in a position to solve.
//!
//! * **Nothing derived is carried.** Counts and their denominators travel;
//!   rates do not. A drop sighting says how many times an item was looted and
//!   how many times the creature was killed, and refuses to divide them,
//!   because kills are counted whether or not the corpse was looted and the
//!   quotient is a floor rather than a rate. Publishing the floor as if it
//!   were the rate is the one mistake this format is shaped to prevent.
//!
//! Filesystem paths are deliberately absent. A log's path names a person's
//! home directory and says nothing about the game, so each log appears as a
//! stable opaque id plus the character and server it belongs to — which are
//! public in-game facts already.
//!
//! # The game is declared once, at the top
//!
//! Every row used to carry its own game tag, from when one schema served both
//! games. It does not any more: eqldb holds EverQuest Legends and eqdb holds
//! EverQuest, so a tag repeated across eighteen row types was eighteen chances
//! to disagree with itself about a question the site had already answered.
//!
//! What the tag was actually *for* survives, because it is worth keeping: it
//! is what stops an eqdb bundle being ingested here, and the two games are
//! indistinguishable line by line. So the bundle names its game once, in
//! [`Bundle::game`], and ingest refuses anything that is not this site's. One
//! check at the boundary, rather than a column on every table behind it.

use crate::bestiary::NpcProfile;
use crate::game::Game;
use crate::recipe::Recipe;
use serde::{Deserialize, Serialize};

/// Bumped when a reader can no longer make sense of an older document.
///
/// Adding an optional field does not need a bump; removing one, or changing
/// what an existing field means, does.
///
/// 2 dropped the per-row `game` field in favour of [`Bundle::game`]. A
/// version 1 document still deserialises — the removed fields were all
/// `#[serde(default)]` and unknown fields are ignored — but it arrives with an
/// empty `game`, which ingest rejects rather than assuming. That is the
/// intended behaviour and not an oversight: a version 1 bundle genuinely does
/// not state, in one place, which game it describes.
pub const BUNDLE_VERSION: u32 = 2;

/// One contributor's complete statement of what their parses have observed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub bundle_version: u32,
    /// When the bundle was written, in seconds since the epoch.
    pub generated_at: i64,
    /// What wrote it, name and version, for when a bad export needs tracing
    /// back to the code that produced it.
    pub generator: String,
    /// Who is asserting all of this. The ingest key: a second bundle from the
    /// same contributor replaces the first one's facts entirely.
    pub contributor: String,
    /// Which game every row in this document describes.
    ///
    /// The one place the game appears, and the only thing standing between an
    /// eqdb bundle and this site's database. EverQuest and EverQuest Legends
    /// share a client and a log format, so nothing further down the document
    /// can be inspected to tell them apart — by the time a drop row is being
    /// read, the question is already unanswerable.
    ///
    /// Empty rather than defaulted to this site's game, because a bundle that
    /// does not say is a bundle nobody can place, and guessing is the failure
    /// this field exists to prevent. Ingest rejects it.
    #[serde(default)]
    pub game: String,
    /// Free text the contributor wants carried alongside the data.
    #[serde(default)]
    pub note: String,

    /// Which optional inputs this export actually read.
    ///
    /// A bundle replaces everything its contributor previously asserted, and
    /// that is the right contract — it is what makes a re-export authoritative
    /// rather than additive. But several of the export's inputs are optional
    /// flags, some of them depending on a daemon being up, and when one is
    /// absent the bundle does not say so. It simply arrives smaller, and the
    /// ingest faithfully replaces richer stored data with poorer new data.
    ///
    /// That is not hypothetical: an export run with the parser closed carried
    /// 103 achievements where the database held 492, and the 389 that vanished
    /// were never reported by anything. The export's own summary read like a
    /// success, because it counted what it had rather than what it had lost.
    ///
    /// So the bundle now states which inputs it consulted, and the ingest
    /// declines to clear a table whose sources are all absent. Absence of a
    /// source is the real fact and it is knowable here, at export time —
    /// unlike a shrink, which can only be guessed at from the far side and
    /// needs a threshold to recognise.
    ///
    /// `None` is a bundle written before this field existed, and keeps the
    /// old contract: it replaces everything, because there is no way to ask
    /// it what it read.
    ///
    /// `Some([])` is a *current* export that consulted nothing optional, and
    /// is the case this exists for. The two must not be conflated — they were
    /// at first, with the field a plain `Vec`, and an export run with no
    /// optional sources at all emptied eighteen tables while the guard stood
    /// by, because "read nothing" and "cannot say" looked identical.
    #[serde(default)]
    pub sources: Option<Vec<String>>,

    /// What this bundle is an observation *of*.
    ///
    /// The whole basis of idempotence, and the field that lets a bundle stop
    /// being a snapshot. `sources` above says which *tables* the export
    /// consulted; this says which *bytes* it read, so the same span is never
    /// counted twice and a client can ask the server where to resume rather
    /// than remembering what it sent.
    ///
    /// Empty is not "nothing was read" — it is a bundle written before this
    /// field existed, or one whose producer cannot say. Those merge but
    /// cannot be superseded or withdrawn, because nothing identifies what
    /// they covered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covers: Vec<Observed>,

    /// Bundles this one re-states, by `bundle_id`.
    ///
    /// The commonest operation while a reader is still under repair is not
    /// *send something new* — it is **send that span again, correctly**, once
    /// the parser is fixed. A span already held is refused, so without this
    /// the operation performed most often would be the one the contract
    /// forbids.
    ///
    /// Withdraw-then-resend would also work and is worse: it leaves a window
    /// where the site is missing data that is about to come back. The server
    /// withdraws these and applies this bundle in one transaction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,

    /// The logs behind the numbers, named without naming the filesystem.
    #[serde(default)]
    pub logs: Vec<LogSource>,

    #[serde(default)]
    pub drops: Vec<DropRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raids: Vec<RaidRow>,
    #[serde(default)]
    pub class_armor: Vec<ClassArmorRow>,
    /// What a complete set is. The denominator, stated: see
    /// `ClassArmorRow`.
    #[serde(default)]
    pub class_armor_slots: Vec<String>,
    #[serde(default)]
    pub kills: Vec<KillRow>,
    #[serde(default)]
    pub vendors: Vec<VendorRow>,
    /// Creature profiles, exactly as the parser's bestiary models them.
    #[serde(default)]
    pub creatures: Vec<NpcProfile>,

    #[serde(default)]
    pub quests: Vec<Quest>,
    #[serde(default)]
    pub achievements: Vec<Achievement>,
    /// Tooltip readings, transcribed from screenshots.
    #[serde(default)]
    pub item_facts: Vec<ItemFacts>,
    /// Which creatures move which factions, from the log.
    #[serde(default)]
    pub faction_hits: Vec<FactionHit>,
    /// What handing an item to an NPC moves, from the log.
    #[serde(default)]
    pub turnins: Vec<TurnIn>,
    /// Levels and rare markers, from consider lines.
    #[serde(default)]
    pub considers: Vec<Consider>,
    /// Every zone the game has, from its own achievements.
    #[serde(default)]
    pub zone_roster: Vec<RosterZone>,
    /// Which icon each item shows, read off its own tooltip.
    #[serde(default)]
    pub item_icons: Vec<ItemIcon>,
    /// How each creature comes back, where somebody has established it.
    #[serde(default)]
    pub spawn_rules: Vec<SpawnRule>,
    /// The ordinary respawn timer, in seconds — the one place the game's
    /// roughly six minutes is written down. A creature says only how it
    /// *differs* from this; see [`SpawnRule`].
    #[serde(default)]
    pub spawn_default_seconds: Option<u32>,
    /// The game's own item ids, from an inventory dump.
    #[serde(default)]
    pub item_ids: Vec<ItemId>,
    /// Where things stand in a zone, walked and `/loc`'d by hand.
    #[serde(default)]
    pub markers: Vec<ZoneMarker>,
    /// Where a creature was standing when it died.
    ///
    /// Deliberately not a marker, though the two are both a name and a
    /// position. A marker is curated: one row per thing, a role somebody
    /// typed, a decision that the spot is worth a pin on the map. A spot is
    /// raw — every kill that happened to have a `/loc` beside it, however
    /// many that turns out to be, and no judgement about whether the place
    /// matters. Several per creature is the answer rather than noise: a
    /// creature with four spawn points has four, and nothing here may
    /// average them into one.
    #[serde(default)]
    pub spots: Vec<CreatureSpotRow>,
    /// Which archive a zone's map is drawn from, learned from `/who`.
    #[serde(default)]
    pub zone_shortnames: Vec<ZoneShortname>,
    /// The servers observed, and what rules each plays by.
    #[serde(default)]
    pub servers: Vec<ServerRules>,
    /// The game's expansions, in release order.
    #[serde(default)]
    pub expansions: Vec<Expansion>,
    /// Which expansion each zone belongs to.
    #[serde(default)]
    pub expansion_zones: Vec<ExpansionZone>,
    /// What each ruleset means, for a page that has to explain itself.
    #[serde(default)]
    pub rulesets: Vec<Ruleset>,

    /// The AA catalogue, as transcribed from the wiki.
    #[serde(default)]
    pub aa: Vec<AaAbility>,
    /// What the parses actually saw bought, aggregated past the player.
    #[serde(default)]
    pub aa_observed: Vec<AaObserved>,
    #[serde(default)]
    pub aa_note: String,
    #[serde(default)]
    pub aa_source: String,

    /// Spells the game handed over, aggregated past the player.
    #[serde(default)]
    pub spell_grants: Vec<SpellGrant>,

    /// How high each skill has been seen to go.
    #[serde(default)]
    pub skills: Vec<SkillObserved>,

    /// The Actions window's abilities, read off their tooltips.
    #[serde(default)]
    pub abilities: Vec<Ability>,

    /// Recipes nobody wrote down — reconstructed from combines in a log.
    #[serde(default)]
    pub observed_recipes: Vec<ObservedRecipe>,

    /// The recipe book, carried whole so its hand-maintained prose survives.
    #[serde(default)]
    pub recipes: Vec<Recipe>,
    /// The book's own note about what it is and how it was assembled.
    #[serde(default)]
    pub recipe_note: String,
    /// Items that a combine hands back rather than consumes.
    #[serde(default)]
    pub recipe_tools: Vec<String>,
    /// Books, read page by page off their own window.
    #[serde(default)]
    pub books: Vec<Book>,
}

/// One book, with the pages somebody has photographed of it.
///
/// # Why the pages are nested
///
/// A page with no book is not a fact about anything -- the text is the book's
/// and the number means nothing without it. Nesting is what makes that
/// unrepresentable, rather than a rule two tables have to keep. The same
/// argument `Recipe` makes about its components.
///
/// # Why the book has a name and the pages do not
///
/// A book is an item you read, so its name is the item's, and two photographs
/// of the same book must land on one page list rather than two. Spelling it
/// once per book is what makes that true by construction: a contributor who
/// types `Guide to Blacksmithing` on one page and `Guide To Blacksmithing` on
/// the next has made one mistake, not created a second book.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Book {
    /// The item's name, as the window spells it.
    pub name: String,
    /// The pages, in whatever order they were photographed. The reader sorts
    /// them; this is a set.
    #[serde(default)]
    pub pages: Vec<BookPage>,
}

/// One page of one book.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BookPage {
    /// The page number the client prints above the text, or 0 for a page
    /// nobody has numbered.
    ///
    /// Zero is a real answer and not a default standing in for one. The
    /// number is printed in a face the reader's atlas only partly covers --
    /// `8` decodes at 0.998 and `7` comes back as ink it cannot name -- so a
    /// page can arrive transcribed and unnumbered, and pretending it is page
    /// zero would sort it first. Unnumbered pages sort last and say so.
    #[serde(default)]
    pub page: i64,
    /// The page's text, lines joined with `\n`. The client's own wrapping is
    /// kept: it is where the line breaks are, and re-flowing it would be this
    /// end guessing at a width.
    pub text: String,
    /// The crop it was read off, so a disputed page can be re-examined
    /// against the same pixels.
    #[serde(default)]
    pub evidence: String,
}

/// The game this site holds.
///
/// eqldb serves EverQuest Legends and eqdb serves EverQuest. That is a
/// property of the repository rather than of any row in it, which is why it is
/// a constant here and not a column anywhere.
pub const SITE_GAME: Game = Game::EverQuestLegends;

/// Which of [`Bundle::sources`] fills each table an ingest would clear.
///
/// A table listed here is cleared only when at least one of its sources was
/// consulted. A table absent from this list is filled from the parser history
/// or the log itself, which the export cannot run without, so there is no
/// state in which it arrives empty by accident.
///
/// `achievements` has two, and needs both listed rather than either alone.
/// The window dump names every achievement with its category; the daemon's
/// task API adds the ones play has since completed. Either is enough to make
/// the table authoritative, and requiring both would refuse perfectly good
/// exports.
pub const TABLE_SOURCES: &[(&str, &[&str])] = &[
    // --- what the parse history fills -------------------------------------
    //
    // These were absent from this list until 4 Sep 2026, and the reasoning
    // was sound while it lasted: `eqldb-export` takes `--history` as a
    // `PathBuf` rather than an `Option`, opens it before anything else, and
    // cannot run without it -- so a bundle it wrote had always read the log,
    // and these tables could not arrive empty by accident.
    //
    // `POST /api/bundle` ended that. A bundle now arrives from whoever holds
    // a token, and nothing about the document says it came from an export.
    // A submitter sending only item tooltips -- `sources: ["item_facts"]`,
    // exactly the shape EQLDB-188's OCR reader is specified to send --
    // reached `may_replace`'s default of true for every table not named here
    // and silently deleted that contributor's drops, kills and creatures.
    //
    // So the export declares `history` and these are gated on it. A bundle
    // written before that field keeps the contract it was written under, by
    // the `sources: None` rule below; one written by an *older* export that
    // named other sources now keeps these tables rather than replacing them,
    // and the ingest says which. That is the safe direction: it holds data it
    // might have replaced instead of deleting data it was never told about.
    ("logs", &["history"]),
    ("drops", &["history"]),
    ("drop_places", &["history"]),
    ("kills", &["history"]),
    ("vendors", &["history"]),
    ("creatures", &["history"]),
    ("creature_melee", &["history"]),
    ("aa_observed", &["history"]),
    ("spell_grants", &["history"]),
    // Raids come from `--raids`, an optional file, and were listed nowhere at
    // all -- so an export run without the flag emptied them. The same shape
    // as the EQLDB-3 incident recorded against `recipes` below, found by the
    // same question asked of every table this time rather than one.
    ("raids", &["raids"]),
    ("raid_encounters", &["raids"]),
    // `bundle.spawn_default_seconds`, which comes from the spawn-rules file
    // its sibling above is already gated on.
    ("spawn_defaults", &["spawn_rules"]),
    ("achievements", &["achievements", "tasks"]),
    // Three feed the quest list: the hand-written walkthroughs, the Plane of
    // Sky chain, and the daemon's task API.
    ("quests", &["quests", "sky", "tasks"]),
    ("quest_items", &["quests", "sky", "tasks"]),
    ("item_facts", &["item_facts"]),
    ("item_facts_stats", &["item_facts"]),
    ("item_augments", &["item_facts"]),
    ("item_effects", &["item_facts"]),
    ("faction_hits", &["faction"]),
    ("turnins", &["faction"]),
    ("considers", &["consider"]),
    ("spawn_rules", &["spawn_rules"]),
    ("item_icons", &["item_icons"]),
    ("zone_roster", &["zone_roster"]),
    ("abilities", &["abilities"]),
    ("skills_seen", &["skills"]),
    ("item_ids", &["inventory"]),
    ("markers", &["labels"]),
    ("books", &["books"]),
    ("book_pages", &["books"]),
    ("creature_spots", &["spots"]),
    ("zone_shortnames", &["labels"]),
    ("observed_recipes", &["tradeskills"]),
    ("observed_components", &["tradeskills"]),
    // The recipe *book*, which is a different source from the combines above.
    //
    // Easy to read as already covered and it is not. An export given no
    // recipe file does not produce an empty book — it falls back to
    // `RecipeBook::embedded()`, EQTraders' crawl of the other game, and the
    // game filter takes 534 of its 535 entries away. So the bundle arrives
    // carrying one recipe and looking entirely plausible, and the ingest
    // replaces the hand-stated book with it.
    //
    // That is the shape of the incident recorded on EQLDB-3: a config mistake
    // that would have deleted 3,848 recipes, caught only because somebody
    // compared the bundle against the previous one by hand.
    ("recipes", &["recipes"]),
    ("recipe_components", &["recipes"]),
    ("recipe_tools", &["recipes"]),
    ("aa", &["aa"]),
    // Stated data, and an export not given the file produces no rows
    // rather than an empty set of them. Without this the first refresh
    // run without --class-armor would silently empty the page.
    ("class_armor", &["class_armor"]),
    ("class_armor_slots", &["class_armor"]),
];

/// Tables no bundle may ever clear for its contributor, whatever it declares.
///
/// A source says "I read this evidence, so my rows for these tables are the
/// whole of it". That is true of a log, and false of everything here — for
/// two different reasons that are worth keeping apart.
///
/// # `item_ids` is a record, not a restatement
///
/// The registry is folded from every inventory dump and loot filter a machine
/// has ever seen, it is one JSON file, and it has a known failure mode: there
/// is a `.before-update` copy beside it precisely because it has been needed.
/// A registry that comes back short would take the site's rows down with it.
///
/// Measured before this was added, against a real send: of 2,259 rows offered,
/// 219 were new, 683 updated their grades, 1,357 were rewritten byte-identical
/// and **none were deleted**. The clear destroyed 2,040 rows to remove zero.
/// It cannot pay for the risk it carries.
///
/// What it costs: a name recorded wrongly cannot be retracted by re-sending,
/// because the key is the name and a correction arrives as a second row. That
/// is a fair trade here and would not be for a log — these names are the
/// client's own spellings out of its own files, not anybody's reading of a
/// screenshot.
///
/// # The item family is a pile of readings, not a survey
///
/// `item_facts` and the three detail tables under it are one row per window
/// somebody photographed. A bundle carrying forty tooltips is saying *I read
/// these forty*, and it has never been in a position to say anything about
/// the four hundred it did not open. The clear read it as the second thing:
/// one tooltip posted with `sources: ["item_facts"]` was measured, on 4
/// September 2026, about to delete 2,706 drops and 3,308 drop_places on its
/// way past — and to answer `202 stored`.
///
/// This does **not** make a reading unretractable, which is the difference
/// from `item_ids` above. A re-read window still replaces the row it names,
/// column for column, and the ingest still deletes that item's stats,
/// augments and effects before writing the new ones. What goes away is the
/// blast radius: the delete is scoped to the items in the bundle instead of
/// to everything the contributor ever photographed.
///
/// `item_icons` is here for a third reason. A row in it is often a *person's*
/// pick, made on the site through the picker, and a re-export from the
/// workstation has never known about those.
pub const NEVER_CLEARED: &[&str] = &[
    "item_ids",
    "item_facts",
    "item_facts_stats",
    "item_augments",
    "item_effects",
    "item_icons",
];

/// Whether an ingest of this bundle may clear `table` for its contributor.
///
/// True for a table nothing optional feeds, and true for one whose source was
/// consulted. False for anything in [`NEVER_CLEARED`], and false when the bundle
/// names its sources — so it is new enough to be believed about this — and
/// none of the ones behind this table are among them.
pub fn may_replace(sources: Option<&Vec<String>>, table: &str) -> bool {
    // First, and before the two defaults below: both of them answer *true*,
    // and a never-cleared table must not be reachable by either — not by a
    // bundle too old to state its sources, and not by one that names the
    // source and is simply wrong about what that entitles it to.
    if NEVER_CLEARED.contains(&table) {
        return false;
    }
    let Some((_, needs)) = TABLE_SOURCES.iter().find(|(t, _)| *t == table) else {
        return true;
    };
    // No list at all: a bundle from before the field. It cannot be asked, so
    // it keeps the contract it was written under.
    let Some(sources) = sources else {
        return true;
    };
    needs.iter().any(|n| sources.iter().any(|s| s == n))
}

impl Bundle {
    /// A bundle with nothing in it yet, and no `generator` on it.
    ///
    /// It used to stamp `concat!("eqldb-export ", env!("CARGO_PKG_VERSION"))`,
    /// which was true while this type lived in the same workspace as the only
    /// program that built one. It is now the *format*, shared by everything
    /// that writes or reads a bundle, and a constant here would have every
    /// bundle claim to have been written by whichever crate the type happens
    /// to live in — a fact about this file rather than about the bundle.
    ///
    /// So the producer says. [`Bundle::generated_by`] is the one line that
    /// does it, and an empty generator arrives at the site visibly empty
    /// rather than plausibly wrong.
    pub fn new(contributor: impl Into<String>, generated_at: i64) -> Bundle {
        Bundle {
            bundle_version: BUNDLE_VERSION,
            generated_at,
            generator: String::new(),
            contributor: contributor.into(),
            game: SITE_GAME.slug().to_string(),
            note: String::new(),
            // Some, not None: a bundle built by this code can always be
            // asked what it read. It has read nothing *yet*, and the export
            // fills the list in from its flags just before writing.
            sources: Some(Vec::new()),
            // Empty, and filled in by whatever assembles the rows: only the
            // producer knows which bytes it actually read. A bundle that
            // never names a span still merges — it simply cannot be
            // superseded or withdrawn later, having said nothing about what
            // it covered.
            covers: Vec::new(),
            supersedes: Vec::new(),
            logs: Vec::new(),
            drops: Vec::new(),
            raids: Vec::new(),
            class_armor: Vec::new(),
            class_armor_slots: Vec::new(),
            kills: Vec::new(),
            vendors: Vec::new(),
            creatures: Vec::new(),
            quests: Vec::new(),
            achievements: Vec::new(),
            item_facts: Vec::new(),
            faction_hits: Vec::new(),
            turnins: Vec::new(),
            considers: Vec::new(),
            zone_roster: Vec::new(),
            item_icons: Vec::new(),
            spawn_rules: Vec::new(),
            spawn_default_seconds: None,
            item_ids: Vec::new(),
            markers: Vec::new(),
            spots: Vec::new(),
            zone_shortnames: Vec::new(),
            servers: Vec::new(),
            rulesets: Vec::new(),
            expansions: Vec::new(),
            expansion_zones: Vec::new(),
            aa: Vec::new(),
            aa_observed: Vec::new(),
            spell_grants: Vec::new(),
            skills: Vec::new(),
            abilities: Vec::new(),
            aa_note: String::new(),
            aa_source: String::new(),
            observed_recipes: Vec::new(),
            recipes: Vec::new(),
            recipe_note: String::new(),
            recipe_tools: Vec::new(),
            books: Vec::new(),
        }
    }

    /// Reject a document this build cannot read, and say which it was.
    pub fn check_version(&self) -> anyhow::Result<()> {
        if self.bundle_version > BUNDLE_VERSION {
            anyhow::bail!(
                "bundle is version {} but this build understands only up to {BUNDLE_VERSION}; \
                 upgrade eqldb rather than ingesting it partially",
                self.bundle_version
            );
        }
        Ok(())
    }

    /// Refuse a bundle describing a game this site does not hold.
    ///
    /// The failure this prevents is silent and permanent. EverQuest and
    /// EverQuest Legends share a client and a log format, so an eqdb bundle
    /// ingested here would insert cleanly, render correctly and be wrong —
    /// classic drops filed as Legends drops, with nothing in the data left to
    /// tell the two apart afterwards. There is no repair short of knowing
    /// which contributor it was and deleting them.
    ///
    /// The tag arrives as free text — a config file, a flag on an importer, a
    /// hand-edited document — and the games are known by several names each.
    /// "eql", "legends" and "everquest-legends" are one game to a person, so
    /// it is resolved through [`Game::parse`], the same table the parser uses,
    /// rather than compared as a string.
    pub fn check_game(&self) -> anyhow::Result<()> {
        let declared = self.game.trim();
        if declared.is_empty() {
            anyhow::bail!(
                "bundle does not say which game it describes. Bundles written before \
                 version {BUNDLE_VERSION} tagged each row instead of the document; \
                 re-export with a current eqldb-export rather than assuming it is \
                 {}",
                SITE_GAME.label()
            );
        }
        match Game::parse(declared) {
            Some(g) if g == SITE_GAME => Ok(()),
            Some(g) => anyhow::bail!(
                "bundle describes {g}, but this site holds {}. Ingesting it would file \
                 {g}'s drops, prices and creatures as {}' own, and nothing in the data \
                 would afterwards distinguish them",
                SITE_GAME.label(),
                SITE_GAME.label(),
            ),
            None => anyhow::bail!(
                "bundle declares game {declared:?}, which is not a game this knows. \
                 Use {} or one of its short forms",
                SITE_GAME.slug(),
            ),
        }
    }

    /// Whether this bundle is the sender's to send.
    ///
    /// Beside [`check_game`](Self::check_game) because it is the same kind of
    /// check -- a question about the document that has to be settled at the
    /// boundary, before anything downstream can act on the answer.
    ///
    /// An ingest replaces `WHERE contributor = $1`. So a bundle that is
    /// allowed to declare a name other than the sender's is a bundle that can
    /// erase any contributor's facts and put its own in their place: the most
    /// destructive thing this codebase can do, reachable by anybody holding
    /// any token. `POST /api/bundle` calls this with the contributor the
    /// *token* names, which is the only name on either side that a stranger
    /// did not choose.
    ///
    /// Not a check the operator's own path needs -- somebody at a shell with
    /// the importer's credential can already write anything -- which is why
    /// it lives here and is called by the endpoint, rather than inside the
    /// ingest where it would also refuse a legitimate hand-run import of
    /// somebody else's bundle.
    /// Name the program that built this, and its version.
    ///
    /// Shown beside the upload on the site, and the first thing looked at
    /// when a bundle carries something surprising — so it names the sender,
    /// not the format.
    #[must_use]
    pub fn generated_by(mut self, generator: impl Into<String>) -> Bundle {
        self.generator = generator.into();
        self
    }

    pub fn check_sender(&self, who: &str) -> anyhow::Result<()> {
        let declared = self.contributor.trim();
        if declared.is_empty() {
            anyhow::bail!("bundle names no contributor, so there is nothing it could replace");
        }
        if declared != who {
            anyhow::bail!(
                "this token speaks for {who}, and that bundle is {declared}'s. Ingesting \
                 it would replace every fact {declared} has contributed with this one's"
            );
        }
        Ok(())
    }

    /// A one-line census, for the ingest log and the export's own report.
    pub fn summary(&self) -> String {
        format!(
            "{} drops, {} kill tallies, {} vendor prices, {} creatures, {} recipes, \
             {} observed recipes, {} quests, {} AAs ({} observations), \
             {} granted spells, {} skills, {} abilities, \
             {} tooltip readings, {} faction pairs, {} turn-in pairs, \
             {} considered, {} spawn rules, {} item icons, {} item ids, {} map markers, \
             {} creature spots, {} books ({} pages), {} logs",
            self.drops.len(),
            self.kills.len(),
            self.vendors.len(),
            self.creatures.len(),
            self.recipes.len(),
            self.observed_recipes.len(),
            self.quests.len(),
            self.aa.len(),
            self.aa_observed.len(),
            self.spell_grants.len(),
            self.skills.len(),
            self.abilities.len(),
            self.item_facts.len(),
            self.faction_hits.len(),
            self.turnins.len(),
            self.considers.len(),
            self.spawn_rules.len(),
            self.item_icons.len(),
            self.item_ids.len(),
            self.markers.len(),
            self.spots.len(),
            self.books.len(),
            self.books.iter().map(|b| b.pages.len()).sum::<usize>(),
            self.logs.len(),
        )
    }
}

/// A log file, identified without disclosing where it lives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSource {
    /// Stable hash of the path. Two exports from the same machine agree on
    /// it; nobody else can turn it back into a path.
    pub id: String,
    pub character: String,
    pub server: String,
    pub bytes: u64,
    pub last_seen: i64,
}

/// One thing a bundle looked at, named so that looking at it twice is
/// recognisable.
///
/// Three producers exist — the daemon parsing logs, the OCR reader posting
/// tooltip windows, and the client table dumps — and each has arrived by its
/// own route with its own rules. One shape makes all three the same kind of
/// statement, which is what collapses the special cases rather than adding a
/// fourth.
///
/// Untagged variants are deliberately avoided: a `Shot` and a `Dump` are both
/// "a string" on the wire, and serde would have to guess. The tag costs a few
/// bytes per entry and removes the guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Observed {
    /// A byte range of one log, half-open: `from` is the first byte read and
    /// `to` is one past the last, so two adjacent spans share a number and do
    /// not overlap.
    ///
    /// `log_key` is the prefix fingerprint, not a hash of the whole file —
    /// the same value [`LogSource::id`] carries and the same one
    /// `uploads::fingerprint` computes on the far side. That is what makes an
    /// appended log the same log: only its first bytes decide its identity,
    /// so a file that grew is recognised rather than re-read from zero.
    Log {
        log_key: String,
        from: u64,
        to: u64,
    },
    /// One screenshot, read and reviewed. The digest `POST /api/shots`
    /// answered with.
    ///
    /// A picture is not appended to, so unlike a log there is no prefix
    /// identity and no span: two files are the same picture or they are two
    /// pictures.
    Shot { shot_key: String },
    /// A client data table, keyed by the content of the dump it came from.
    Dump { digest: String },
}

impl Observed {
    /// Whether two observations describe overlapping evidence.
    ///
    /// The question the server has to answer before it may add a count to
    /// what it already holds. Deliberately here rather than at the far end:
    /// the rule belongs beside the shape it is about, and both sides need it
    /// — the sender to avoid building a block that will be refused, the
    /// receiver to refuse it.
    ///
    /// Spans are half-open, so `0..100` and `100..200` do not overlap. A
    /// `Shot` or a `Dump` overlaps only itself, having no extent.
    pub fn overlaps(&self, other: &Observed) -> bool {
        match (self, other) {
            (
                Observed::Log { log_key: a, from: af, to: at },
                Observed::Log { log_key: b, from: bf, to: bt },
            ) => a == b && at > bf && bt > af,
            (Observed::Shot { shot_key: a }, Observed::Shot { shot_key: b }) => a == b,
            (Observed::Dump { digest: a }, Observed::Dump { digest: b }) => a == b,
            _ => false,
        }
    }
}

/// One item seen dropping from one creature.
///
/// `looted` and `quantity` differ: three drops of a stack of four is three
/// sightings and twelve items, and which one you want depends on the question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DropRow {
    /// The server this was looted on, as the log filename spells it.
    ///
    /// Carried per row even though the site pools by ruleset, because the two
    /// questions are different. "What do these rules produce" is what a reader
    /// wants; "which server said so" is what makes the answer checkable — a
    /// surprising drop can be traced to the log it came from, and a server
    /// added to the wrong ruleset can be found and moved without re-collecting
    /// anything.
    #[serde(default)]
    pub server: String,
    /// Which rules that server plays by. Resolved at export from
    /// `assets/servers.json`; "standard" when the server is not listed.
    #[serde(default)]
    pub ruleset: String,
    pub item: String,
    pub npc: String,
    /// Which grades this creature dropped, as (grade, times), lowest first.
    /// Grade 0 is an ungraded drop, and the counts sum to `looted`.
    ///
    /// A grade is a roll on the same item rather than a different item, so
    /// the drop is filed under the item's own name — but *which* rolls a
    /// creature produces is a fact about that creature and the only place it
    /// survives. Without this the fold is lossy: eight Adamantite Epaulets
    /// off a ghoul cavalier, with no way to know three were +1 and five +2.
    ///
    /// Empty for a contributor whose parser predates the fold, which is why
    /// it defaults rather than being required.
    #[serde(default)]
    pub grades: Vec<(u32, u64)>,
    /// Every zone the pairing was seen in. A creature is not always confined
    /// to one.
    #[serde(default)]
    pub zones: Vec<String>,
    /// Where those loots happened, and how many in each.
    ///
    /// `zones` is the set of places and `looted` one count for all of them,
    /// which is not an attribution: a black wolf dropped 832 Chunk of Meat
    /// across three zones as one number. These split it, and carry the
    /// instance tier as well, because `solo T1` and `instance T1` are
    /// different fights and `creatures` already keys kills that way.
    ///
    /// Empty for a row nobody has rescanned since this existed. That is not
    /// "the drop had no place" and must not read as one -- an absent
    /// breakdown is unknown, and the page says so rather than inventing an
    /// open-world tier 0.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<DropPlaceRow>,
    pub looted: u64,
    pub quantity: u64,
    pub first_seen: i64,
    pub last_seen: i64,
}

/// A raid, as somebody who has run it says it is made up.
///
/// Stated, not observed. Nothing in a log says a creature is a raid boss,
/// and the obvious proxy -- health -- catches trash in a raid zone: eleven
/// creatures clear 100,000 and they are not all encounters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RaidRow {
    pub expansion: String,
    /// Stated, because alphabetical would put Kunark before Classic.
    #[serde(default)]
    pub order: i64,
    pub raid: String,
    pub zone: String,
    /// Named by the bestiary's own spelling, so the join cannot miss.
    pub encounters: Vec<String>,
}

/// One class's armour set, stated rather than derived.
///
/// Which set belongs to which class could be read off a tooltip -- the item
/// says who can wear it -- and the pieces are in fact derived that way. What
/// cannot be derived is the set's *extent*: how many pieces a complete one is.
/// Counting that from the pieces on file would make "complete" mean "as much
/// as we happen to have", so a set showing four pieces because three were
/// never photographed would read as a four-piece set. The denominator has to
/// be asserted or the fraction means nothing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClassArmorRow {
    /// The three-letter class, as the tooltips spell it: `CLR`, not `Cleric`.
    ///
    /// One of this and `classes` is stated, not both. Sixteen of the eighteen
    /// sets belong to exactly one class and say so here.
    #[serde(default)]
    pub class: String,
    /// Every class that wears this set, where more than one does.
    ///
    /// Midnight Clad is worn by eight and Lustrous Russet by nine, and both
    /// are **one set** rather than eight and nine sets sharing a name — the
    /// pieces are the same items. Rogue is named by both, which is what the
    /// tooltips say rather than a mistake in this file.
    ///
    /// Expanded to one row per class on the way into the bundle, so the
    /// stored shape is unchanged and the page regroups on the set's name.
    #[serde(default)]
    pub classes: Vec<String>,
    /// The set's name, which is the prefix its pieces share -- `Ethereal
    /// Mist`, whose pieces are `Ethereal Mist Helm` and the rest.
    pub set: String,
    pub expansion: String,
    #[serde(default)]
    pub order: i64,
}

/// One place a drop was seen, and how often there.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DropPlaceRow {
    pub zone: String,
    /// `open`, `solo` or `instance`.
    pub mode: String,
    pub difficulty: u8,
    pub looted: u64,
    pub quantity: u64,
}

/// How many times a creature was killed — the denominator, travelling
/// separately because it is collected separately and is a floor of its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillRow {
    pub npc: String,
    pub kills: u64,
}

/// A recipe reconstructed from watching somebody make the thing.
///
/// This is a different kind of claim from a recipe book entry and is kept in
/// its own type rather than dressed up as one. The book says what a recipe
/// *is*; this says what was seen going in, across however many combines the
/// log happened to describe — and the client only announces components drawn
/// from a depot, so a combine worked out of your bags announces nothing at
/// all.
///
/// Hence `per_combine` being a float, which is the honest part. *Metal Bits*
/// shows `Small Piece of Ore ×2` cleanly because all 547 combines revealed
/// their inputs. *Banded Gorget* shows `Sheet Metal ×0.30` because only some
/// of its 95 combines did, and nothing distinguishes a complete observation
/// from a partial one. Rounding that to a whole number would turn a coverage
/// figure into a quantity and state a recipe nobody has actually established.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ObservedRecipe {
    pub product: String,
    #[serde(default)]
    pub skill: String,
    /// "observed" when the components were seen in the log, "recipe" when the
    /// figure was taken from the book because the log never revealed any.
    #[serde(default)]
    pub basis: String,
    /// Attempts, successes and failures alike — a failed combine eats its
    /// components too.
    pub combines: u64,
    /// Attempts whose components the log actually revealed. The denominator
    /// behind every `per_combine` below.
    pub costed: u64,
    #[serde(default)]
    pub successes: u64,
    #[serde(default)]
    pub failures: u64,
    /// How many of those attempts raised the skill.
    ///
    /// The number a person actually plans by: Electrum Malachite Bracelet
    /// took 54 skill-ups out of 72 attempts and Jaded Platinum Ring took 6
    /// out of 65, and only the counts tell them apart. It rode in the scan's
    /// `crafts` block from the beginning and had nowhere to go.
    #[serde(default)]
    pub skill_ups: u64,
    #[serde(default)]
    pub components: Vec<ObservedComponent>,
    /// Components seen going in that no purchase has ever priced.
    #[serde(default)]
    pub unpriced: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ObservedComponent {
    pub item: String,
    /// Average count per combine that announced *this component*.
    ///
    /// Not per combine that announced anything, which is what it used to be
    /// and what made *Banded Gorget* read `Sheet Metal ×0.30`. One combine
    /// can announce two of a recipe's three components and stay silent about
    /// the third, so dividing the third by the whole recipe's coverage
    /// measures how often that component came from a depot rather than how
    /// much of it a combine takes. A fraction now means the observations
    /// genuinely disagreed.
    pub per_combine: f64,
    /// Combines that announced this component, against the recipe's `costed`.
    /// Zero when the figure came from the book rather than from a combine.
    #[serde(default)]
    pub seen_in: u64,
}

/// One Alternate Advancement ability, as the catalogue describes it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AaAbility {
    pub name: String,
    /// "General", "Archetype", "Class" or "Special".
    #[serde(default)]
    pub category: String,
    /// Empty unless `category` is "Class".
    #[serde(default)]
    pub class: String,
    /// The expansion the ability arrived in, as the AA window's own Category
    /// column names it. A different fact from `category` above, which is the
    /// window's tab; the two share a word and nothing else.
    #[serde(default)]
    pub expansion: String,
    pub ranks: u32,
    /// The cost of each rank, in order.
    ///
    /// `None` is a rank nobody has established — the wiki writes those as
    /// "?" — and is emphatically not zero. Zero is itself a real cost,
    /// meaning the rank is granted rather than bought, and the two must not
    /// collapse into each other: eqldb can sometimes fill an unknown from its
    /// own observations, which only works while the unknown is still visible.
    #[serde(default)]
    pub costs: Vec<Option<u32>>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub requirements: String,
    /// The game's own id for the ability, where a client dump supplied one.
    ///
    /// The wiki knows abilities only by name, which makes two abilities that
    /// share a name into one row and loses a renamed one entirely. The client
    /// prints `Ability #119`, which is what the ability actually is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ability_id: Option<u32>,
    /// What each rank does, in the client's own words.
    ///
    /// The wiki has one sentence for a whole ability; the client describes
    /// every rank separately, and the difference between them is the point —
    /// Mental Clarity restores one point of mana at its first rank and four
    /// at its fourth.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rank_descriptions: Vec<String>,
}

/// What the parses saw of one ability at one rank.
///
/// This is the part derived from play, and it is aggregated past the player
/// on purpose. The parser records who bought what and when; a public
/// reference has no use for that and no business publishing it. What survives
/// is the figure and how many characters reported it — enough to show that
/// two characters disagree, without saying which.
///
/// `invested` is *cumulative*: the running total sunk into the ability
/// through this rank, not the price of the rank itself. That is what the
/// parser records, and reading it as a rank price gets the arithmetic wrong.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AaObserved {
    pub ability: String,
    pub rank: u32,
    pub invested: u32,
    pub observations: u32,
}

/// An ability the client lists in its Actions window.
///
/// Read off the tooltip, because it is nowhere else: not in `dbstr_us.txt`,
/// not in `eqstr_us.txt`, not printed to the log. A screenshot is the only
/// route, the same as an item's numbers.
///
/// Nine of these share a name with a skill — Bash, Kick, Begging, Bind Wound —
/// and are shown on that skill's page. The rest are abilities in their own
/// right: stances, invocations, and the small commands like Camp and Origin.
///
/// `classes` is durable and the roster is not. The tooltip lists every class
/// that can use the ability regardless of who is looking — Bash reads
/// `WAR CLR PAL SHD` on a character that is neither a cleric nor a paladin —
/// but *which rows appear at all* depends on the classes being run, so a
/// capture is a snapshot of one build. The same shape as the alternate
/// advancement Class tab.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ability {
    pub name: String,
    /// The tooltip's own heading, carried only where it differs from the row.
    ///
    /// It frequently does, and not by truncation: the row says `Melee` and
    /// the tooltip says `Attack`, the row says `Empower` and the tooltip says
    /// `Empowering Invocation`. Every invocation and both combat stances do
    /// this. The row's name is what the client's own list calls it, so that
    /// is the name; the heading is kept beside it rather than chosen between.
    #[serde(default)]
    pub tooltip_title: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub classes: Vec<String>,
    #[serde(default)]
    pub reuse_seconds: Option<i64>,
    /// The tooltip's wording, kept beside the parsed seconds so a
    /// misreading stays recoverable.
    #[serde(default)]
    pub reuse_text: String,
    /// The table column's wording, which formats the same value differently:
    /// `1m 0s` in the column against `Reuse: 1m` in the tooltip.
    #[serde(default)]
    pub reuse_column: String,
    /// One entry per paragraph, as the tooltip breaks them.
    #[serde(default)]
    pub description: Vec<String>,
    #[serde(default, rename = "macro")]
    pub macro_command: String,
    /// Why a row has no tooltip — the window was never hovered over it.
    #[serde(default)]
    pub note: String,
}

/// How high a skill has been seen to go.
///
/// A floor on the cap, not the cap. Somebody reached this value, so the game
/// permits at least it; whether it permits more is not in a log. Same shape
/// as every drop count here — "looted 47" bounds how often a thing drops
/// without claiming a rate.
///
/// What does not travel is how often anybody practised or who they were. A
/// skill's ceiling is a fact about the game; one character's progress towards
/// it is progression, and is left in the log with the levels.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillObserved {
    pub skill: String,
    pub at_least: i64,
}

/// A spell the game handed over, aggregated past the player.
///
/// EverQuest Legends grants spells outright rather than selling every one of
/// them as a scroll, and the client's spell file does not say which. A grant
/// is the game itself answering three questions the site otherwise has to
/// guess at from outside: the spell is real, it is reachable, and it costs
/// nothing to get.
///
/// `level` is the level gained at that moment, or 0 when none was. Both are
/// grants and both are worth having, but they say different things. With a
/// level: *this spell is given at this level*. Without one: the grant came
/// from a change of invocation, which hands over everything the new classes
/// already qualify for, so it says the spell is granted and nothing about
/// when. The two are kept as separate rows rather than merged.
///
/// What does not travel is who was granted it, when, or what they were
/// playing. `observations` is how many times it was seen, which is enough to
/// show that a level is well attested without naming a character.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpellGrant {
    pub spell: String,
    #[serde(default)]
    pub level: u32,
    #[serde(default)]
    pub observations: u32,
}

/// Coin paid for one hand-in, as the ends of what was actually seen.
///
/// In copper, which is the only unit that adds up; the page splits it back
/// into denominations. Two ends and a count rather than an average: an
/// average of five payments that ranged from 152 to 662 is a number the game
/// never paid, and it would be read as the one it always pays.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoinReward {
    /// The smallest payment seen.
    pub low: u64,
    /// The largest.
    pub high: u64,
    /// How many payments those two are the ends of. Recorded rather than
    /// shown: two payments that differ prove the coin varies and five say
    /// more about how widely, which is worth keeping even though the page
    /// prints the range on its own.
    #[serde(default)]
    pub seen: u64,
}

/// A quest walkthrough: how to start it, what it wants, what it gives.
///
/// This is the one type eqldb has to declare for itself rather than borrowing
/// from the parser. The parser's `QuestRecord` lives in `eqlparserd`, which is
/// a binary with no library target, so there is nothing to depend on. Field
/// names match so the JSON deserialises straight across, and every field is
/// `#[serde(default)]` so a parser that grows a new one does not break an
/// older reader.
///
/// The mismatch is deliberate in one direction: `QuestRecord` also carries
/// `status` — whether *this player* has finished it. That is progress, not
/// reference, and a public site has no business publishing whether someone
/// has done a quest yet. It is dropped on export rather than carried and
/// hidden.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Quest {
    pub name: String,
    #[serde(default)]
    pub giver: String,
    #[serde(default)]
    pub zone: String,
    /// Classes it applies to. Empty means any.
    #[serde(default)]
    pub classes: Vec<String>,
    /// How to start it — the hail, the keyword, the prerequisite.
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub items: Vec<String>,
    #[serde(default)]
    pub reward: String,
    /// What the coin actually came to, where somebody watched it more than
    /// once.
    ///
    /// Separate from `reward` because a single figure in the sentence reads
    /// as the price. Tiam Khonsir paid five different amounts for five
    /// identical hand-ins, and the honest form of that is the two ends and
    /// the count they came from.
    #[serde(default)]
    pub reward_coin: Option<CoinReward>,
    /// The set a reward is one piece of, picked by the game at random.
    ///
    /// Not the same as `reward` naming two items with a comma between them:
    /// that is a choice the player makes, and this is a roll they do not see.
    #[serde(default)]
    pub reward_one_of: Vec<String>,
    #[serde(default)]
    pub notes: String,
    /// Quest text as the client printed it.
    #[serde(default)]
    pub dialogue: Vec<QuestLine>,
    /// Where the *description* came from: "manual", "task" or "sky".
    #[serde(default)]
    pub source: String,
    /// Whether somebody has run this walkthrough in game and found it right.
    ///
    /// Set by hand by whoever checked it, and false until then. It is not
    /// derivable: a quest imported from the client's task window is accurate
    /// about what the game tracks and says nothing about whether the steps
    /// underneath it are the ones that work, and a captured dialogue proves
    /// only that somebody hailed the giver.
    ///
    /// So this is a person's word, which is why it is worth showing — the
    /// page already says where a description came from, and this says whether
    /// anybody has tested it.
    #[serde(default)]
    pub verified: bool,
    /// The achievement this quest completes, by name, or empty.
    ///
    /// A transcriber's claim like the steps and the reward, not something a
    /// log states — but a checkable one: the name has to match an achievement
    /// in the client's own table for the page to show it.
    #[serde(default)]
    pub achievement: String,
    /// Whether the game tracks this as a task.
    ///
    /// A quest and a task are the same thing seen from two sides — the task
    /// system is how the modern client tracks a quest. Older quests are
    /// driven entirely by talking to somebody and never appear in it, so the
    /// absence of a task is a fact about the quest's vintage rather than a
    /// hole in the record, and the page says which it is.
    #[serde(default)]
    pub task: bool,
    /// A task that resets weekly rather than being done once.
    #[serde(default)]
    pub weekly: bool,
    #[serde(default)]
    pub updated: i64,
}

/// What an item's own tooltip says about it.
///
/// Nothing in a log states an item's stats, so this is the one part of eqldb
/// that comes from somebody looking at the game and writing down what they
/// saw. That makes it the weakest evidence here and it is labelled as such —
/// `source` and `evidence` travel with every record so a surprising number
/// can be traced back to the screenshot it was read from and checked.
///
/// **Values are as observed at `tier`, never converted to a base.** A +10
/// weapon's numbers are not the item's base numbers, and while the grade rule
/// could in principle be inverted, doing so silently would publish an
/// inference as a measurement. A base-tier reading is simply worth more, and
/// where both exist the two together test the rule instead of assuming it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ItemFacts {
    /// The name as the tooltip shows it, grade included.
    pub item: String,
    /// Where the tooltip was read. An item's flags are not universal: the same
    /// item is NO DROP on a standard server, tradeable on Firiona Vie, and on
    /// Lethar its class and race lines say ALL. A reading with no server is a
    /// reading nobody can place.
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub ruleset: String,
    /// The grade these values were observed at. 0 is an ungraded base item.
    #[serde(default)]
    pub tier: u8,
    /// How this was established: "screenshot" today.
    #[serde(default)]
    pub source: String,
    /// Whether the client's "Unmodified" box was ticked when the screenshot
    /// was taken.
    ///
    /// The item window shows an item as *this character* sees it unless that
    /// box is ticked, and the box is off by default. So a reading may carry
    /// the observer's own alternate advancement and heroic modifiers baked
    /// into the numbers, which is the one way a tooltip transcription can be
    /// faithful and still wrong.
    ///
    /// `None` means nobody recorded which state it was in, which is what
    /// every reading taken before this field existed honestly is. It is not
    /// `false`: a reading that might be unmodified is not the same as one
    /// known to be modified, and collapsing the two would turn an open
    /// question into a false claim.
    ///
    /// Augments, at least, are not the problem: all eight exaltation tooltips
    /// on file grant an effect and no stat, so nothing slotted into an item
    /// has been seen to move its numbers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unmodified: Option<bool>,
    /// What to look at to check the reading — a screenshot filename.
    #[serde(default)]
    pub evidence: String,
    /// A scroll's `Level Needed` line, as the window prints it: `ENC(43)`, or
    /// `CLR(1) PAL(2) SHM(1) BST(5)` where several classes learn it.
    ///
    /// Load-bearing, not a copy of the spell's own class list. 17 of the 99
    /// scroll readings name a spell that matches more than one row in
    /// `spells_us.txt` — `Illusion: Skeleton` matches two ids — and this line
    /// is what says which. Kept as the window wrote it, since parsing it is
    /// the reader's problem and a failed parse should not lose the evidence.
    /// Which half of a split name this reading is.
    ///
    /// Empty on every reading but the handful that need it. The client prints
    /// one name for two items — `Bixie Stinger` is ids 14029 and 20994 — so
    /// the tooltip itself cannot say which was photographed, and a reading
    /// filed on the name alone lands on whichever page the name resolves to.
    ///
    /// This is the transcriber saying **where they looted it**, which is a
    /// fact they have and the screenshot does not. It is kept apart from
    /// `item` on purpose: that field holds the client's own spelling, and
    /// typing `Bixie Stinger (Sky)` into it would put the site's word for
    /// something into the field that records the game's.
    ///
    /// Must match a name in `store::ITEM_SPLITS` or it is ignored — a
    /// disambiguator nobody can resolve is worse than none.
    #[serde(default)]
    pub split_as: String,
    #[serde(default)]
    pub level_needed: String,
    /// The description the window printed.
    ///
    /// Usually the client's own file says the same thing, and 68 of 88
    /// compared exactly with zero disagreements. It is kept for the 18 where
    /// the file holds an unresolved template — "causing between #1 and @1
    /// damage" — and the 2 whose spell is not in the file at all.
    #[serde(default)]
    pub description: String,

    /// "Lore Equipped", "No Trade", "Placeable", "Quest" and friends.
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub classes: Vec<String>,
    #[serde(default)]
    pub races: Vec<String>,
    /// "Primary", "Primary Secondary", and so on.
    #[serde(default)]
    pub slot: String,
    #[serde(default)]
    pub size: String,
    #[serde(default)]
    pub weight: Option<f64>,
    #[serde(default)]
    pub skill: String,

    #[serde(default)]
    pub base_dmg: Option<u32>,
    #[serde(default)]
    pub delay: Option<u32>,
    #[serde(default)]
    pub dmg_bon: Option<u32>,
    /// The tooltip prints this, and it must equal `base_dmg / delay`. That
    /// redundancy is the whole reason it is carried: see
    /// eqldb-core's `items::check_ratio`.
    #[serde(default)]
    pub ratio: Option<f64>,

    #[serde(default)]
    pub stats: Vec<ItemStat>,
    /// Focus, click, proc and combat effects, as printed.
    #[serde(default)]
    pub effects: Vec<String>,
    /// What is slotted into the item's exaltation sockets.
    ///
    /// Kept apart from `effects` because an exaltation is removable: the
    /// augment can be taken out and put in something else, so the effect it
    /// grants belongs to the augment and not to the item holding it. An
    /// effect listed on a weapon may be the weapon's own or may be on loan,
    /// and the two are different claims.
    /// What the item is socketed with on the copy that was photographed.
    ///
    /// Not a property of the item, and deliberately not shown as one. 65 of
    /// the 91 socketed readings hold the item's own `(Exaltation)` augment,
    /// which is what an item spawns with; the other 26 are somebody's choice
    /// -- Gilded Cloth in Adamantite Epaulets, White Dragonscale Cloak in
    /// Cloak of Flames. hammackj: "I can put custom ones in, typically an
    /// item won't spawn with things not named the same as the item."
    ///
    /// So this is the same class of fact as the grade and as attunement: true
    /// of one copy on one day. `augment_slots` beside it is the item's own.
    #[serde(default)]
    pub augments: Vec<Augment>,
    /// Which augment slots the item has, as the tooltip lists them.
    ///
    /// Intrinsic, unlike `augments`: the slots are the item's, whatever is or
    /// is not sitting in them. 618 readings carry this.
    ///
    /// Observed rather than projected, which is the point of storing it.
    /// `js/grade.js` computes a socket list from the tier and has to, because
    /// nothing was on file to say otherwise; it gets `min(tier, 4)` right on
    /// 315 of 318 graded readings and is wrong in two ways it could not know
    /// about. Three readings are short by one -- probably incomplete
    /// transcriptions -- and, more usefully, the projection has never heard
    /// of **Ornamentation**, which is a fifth kind present on 498 readings
    /// including 298 at tier 0. An item with no upgrade sockets at all still
    /// has that one.
    #[serde(default)]
    pub augment_slots: Vec<String>,
    /// The item this one turns into, where the window names one.
    ///
    /// A *transformation*, not an upgrade: on both pairs on file each side
    /// names the other, so there is no "before" and "after" to model. Stored
    /// as the tooltip printed it and resolved to a page at render time.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub converts_to: String,
    /// Readable text, for a note or a book. The client shows it in a window
    /// of its own and never writes it to the log, so a screenshot is the only
    /// way it can be recorded at all.
    #[serde(default)]
    pub note: String,
    /// Whether the item's window **has** a Lore tab.
    ///
    /// Not every item does, and until this was recorded nothing could tell
    /// "nobody has photographed the lore" from "there is no lore to
    /// photograph". Every item with a reading and no lore looked like a gap,
    /// which made a worklist of 1,704 items where the real one is around 50.
    ///
    /// `None` means no tab row was read -- an unreadable window, or an older
    /// reading taken before this was recorded. It is emphatically not
    /// `Some(false)`: absence is what stops an unread window being treated as
    /// evidence that an item has no lore.
    ///
    /// `parse.py` counts the words on the tab row rather than decoding them.
    /// The unselected tab is white and small and does not survive the reader
    /// -- `Lore` beside a selected `Description` comes back as `pg` at 0.51 --
    /// but the row's *shape* is unambiguous: two entries in the tab
    /// vocabulary, the selected one identified by its highlight, so a second
    /// word is the second tab. Measured over the crops on file: 25 of 25
    /// items whose Lore tab has been shot carry two words there, and 182 of
    /// 220 items with a reading and no lore carry one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_lore_tab: Option<bool>,
    /// The item's Lore tab, where it has one.
    ///
    /// A separate field rather than more prose in `note`, because it is a
    /// separate tab in the client and a separate screenshot to capture: the
    /// Description tab does not show it, so an item can have stats recorded
    /// and lore still missing. Empty means nobody has photographed the tab,
    /// not that the item has no lore.
    #[serde(default)]
    pub lore: String,
    #[serde(default)]
    pub value_copper: Option<u64>,
    /// The Value line as the window printed it — "3 platinum 4 gold 2 silver
    /// 9 copper", or "absolutely nothing" for an item worth none.
    ///
    /// `value_copper` beside it is the same fact in one number, and this is
    /// what it was read from. Kept because a transcription that disagrees
    /// with its own arithmetic is a thing worth being able to see: one
    /// reading on file states 0 copper against a printed 142 platinum.
    #[serde(default)]
    pub value_text: String,

    /// The window's `Type:` line — "Shield" and friends.
    ///
    /// Named `type_line` rather than `type` on purpose: a batch once carried
    /// `type` holding this, and a `type` field meaning item *category* would
    /// have absorbed it silently. See `extra` below.
    #[serde(default)]
    pub type_line: String,
    /// Whether the window says the item can be graded up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upgradeable: Option<bool>,
    /// The upgrade counter as printed — "10 / 10", "0 / 1".
    #[serde(default)]
    pub tier_progress: String,
    /// The window's own `Tier` line, where it disagrees with the grade in
    /// the name.
    ///
    /// The site splits variants on the name, so that is what `tier` follows.
    /// This records what the window actually said, because a disagreement
    /// between the two is evidence about one of them and discarding it means
    /// discovering it again later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_observed: Option<u8>,
    #[serde(default)]
    pub tier_note: String,

    /// A bag's slot count, the size of item it takes, and what it saves.
    ///
    /// `container` was held out of this struct while 11 readings said
    /// `"CLOSED"` and one said `true` — either the container's state or
    /// whether the item is one, and no field could be added until somebody
    /// said which. The booleans were a capture-side bug and are gone: all 19
    /// readings now state the printed word, on exactly the 12 items that
    /// also state a capacity. Carried verbatim, like `type_line` and
    /// `value_text` beside it, because it is a line the window printed and
    /// interpreting it is not this struct's job.
    #[serde(default)]
    pub container: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<u32>,
    #[serde(default)]
    pub size_capacity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight_reduction: Option<u32>,

    /// Damage the window states separately from `base_dmg`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backstab_dmg: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fire_dmg: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cold_dmg: Option<u32>,
    /// A bow or thrown weapon's range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<u32>,

    /// Deities the item is restricted to, as `classes` and `races` are.
    #[serde(default)]
    pub deities: Vec<String>,
    /// What the item's sockets accept, where the window says so.
    #[serde(default)]
    pub socket_note: String,
    /// An effect on the item that belongs to something slotted into it.
    ///
    /// The counterpart to `augments`, which says what is in the sockets:
    /// this says what those things are granting. Both describe the copy that
    /// was photographed rather than the item, which is why the effect is
    /// named with its source instead of being folded into `effects`.
    #[serde(default)]
    pub granted_effects: Vec<GrantedEffect>,

    /// Everything the sender carried that this struct has no field for.
    ///
    /// Serde drops unknown keys silently, which is right for tolerating a
    /// newer transcription pass against an older binary — but it makes a
    /// whole class of mistake invisible. The transcription is written in one
    /// repository and this struct in another, and nothing checks the names
    /// against each other. A field added here later, with a name a batch is
    /// already using for something else, would start absorbing that data with
    /// different semantics and no error anywhere.
    ///
    /// That is not hypothetical. A batch carried `type` holding the string
    /// `"Shield"` — the window's `Type:` line, sitting where a weapon prints
    /// `Skill:`. A `type` field added here would almost certainly have meant
    /// item category, would also have been a `String`, and would have
    /// deserialised it without complaint. It was renamed to `type_line`
    /// before either happened, but only because somebody listed one artifact
    /// against the other by hand.
    ///
    /// Capturing them here means the export can say what arrived that it did
    /// not understand, so the set is known rather than discovered later. It
    /// stays in step with the struct by construction: a field added above
    /// stops appearing below, with nothing to remember to update.
    /// Read but not written. Carrying them onward would mean a field added
    /// here later picks them up out of *old bundles* as well as new batches,
    /// which is the collision this exists to make visible rather than a
    /// second place for it to happen.
    #[serde(flatten, default, skip_serializing)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

impl ItemFacts {
    /// Whether this is a reading of the item's **Lore tab and nothing else**.
    ///
    /// The Lore tab is a separate tab of the Item Display window carrying one
    /// field, so a photograph of it is a reading of that field -- not of the
    /// item. Treating it as a reading of the item is destructive, because
    /// `ingest` keys `item_facts` on `(contributor, base_key, tier)` and its
    /// `ON CONFLICT` sets every column from the incoming row: a Lore tab
    /// arriving for an item already on file blanked its slot, size, flags and
    /// classes and left the lore behind. Reproduced in
    /// `a_lore_reading_does_not_blank_the_stats_already_on_file`, which also
    /// says which bundles can reach it -- not the OCR pipeline's, whose fold
    /// runs over the whole corpus, but one carrying tooltip rows for a source
    /// it does not declare.
    ///
    /// # Why this is a comparison and not a list of field names
    ///
    /// Because a list would rot. `finalize_all.py` and `emit_itemfacts.py`
    /// already carry two separate allowlists over this same field set, and
    /// the second one says in its own comment what happens when they drift:
    /// `fire_dmg` was added to one and not the other, and the Champion Arrows
    /// carried 3 and 4 through finalize and arrived as nothing, silently. A
    /// third list here would fail the same way, and it would fail *open* --
    /// a new field omitted from it would make a full reading look lore-only
    /// and turn a correction into a wipe.
    ///
    /// So this compares against a copy of itself carrying only the fields
    /// that say *which reading this is* rather than *what the item is*:
    /// identity (`item`, `split_as`, `tier`), provenance (`server`,
    /// `ruleset`, `source`, `evidence`) and the lore. Anything else that is
    /// set -- including a field added to this struct tomorrow -- makes it a
    /// reading of the item, which is the safe answer.
    pub fn lore_only(&self) -> bool {
        if self.lore.trim().is_empty() {
            return false;
        }
        *self
            == ItemFacts {
                item: self.item.clone(),
                split_as: self.split_as.clone(),
                tier: self.tier,
                server: self.server.clone(),
                ruleset: self.ruleset.clone(),
                source: self.source.clone(),
                evidence: self.evidence.clone(),
                lore: self.lore.clone(),
                // A Lore window states this -- it is looking at the tab, so
                // the tab exists -- and it is still a reading of that tab
                // alone. Left out of this copy it would make every Lore
                // window look like a reading of the item, which is the wipe
                // this method exists to prevent.
                has_lore_tab: self.has_lore_tab,
                // Carried across for the opposite reason to `has_lore_tab`
                // above, and this one was measured rather than reasoned.
                //
                // `extra` is every key the sender wrote that this struct has
                // no field for -- `crops`, `checkbox_label`, `captured_first`,
                // `slots`. It is a diagnostic, not a reading: it exists so
                // that a name added here tomorrow is seen to collide with an
                // old bundle's spelling. Left out of this copy, any of them
                // being present makes a Lore window look like a reading of
                // the whole item.
                //
                // Which is what was happening. All 2,280 entries in
                // `item-facts.json` carry at least one -- `crops` is on every
                // one -- so `lore_only` answered false for every real reading
                // ever written, including the 191 with lore, and had only
                // ever answered true for structs built in a test. It went
                // unnoticed because the ingest cleared `item_facts` first and
                // there was nothing on file to blank; the moment the item
                // family stopped being cleared, this became the wipe the
                // method exists to prevent.
                //
                // It also fails in the better direction now. A reading
                // carrying lore and nothing but unrecognised keys is merged
                // rather than substituted, and a merge that should have been
                // a replacement loses nothing.
                extra: self.extra.clone(),
                ..Default::default()
            }
    }
}


/// An effect a slotted augment is granting to its host.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GrantedEffect {
    /// The effect line as printed.
    pub effect: String,
    /// The augment granting it.
    #[serde(default)]
    pub by: String,
    /// Which socket that augment is in.
    #[serde(default)]
    pub slot: String,
}

/// One exaltation socket and what is in it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Augment {
    /// "Focus", "Click", "Worn", "Proc", "Ornamentation".
    pub slot: String,
    /// The augment's name as printed. Empty sockets are not recorded.
    pub item: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ItemStat {
    pub name: String,
    pub value: i64,
    /// The heroic half, where the tooltip shows one: EverQuest writes
    /// "Strength: 42 +19", two numbers for one line. Legends has no such
    /// thing, so this is absent on every reading taken there rather than
    /// zero — a zero would claim the tooltip said "+0", which it did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heroic: Option<i64>,
}

/// What killing one creature does to one faction.
///
/// Attribution is possible because the client prints faction adjustments
/// *before* the death they belong to, so a run of them is assigned to the
/// death that follows. Bursts with no death after them — a turn-in, or a kill
/// whose death line never printed — are dropped rather than guessed at, which
/// is why `kills` here is a floor on how often the creature was actually
/// killed and never a total.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FactionHit {
    pub creature: String,
    /// Where it was killed. Part of the key, not a decoration: an orc
    /// centurion in West Commonlands moves Freeport's factions and one in
    /// Crushbone moves Faydwer's, and they share nothing but a name.
    #[serde(default)]
    pub zone: String,
    pub faction: String,
    /// Every distinct adjustment seen, with how often. A creature whose hit
    /// never varies has one entry; one that does keeps both, because an
    /// average of two amounts is a number nobody was ever awarded.
    #[serde(default)]
    pub adjustments: Vec<FactionAmount>,
    /// Attributed kills behind this pairing.
    pub kills: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FactionAmount {
    pub amount: i64,
    pub times: u64,
}

/// What handing one item to one NPC does to one faction.
///
/// The other half of faction, and the half a quest page is actually asked
/// about. Killing is not the only way standing moves, and a walkthrough that
/// lists two acceptable turn-in items is useless without the thing that
/// separates them: a Metal Bit moves New Sebilisian Expedition by 5 and a
/// Small Piece of High Quality Ore by 10, so the ore is worth twice the trip.
///
/// Keyed on the item and the NPC rather than on the quest. The same item
/// handed to two people is two different bargains, and an item's worth is a
/// fact about the item — it stays true for a quest nobody has written up yet.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnIn {
    pub npc: String,
    pub item: String,
    pub faction: String,
    /// Every distinct adjustment seen, with how often — the same rule as
    /// [`FactionHit`]. **This is the durable part.** The amount per
    /// acknowledgement is a property of the exchange and does not depend on
    /// how much anybody happened to hand over.
    #[serde(default)]
    pub adjustments: Vec<FactionAmount>,
    /// How many items went across.
    pub offered: u64,
    /// In how many gestures. One offer of four hundred is not four hundred
    /// offers, and the two numbers answer different questions.
    #[serde(default)]
    pub handins: u64,
}

/// What considering a creature reported.
///
/// The consider line is the only place a log states a creature's level —
/// nothing in its death, its loot or its melee carries one. It also marks
/// rare spawns, and that mark is asymmetric evidence: seeing it proves a rare
/// version exists, while not seeing it proves only that nobody has met one.
///
/// The attitude in the same line is deliberately absent. Whether something
/// scowls at you or regards you indifferently is your faction standing, not a
/// property of the creature, and would read differently for someone else.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Consider {
    pub creature: String,
    /// Every level seen, with how often. One name can cover spawns at
    /// several, and a mean of 27 and 31 describes neither.
    #[serde(default)]
    pub levels: Vec<ConsiderLevel>,
    pub rare: bool,
    /// The client said it would take an army to defeat.
    ///
    /// A different mechanism from `rare`, not a stronger version of it. Rare
    /// is an interjection inside the consider line; this is a line of its
    /// own, immediately after and sharing its timestamp to the second:
    ///
    /// ```text
    /// Eye of Veeshan regards you indifferently -- ... (Lvl: 70)
    /// This creature would take an army to defeat!
    /// ```
    ///
    /// Nothing in the logs carries both marks, and Cazic-Thule cons with no
    /// interjection at all — so a raid target is invisible to anything
    /// looking where the rare mark lives.
    #[serde(default)]
    pub raid: bool,
    pub considers: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsiderLevel {
    pub level: u32,
    pub times: u64,
}

/// A zone the game has, whether or not anybody has walked into it.
///
/// The client ships 186 map files and they are EverQuest's. Nothing else here
/// enumerates the zones *Legends* holds: a zone reaches the database when
/// somebody enters it, so the site knew 30 and had no way to say whether that
/// was a fifth of the game or a fiftieth.
///
/// The achievement table answers it. Legends gives one `<Zone> Traveler` per
/// zone, and the client's own table lists the ones nobody has earned — 140
/// against the 92 in a window dump. That is the game's list rather than
/// anybody's guess.
///
/// It is a roster of *names*, and deliberately not a pairing to a map: a map
/// is named by its archive shortname and nothing in the client pairs the two.
/// See `tools/import-zone-roster.py`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RosterZone {
    pub zone: String,
    /// The game gives a `Hunter of <Zone>` for it, so it has a bestiary.
    /// 77 of the 140 do; the rest are cities, guild halls and housing.
    #[serde(default)]
    pub bestiary: bool,
    /// Which landmass, from the `<Continent> Explorer` achievement that
    /// requires this zone's Traveler. "Other" where no Explorer names it,
    /// which is Kunark and Velious — the game has no Explorer for either,
    /// so grouping them anywhere else would be geography nothing states.
    #[serde(default)]
    pub continent: String,
    /// Whether the game names this zone with a Traveler achievement.
    ///
    /// False for one the logs name and no achievement does — `Clan
    /// Crushbone` where the roster says `Crushbone`, or `New Sebilis
    /// Expedition`, which has no achievement of any kind. Kept so "what the
    /// game lists" stays separable from "what somebody added".
    #[serde(default = "yes")]
    pub from_achievement: bool,
}

fn yes() -> bool {
    true
}

/// Which of the client's icons an item shows.
///
/// Observed, not borrowed. The number used to come from the community item
/// catalogue, and `985e31e` removed that catalogue because its numbers were
/// live EverQuest's rather than this game's — the icon column went with it and
/// every item page has rendered blank since. No client file this project reads
/// supplies one, and an inventory dump gives the game's item id, which is a
/// different number.
///
/// So it is read off the item's own tooltip: the window draws the icon, the
/// screenshots are on disk, and the client's icon sheets are on disk, so the
/// pairing can be established by finding which sheet icon the window is
/// showing. `tools/match-item-icons.py` does that and this carries the result.
///
/// That makes an icon the same kind of claim as the tooltip readings beside
/// it — taken from this game and checkable against the pixels it came from —
/// rather than another game's catalogue believed because it agreed the first
/// eighty-three times.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ItemIcon {
    pub item: String,
    /// The client's flat icon number, which starts at 500.
    pub icon: u32,
    /// How well the window's icon matched this sheet icon, 0 to 1.
    #[serde(default)]
    pub score: f64,
    /// How far clear of the next-best sheet icon it was.
    ///
    /// Carried because the score alone does not say whether a match was a
    /// call or a coin toss: armour sets have near-identical icons, so a
    /// reading can score 0.95 against two of them and mean nothing, while a
    /// 0.75 that beats everything else by 0.38 is certain.
    #[serde(default)]
    pub gap: f64,
    /// The crop it was read from, so a wrong icon can be traced to a picture.
    #[serde(default)]
    pub evidence: String,
}

/// How a creature comes back after it dies.
///
/// Transcribed, not parsed. Nothing in a log states a respawn timer or a
/// lockout — a death line says a thing died and the next one says it died
/// again, and the gap between them is when somebody happened to be standing
/// there. So this travels like a quest walkthrough does: somebody asserts it
/// and says how they know.
///
/// # Why a kind and not a duration
///
/// The obvious model is a `respawn_seconds` on the creature, and it is wrong
/// here, because Legends has two unrelated mechanisms and only one of them is
/// a duration:
///
///   * Ordinary creatures are on a timer, and in Legends it is about six
///     minutes near enough across the board.
///   * Raid targets are not on a timer at all. They are instanced, on a
///     weekly lockout that resets Tuesday, with one daily kill on top. There
///     is no number of seconds that describes that, and writing one down —
///     even a large one — states something false.
///
/// A single duration field forces the second kind to lie or to be null, and a
/// null reads as "nobody has measured it" rather than "the question does not
/// apply". Hence [`SpawnKind`].
///
/// # Why there is usually no number
///
/// `seconds` is for a creature that differs from the ordinary timer, not for
/// every creature that has one. The six minutes is a fact about the game, and
/// stamping it onto eleven hundred creature rows would manufacture eleven
/// hundred observations out of one rule — the same mistake as believing an
/// item catalogue because the first eighty-three entries agreed. The default
/// lives once, in the registry's header, and a row carries a number only
/// where somebody has actually established a different one.
///
/// # The wiki is not a source for this, ever
///
/// eqlwiki's `respawn_time` holds *classic EverQuest* values, which are a
/// different game: Efreeti Lord Djarn is listed at 22 minutes against an
/// actual 6. Worse, some pages staple the timer into free text where nothing
/// is watching for it — Drelzna arrived carrying `Respawn: 18.5 minutes` on
/// the end of a Harmtouch entry. The standing rule downstream is that a wiki
/// respawn number is wrong for Legends *whichever field it arrives in*,
/// including one that looks plausible, and [`SpawnRule::evidence`] exists so
/// that a row which cannot say where it came from can be spotted rather than
/// trusted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpawnRule {
    pub creature: String,
    pub kind: SpawnKind,
    /// How long the timer is, where it is *not* the ordinary one. `None`
    /// means the registry's default applies, not that nobody knows.
    #[serde(default)]
    pub seconds: Option<u32>,
    /// How the lockout resets, for an instanced target.
    #[serde(default)]
    pub lockout: Option<Lockout>,
    /// How this was established — who timed it, or where it is written down.
    /// A row with nothing here is a claim nobody can check.
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub note: String,
}

/// Which of the game's two mechanisms brings a creature back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpawnKind {
    /// On a timer. The registry's default unless `seconds` says otherwise.
    #[default]
    Timed,
    /// In an instance, on a lockout. `seconds` is meaningless here.
    Instanced,
    /// Does not respawn because it does not die: a quest giver, a merchant,
    /// anything that stands where it stands. Distinct from `Timed` with an
    /// unknown number, and the honest answer for the seven quest NPCs on
    /// EQLDB-46 that are on the list because a wiki page exists for them and
    /// not because anybody should kill one.
    Static,
    /// Nobody has established which of the above applies. Written down rather
    /// than defaulted, so "we have not looked" and "it is on the ordinary
    /// timer" stay different answers.
    Unknown,
}

impl SpawnKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SpawnKind::Timed => "timed",
            SpawnKind::Instanced => "instanced",
            SpawnKind::Static => "static",
            SpawnKind::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> SpawnKind {
        match s.trim().to_lowercase().as_str() {
            "timed" => SpawnKind::Timed,
            "instanced" => SpawnKind::Instanced,
            "static" => SpawnKind::Static,
            _ => SpawnKind::Unknown,
        }
    }
}

/// When an instanced target becomes killable again.
///
/// Two independent allowances, which is why they are two fields. The weekly
/// lockout is the one that gates the real kill; the daily is an extra on top
/// and does not reset the weekly. A reader who is told only one of them plans
/// the wrong week.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockout {
    /// The weekday the weekly lockout resets on, 0 = Sunday. Tuesday in
    /// Legends. Stored as a day rather than a duration because that is what
    /// it is: it does not reset a week after your kill, it resets on Tuesday.
    #[serde(default)]
    pub resets_weekday: u8,
    /// Kills allowed per day on top of the weekly one. Zero means none.
    #[serde(default)]
    pub daily_kills: u32,
}

/// An item paired with the game's own identifier for it.
///
/// The game's own id for an item, and the first key here that is not a name.
///
/// Everything else is matched on names, which vary by article, case and grade
/// suffix; an id does not. It is grade independent — a base and a +4 share one
/// — which is direct confirmation that a grade belongs to a copy rather than
/// to an item, the assumption the whole base-name model rests on.
///
/// # It identifies a family, not a thing
///
/// This used to be called "canonical", and that was too strong. An id is also
/// shared between an item and its `(Exaltation)` augment: Shield of the
/// Immaculate and Shield of the Immaculate (Exaltation) are both 11551, and
/// 42 pairs on file are like that.
///
/// Those are *not* one thing. Every `(Exaltation)` reading eqldb holds — 49 of
/// them — carries the Augmentation flag, is size TINY, prints no weight and
/// has no stat rows at all. No plain reading carries the flag: 0 of 1,185.
/// The separation is total on every one of those four, and the augment's own
/// tooltip says "This Augmentation fits in slot types", where its host is a
/// wearable shield. Fangol is GIANT, weighs 8.0 and has four stats; Fangol
/// (Exaltation) is TINY, weighs nothing and has none.
///
/// So an id does not answer "is this the same object". Two rows sharing one
/// is grounds to look, not grounds to merge — merging these would delete the
/// augment. The reasoning that nearly did (id is grade-independent, therefore
/// a suffix is a state) fails on its own terms: the id is item/augment
/// independent too, so it cannot carry that weight either.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ItemId {
    pub item: String,
    pub item_id: u32,
    /// Grades this item is known to exist at: somebody has carried one, or a
    /// loot filter has found one on a corpse. Evidence about those grades and
    /// nothing about the ones between them.
    ///
    /// The two claims are not equally strong, and the record `eqlparserd`
    /// accumulates keeps them apart — `grades_held` there means held, and
    /// `grades_seen` means seen. They arrive here as one list because the
    /// question this side asks is which grades the item has, which both
    /// answer; the field kept its name because that is what the column is
    /// called and renaming it would be a schema change for a comment.
    #[serde(default)]
    pub grades_held: Vec<u8>,
}

/// A creature's position at the moment it died.
///
/// Paired by the parser: a `/loc` reading within [`SPOT_WINDOW`] seconds of a
/// kill you made. Both halves are things the game printed, but the *pairing*
/// is an inference — the reading is where the player was standing, not where
/// the corpse is, and the player has usually walked to the corpse to loot it.
///
/// Which is why `gap` travels all the way to the page instead of being
/// thresholded away at ingest. It is the whole reliability of the row: a
/// one-second reading is a corpse, a nine-second one is a guess about where
/// somebody wandered, and a reader deciding whether to trust the pin needs to
/// know which they are looking at. Dropping it here would leave the site
/// asserting nine seconds and one second equally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatureSpotRow {
    /// The creature's name as the log wrote it, article and all.
    pub name: String,
    /// The zone's display name, for a reader.
    #[serde(default)]
    pub zone: String,
    /// The archive the zone's map is drawn from, which is what a position is
    /// actually plotted against. A spot with none cannot reach a map.
    #[serde(default)]
    pub zone_short: String,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub z: f64,
    /// Seconds between the death and the reading. See the type's own note:
    /// this is the row's reliability, not a detail.
    #[serde(default)]
    pub gap: i64,
    /// When it happened, seconds since the epoch. Two spots at one position
    /// an hour apart are two sightings of a spawn point; the same second
    /// twice would be one kill double-counted.
    #[serde(default)]
    pub at: i64,
}

/// Where one thing stands in one zone.
///
/// Walked by hand: somebody hailed the thing and typed `/loc`, so this is an
/// observation like a drop or a price and travels the same way. The position
/// is in EQ *map* coordinates, which is what the client's map files and the
/// SVGs exported from them use — no projection is needed to draw it.
///
/// Keyed on the zone's archive shortname rather than its display name. Two
/// difficulty variants of a zone share one archive and therefore one map, so
/// the display name does not identify the picture a marker belongs on.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ZoneMarker {
    pub name: String,
    /// The archive the zone's map is drawn from, e.g. `freportw`.
    pub zone_short: String,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub z: f64,
    /// What the thing does, where the player wrote it down: `Banker`,
    /// `Berserker Guildmaster`, `Smithing Supplies`.
    ///
    /// Nothing in the game states this. It comes from the suffix on the
    /// `--EQLCartographer--` tag, which is somebody standing there deciding
    /// what the thing is for — the one part of a marker no log line supplies.
    #[serde(default)]
    pub role: String,
    /// `npc`, `zone` for a zone line, or `craft` for a forge or oven.
    ///
    /// Carried rather than derived: a crafting station is not an NPC with an
    /// odd name, and the page colours and links them differently.
    #[serde(default)]
    pub kind: String,
}

/// One server, and the rules it plays by.
///
/// EverQuest's servers do not agree about what an item is. Firiona Vie makes
/// everything tradeable; the random-loot TLPs shuffle the loot tables outright;
/// Frostreaver removes NO DROP rather than relaxing it; Lethar is to make
/// every item ALL/ALL. So "this mob drops that item" and "this item is NO
/// DROP" are both answers to a question that has to name a ruleset before it
/// means anything.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerRules {
    /// The token in a log's filename: `eqlog_<Character>_<server>.txt`.
    pub server: String,
    /// What the client's own server-select screen calls this ruleset:
    /// "Standard", "Randomized Loot", "Timelocked Progression", "PvP". It
    /// describes the *progression* rules and says nothing about trade, which
    /// is why the loot and flag behaviour below are separate fields rather
    /// than inferred from it — Firiona Vie reads "Roleplaying Preferred" and
    /// has been free trade for twenty years.
    #[serde(default)]
    pub client_ruleset: String,
    /// The expansion the server is currently unlocked to.
    #[serde(default)]
    pub expansion: String,
    #[serde(default)]
    pub true_box: bool,
    #[serde(default)]
    pub all_access: bool,
    #[serde(default)]
    pub random_loot: bool,
    #[serde(default)]
    pub free_trade: bool,
    /// Whether this contributor has actually played here. The registry lists
    /// every server the game has; only some of them have anything behind
    /// them, and a picker offering twenty empty ones is worse than useless.
    #[serde(default)]
    pub observed: bool,
    /// How a person writes it: "Firiona Vie" for `firiona`.
    #[serde(default)]
    pub display: String,
    pub ruleset: String,
}

/// What a ruleset is, in enough words for a page to warn about it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ruleset {
    pub id: String,
    #[serde(default)]
    pub display: String,
    /// Why a reader should or should not trust a drop filed under it.
    #[serde(default)]
    pub note: String,
    /// Whether loot tables are shuffled here. The one property the site has to
    /// act on rather than merely display: a drop from a shuffled table says
    /// nothing about where the item drops anywhere else.
    #[serde(default)]
    pub random_loot: bool,
}

/// One expansion, and where it falls in the release order.
///
/// Not observed and not in any client file. This is a catalogue somebody
/// maintains by hand, carried for the same reason the AA one is: there is
/// nothing here for two contributors to disagree about.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Expansion {
    pub name: String,
    /// Release order, 1 for Classic. What a list sorts by, since alphabetical
    /// order of expansion names is meaningless to anybody who plays.
    #[serde(default)]
    pub sort_order: i64,
}

/// A zone, and the expansion it came with.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExpansionZone {
    pub zone: String,
    pub expansion: String,
}

/// A zone's display name paired with the archive its map comes from.
///
/// Nothing the client ships joins these two. `/who` prints them together once
/// per zone visited, which is where this comes from — so it covers the zones
/// somebody has actually stood in, and a zone with no pair simply shows no
/// map rather than guessing at one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ZoneShortname {
    pub zone: String,
    pub zone_short: String,
}

/// An achievement, which here is a name and nothing else.
///
/// The client announces an achievement when it is earned and says nothing
/// about what it required, what it is worth, or what it belongs with. The
/// timestamp it does give is when *this* character earned it, which is
/// progress and does not travel. So an achievement page can honestly say the
/// achievement exists and no more, and says exactly that rather than dressing
/// a bare list up as a catalogue.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Achievement {
    pub name: String,
    /// What the achievement window files it under.
    ///
    /// Empty for one learned from a log, where the client shouts the name and
    /// nothing else. A dump of the window supplies it.
    #[serde(default)]
    pub category: String,
    /// The checklist beneath it, one line per thing to do.
    ///
    /// Kept as separate lines rather than joined into a paragraph, because
    /// that is what they are: three factions to raise is three errands.
    #[serde(default)]
    pub requirements: Vec<String>,
    /// Where the achievement window puts this one's category, counting from
    /// the top. -1 for one learned from a log, which knows no category at all.
    ///
    /// The window's order is not alphabetical and not the order of the
    /// client's category ids. It is only written down in the dump.
    #[serde(default = "no_window_order")]
    pub window_order: i64,
}

fn no_window_order() -> i64 {
    -1
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuestLine {
    #[serde(default)]
    pub at: i64,
    #[serde(default)]
    pub speaker: String,
    #[serde(default)]
    pub text: String,
}

/// What one merchant charged for one item.
///
/// Merchants are kept apart rather than averaged: the same reagent costs
/// different amounts from different vendors and at different factions, and
/// "which zone was that vendor in" is precisely the question one blended
/// price cannot answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorRow {
    pub item: String,
    pub merchant: String,
    #[serde(default)]
    pub zone: String,
    pub unit_copper: u64,
    /// How many were actually bought at this price.
    pub bought: u64,
    /// How many times the merchant quoted it without a sale.
    ///
    /// Clicking an item in a merchant's window makes them state the price,
    /// which costs nothing and prices a whole shop in a minute. It is the
    /// same number a purchase establishes — across 643 pairs seen both ways
    /// the quote matched the unit price paid every time — so this is not
    /// weaker evidence, just cheaper. Counted apart because a purchase also
    /// proves the item was in stock and affordable, and a quote does not.
    #[serde(default)]
    pub quoted: u64,
    pub last_bought: i64,
}

#[cfg(test)]
mod sender_tests {
    use super::*;

    /// The one that matters. An ingest replaces by contributor, so a bundle
    /// allowed to name somebody else is a bundle that can erase them.
    #[test]
    fn a_bundle_may_not_be_sent_under_another_contributors_name() {
        let doc = Bundle::new("Rivont", 1_788_000_000);
        let e = doc.check_sender("Bkisva").unwrap_err().to_string();
        assert!(e.contains("Bkisva"), "{e}");
        assert!(e.contains("Rivont"), "{e}");
        // Says what it would have cost, not just that it was refused: the
        // sender is usually somebody who mis-set a config, and the reason is
        // what tells them which field.
        assert!(e.contains("replace"), "{e}");
    }

    #[test]
    fn a_bundle_sent_under_its_own_name_is_fine() {
        let doc = Bundle::new("Rivont", 1_788_000_000);
        doc.check_sender("Rivont").unwrap();
    }

    /// Whitespace is not a second contributor. The token's name comes from
    /// the database and the bundle's from a file somebody wrote, so the two
    /// arrive with different habits.
    #[test]
    fn the_declared_name_is_trimmed_before_it_is_compared() {
        let mut doc = Bundle::new("Rivont", 1_788_000_000);
        doc.contributor = "  Rivont  ".into();
        doc.check_sender("Rivont").unwrap();
    }

    /// An empty name would otherwise compare equal to an empty token name and
    /// pass, which is the one input that turns this check off entirely.
    #[test]
    fn a_bundle_naming_nobody_is_refused_even_by_a_nameless_token() {
        let mut doc = Bundle::new("Rivont", 1_788_000_000);
        doc.contributor = "   ".into();
        let e = doc.check_sender("").unwrap_err().to_string();
        assert!(e.contains("names no contributor"), "{e}");
    }
}

#[cfg(test)]
mod spawn_tests {
    use super::*;

    /// The kinds survive a round trip through the bundle as words rather than
    /// numbers, so a bundle stays readable and a reordering of the enum
    /// cannot silently turn every raid target into a timed spawn.
    #[test]
    fn a_kind_travels_as_its_own_name() {
        for k in [SpawnKind::Timed, SpawnKind::Instanced, SpawnKind::Static, SpawnKind::Unknown] {
            let json = serde_json::to_string(&k).unwrap();
            assert_eq!(json, format!("\"{}\"", k.as_str()));
            assert_eq!(serde_json::from_str::<SpawnKind>(&json).unwrap(), k);
            assert_eq!(SpawnKind::parse(k.as_str()), k);
        }
    }

    /// An unreadable kind becomes `Unknown`, never `Timed`.
    ///
    /// This is the direction that matters. Defaulting a kind nobody
    /// recognises to the ordinary timer would state a respawn for something
    /// that has none, which is the exact failure the whole model exists to
    /// prevent; defaulting it to unknown states nothing.
    #[test]
    fn an_unreadable_kind_is_unknown_rather_than_timed() {
        for s in ["", "  ", "weekly", "22 minutes", "respawn", "TIMED?"] {
            assert_eq!(SpawnKind::parse(s), SpawnKind::Unknown, "{s:?}");
        }
        // And the one that must still work, in any case.
        assert_eq!(SpawnKind::parse("  Instanced  "), SpawnKind::Instanced);
    }

    /// A creature with no rule of its own is absent from the registry, and a
    /// timed one with no seconds is deferring to the default. Those are
    /// different, and both have to survive serialisation.
    #[test]
    fn a_timed_rule_may_carry_no_duration() {
        let r: SpawnRule = serde_json::from_str(
            r#"{"creature":"a rat","kind":"timed"}"#,
        )
        .unwrap();
        assert_eq!(r.kind, SpawnKind::Timed);
        assert_eq!(r.seconds, None);
        assert!(r.lockout.is_none());
    }

    #[test]
    fn a_lockout_reads_as_a_weekday_and_a_daily_allowance() {
        let r: SpawnRule = serde_json::from_str(
            r#"{"creature":"Lord Nagafen","kind":"instanced",
                "lockout":{"resets_weekday":2,"daily_kills":1}}"#,
        )
        .unwrap();
        assert_eq!(r.kind, SpawnKind::Instanced);
        let l = r.lockout.expect("a lockout");
        assert_eq!(l.resets_weekday, 2, "Tuesday");
        assert_eq!(l.daily_kills, 1);
        // The thing a duration field would have forced somebody to invent.
        assert_eq!(r.seconds, None);
    }
}

#[cfg(test)]
mod extra_tests {
    use super::*;

    /// A field the struct has no home for is captured, not lost.
    ///
    /// The point is not the capture. It is that the set stays in step with
    /// the struct by construction: a field added above stops appearing in
    /// `extra`, with nothing to remember to update. A hand-maintained list of
    /// known names could not promise that, and would drift the first time
    /// somebody added a field without thinking about the checker.
    #[test]
    fn an_unknown_field_is_captured_rather_than_dropped_silently() {
        let f: ItemFacts = serde_json::from_str(
            r#"{"item":"Chitin Shell Shield","type":"Shield",
                "container":"CLOSED","tier_progress":"0 / 1"}"#,
        )
        .unwrap();
        assert_eq!(f.item, "Chitin Shell Shield");
        // Both of these used to land in `extra` and both are fields now,
        // which is the property this test exists for: the set narrows by
        // itself as the struct grows, with nothing to remember to update.
        // `container` was the last one held back, and only because two of
        // its readings were booleans and nobody could say whether it meant
        // the container's state or that the item is one. The booleans were a
        // capture-side bug; all 19 readings now print the same word.
        assert_eq!(f.tier_progress, "0 / 1");
        assert_eq!(f.container, "CLOSED");
        // `type` is what is left, and it is the reason this test was written:
        // a batch carried it holding the window's `Type:` line, and a `type`
        // field meaning item *category* would have absorbed it in silence.
        assert_eq!(f.extra.len(), 1);
        assert_eq!(f.extra["type"], serde_json::json!("Shield"));
    }

    /// What is captured is read but not written back out.
    ///
    /// Carrying it onward would let a field added here later pick these up
    /// out of old bundles as well as out of new batches — a second place for
    /// the collision this exists to expose.
    #[test]
    fn a_captured_field_does_not_travel_in_the_bundle() {
        let f: ItemFacts =
            serde_json::from_str(r#"{"item":"Chitin Shell Shield","type":"Shield"}"#).unwrap();
        let out = serde_json::to_string(&f).unwrap();
        assert!(out.contains("Chitin Shell Shield"), "{out}");
        assert!(!out.contains("\"type\""), "the unknown field must not be re-emitted: {out}");
    }
}

/// A bundle's identity, which is its content.
///
/// Sixteen hex characters of the SHA-256 of the file exactly as it was read,
/// so re-ingesting a file that has not changed is visibly the *same* bundle
/// rather than a new one, and a bundle that has changed cannot pretend to be
/// the old one.
///
/// Here rather than in `eqldb-ingest`, where it was, because the ingest is no
/// longer the only thing that needs to name a bundle: `eqldb-worker` records
/// what it ingested in `site.ingest_log`, and a second copy of this line would
/// be a second copy of the rule -- the fault this codebase produces most
/// often. Two callers, one definition, and the digest is over bytes rather
/// than over a parsed document so that whitespace cannot make one bundle two.
pub fn bundle_id(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))[..16].to_string()
}

/// Read a bundle off disk and name it, in one step.
///
/// The two belong together: the id is the digest of the bytes that were
/// parsed, so a caller that reads the file twice -- or parses one copy and
/// digests another -- can produce an id that describes neither. Returning
/// both from one read makes that impossible rather than unlikely.
pub fn read_identified(path: &std::path::Path) -> anyhow::Result<(Bundle, String)> {
    use anyhow::Context as _;
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let doc: Bundle = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing {} as a contribution bundle", path.display()))?;
    let id = bundle_id(&bytes);
    Ok((doc, id))
}

#[cfg(test)]
mod lore_only_tests {
    use super::*;

    fn lore_window() -> ItemFacts {
        ItemFacts {
            item: "Mote of Potential".into(),
            server: "freeport".into(),
            ruleset: "legends".into(),
            source: "screenshot".into(),
            evidence: "EQ003000.jpg".into(),
            lore: "Strengthens an item".into(),
            has_lore_tab: Some(true),
            ..Default::default()
        }
    }

    #[test]
    fn a_window_carrying_only_lore_is_a_reading_of_one_field() {
        assert!(lore_window().lore_only());
    }

    /// The case that was live and unnoticed.
    ///
    /// Every reading `emit_itemfacts.py` writes carries `crops`, and this
    /// struct has no field for it, so it lands in `extra`. Counted against
    /// the real corpus: 2,280 of 2,280 entries carry at least one such key,
    /// which made this method answer false for every reading ever written.
    #[test]
    fn the_readers_own_bookkeeping_does_not_make_it_a_reading_of_the_item() {
        let mut f = lore_window();
        f.extra.insert("crops".into(), serde_json::json!(["eql_mote.png"]));
        f.extra.insert("captured_first".into(), serde_json::json!("2026-08-19"));
        assert!(
            f.lore_only(),
            "an unrecognised key turned a Lore tab into a reading of the item"
        );
    }

    /// And a real reading is still a real reading.
    #[test]
    fn a_field_of_the_item_makes_it_a_reading_of_the_item() {
        for f in [
            ItemFacts { slot: "Neck".into(), ..lore_window() },
            ItemFacts { flags: vec!["Lore".into()], ..lore_window() },
            ItemFacts {
                stats: vec![ItemStat { name: "AC".into(), value: 10, heroic: None }],
                ..lore_window()
            },
        ] {
            assert!(!f.lore_only(), "a reading of the item was treated as one field");
        }
    }

    #[test]
    fn a_window_with_no_lore_is_not_a_lore_window() {
        assert!(!ItemFacts { lore: "  ".into(), ..lore_window() }.lore_only());
    }
}

#[cfg(test)]
mod observed_tests {
    use super::*;

    fn span(key: &str, from: u64, to: u64) -> Observed {
        Observed::Log { log_key: key.into(), from, to }
    }

    /// Spans are half-open, and adjacency is the case that decides whether a
    /// catch-up walk works at all.
    ///
    /// A client sending a log in 24-hour blocks hands over `0..100` and then
    /// `100..200`. If the shared boundary counted as an overlap the second
    /// block would be refused and the walk would stop after one step — so
    /// this is not a detail of the comparison, it is whether the feature
    /// exists.
    #[test]
    fn adjacent_spans_do_not_overlap_but_a_shared_byte_does() {
        assert!(!span("a", 0, 100).overlaps(&span("a", 100, 200)));
        assert!(span("a", 0, 101).overlaps(&span("a", 100, 200)));
        assert!(span("a", 0, 100).overlaps(&span("a", 50, 60)), "contained");
        assert!(span("a", 50, 60).overlaps(&span("a", 0, 100)), "containing");
        assert!(span("a", 0, 100).overlaps(&span("a", 0, 100)), "identical");
    }

    /// Two logs that happen to be read at the same offsets are two logs.
    ///
    /// Every contributor's first log starts at byte 0, so a comparison that
    /// ignored the key would refuse the second machine's first block as a
    /// repeat of the first machine's.
    #[test]
    fn a_different_log_at_the_same_offsets_is_not_an_overlap() {
        assert!(!span("a", 0, 100).overlaps(&span("b", 0, 100)));
    }

    /// A picture has no extent, so it overlaps itself and nothing else.
    #[test]
    fn evidence_without_a_span_overlaps_only_itself() {
        let one = Observed::Shot { shot_key: "abc".into() };
        let two = Observed::Shot { shot_key: "def".into() };
        assert!(one.overlaps(&one));
        assert!(!one.overlaps(&two));
        assert!(
            !one.overlaps(&Observed::Dump { digest: "abc".into() }),
            "the same string under a different kind is different evidence"
        );
    }

    /// The tag travels as a word.
    ///
    /// `Shot` and `Dump` are both a single string on the wire, so an untagged
    /// enum would have to guess between them by field name and would silently
    /// pick whichever variant serde tried first.
    #[test]
    fn a_variant_travels_as_its_own_name() {
        let json = serde_json::to_string(&span("k", 0, 10)).unwrap();
        assert!(json.contains(r#""kind":"log""#), "{json}");
        assert_eq!(serde_json::from_str::<Observed>(&json).unwrap(), span("k", 0, 10));

        let shot = Observed::Shot { shot_key: "d".into() };
        let json = serde_json::to_string(&shot).unwrap();
        assert!(json.contains(r#""kind":"shot""#), "{json}");
        assert_eq!(serde_json::from_str::<Observed>(&json).unwrap(), shot);
    }

    /// A bundle written before either field existed still parses, and says
    /// nothing about what it covered rather than claiming it covered nothing.
    ///
    /// Every bundle on file today is this shape — 285 of them — so a change
    /// that made them unreadable would strand the corpus rather than migrate
    /// it.
    #[test]
    fn a_bundle_from_before_these_fields_still_parses() {
        let b: Bundle = serde_json::from_str(
            r#"{"bundle_version":1,"generated_at":0,"generator":"old",
                "contributor":"hammackj","game":"everquest-legends","note":""}"#,
        )
        .expect("an older bundle must stay readable");
        assert!(b.covers.is_empty());
        assert!(b.supersedes.is_empty());
    }

    /// And a bundle that covers nothing does not grow the wire format.
    #[test]
    fn empty_fields_are_not_written() {
        let json = serde_json::to_string(&Bundle::new("hammackj", 0)).unwrap();
        assert!(!json.contains("covers"), "{json}");
        assert!(!json.contains("supersedes"), "{json}");
    }
}
