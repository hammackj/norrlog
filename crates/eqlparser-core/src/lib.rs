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
pub mod class;
pub mod encounter;
pub mod event;
pub mod experience;
pub mod lockout;
pub mod parser;
pub mod skyquest;
pub mod spell;
pub mod task;
pub mod tells;
pub mod tradeskill;

pub use class::{Class, ClassDetector, ClassScore};
pub use encounter::{ActorStats, Encounter, EncounterTracker};
pub use experience::{ExpSource, ExpTracker, LevelRecord};
pub use lockout::{BossKill, LockoutReport, LockoutTracker};
pub use event::{
    Actor, Coin, DamageKind, Event, Flags, LogEntry, LootDisposition, SpellbookChangeKind,
};
pub use parser::Parser;
pub use spell::{ActiveBuff, Spell, SpellDb, SpellResolver};
pub use task::{TaskReport, TaskTracker};
pub use tradeskill::{TradeskillReport, TradeskillTracker};
