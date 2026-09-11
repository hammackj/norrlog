//! Core parsing and analysis for EverQuest Legends log files.
//!
//! The crate is layered so each stage is independently testable:
//!
//! * [`event`]  — the typed vocabulary a log line reduces to.
//! * [`parser`] — regex rules turning raw text into events.
//!
//! Later stages (encounter segmentation, spell resolution, persistence) build
//! on these without reaching back into raw text.

pub mod aa;
pub mod alert;
pub mod bestiary;
pub mod bundle;
pub mod class;
pub mod collect;
pub mod classlevel;
pub mod drops;
pub mod encounter;
pub mod event;
pub mod experience;
pub mod feed;
pub mod game;
pub mod itemids;
pub mod lockout;
pub mod npc;
pub mod parser;
pub mod procs;
pub mod recipe;
pub mod skyquest;
pub mod spell;
pub mod task;
pub mod tells;
pub mod tradeskill;
pub mod zone;

pub use class::{Class, ClassDetector, ClassScore};
pub use collect::{
    ConsiderCollector, Considered, FactionCause, FactionCollector, FactionMoves, Player,
    AaCollector, AaStep, CombatCollector, ForageCollector, Marker, MarkerCollector, MarkerKind, PlayerCollector, QuestCollector, SkillCollector, TradeCollector, ZoneForage,
};
pub use encounter::{ActorStats, Encounter, EncounterTracker};
pub use event::{
    Actor, Coin, DamageKind, Event, Flags, LogEntry, LootDisposition, SpellbookChangeKind,
};
pub use experience::{ExpSource, ExpTracker, LevelRecord};
pub use procs::{ProcKind, ProcRegistry, ProcReport, ProcRow, ProcTracker};
pub use feed::{Context, Feed, Observer};
pub use lockout::{BossKill, LockoutReport, LockoutTracker};
pub use parser::Parser;
pub use spell::{ActiveBuff, Spell, SpellDb, SpellResolver};
pub use task::{TaskReport, TaskTracker};
pub use tradeskill::{TradeskillReport, TradeskillTracker};
