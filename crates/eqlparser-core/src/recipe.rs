//! A recipe book: what a combine makes, out of what, in which tradeskill.
//!
//! # Why this exists
//!
//! The client never names the tradeskill on a combine line, so
//! [`crate::tradeskill`] infers it from an adjacent skill-up. That works right
//! up until a recipe stops skilling you up — which is exactly when you do most
//! of your combines — and it never works at all for a recipe that has never
//! skilled you up in this log. Those combines are left unattributed, and no
//! amount of cleverness about the log will fix it: the evidence is not there.
//!
//! So it is supplied instead. The book is a curated file, grown by
//! `tools/import-recipes.py` from EQTraders recipe listings, and a product
//! found in it is declared rather than guessed at.
//!
//! # Tools are not products
//!
//! A combine hands back the tools it used, and the client announces that with
//! the same line it uses for the thing you actually made:
//!
//! ```text
//! You have fashioned the items together to create something new: Small Piece of High Quality Ore.
//! You have fashioned the items together to create something new: Smithing Chisel.
//! ```
//!
//! Read literally that is two combines, and on one character's log it inflated
//! Blacksmithing by 113 combines that never happened — and worse, put the real
//! combine's components under the chisel. Nothing in the line distinguishes the
//! two cases, so the book carries a list of tools, and a "fashioned" line
//! naming one is the tool coming back.

use crate::game::Game;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;

/// The book that ships with the build.
const EMBEDDED: &str = include_str!("../assets/recipes.json");

/// One input to a recipe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Component {
    pub item: String,
    #[serde(default = "one")]
    pub qty: u32,
    /// Sold by a merchant, per the source listing.
    #[serde(default)]
    pub vendor: bool,
}

fn one() -> u32 {
    1
}

fn default_game() -> Game {
    Game::EverQuest
}

/// One way of making one item.
///
/// Everything but the product and the skill is optional, because a hand-written
/// entry may know only which tradeskill a recipe belongs to. An absent
/// `components` means its inputs are not recorded here — which is not the same
/// as a recipe that takes no inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    pub product: String,
    pub skill: String,
    /// Which game this recipe describes. EQTraders documents classic
    /// EverQuest, so an entry that does not say defaults to that rather than
    /// to the game currently being played — the source is what the tag is
    /// about, not the reader.
    #[serde(default = "default_game")]
    pub game: Game,
    #[serde(rename = "yield", default)]
    pub yields: Option<u32>,
    #[serde(default)]
    pub trivial: Option<u32>,
    #[serde(default)]
    pub containers: Vec<String>,
    #[serde(default)]
    pub components: Option<Vec<Component>>,
    /// Annotations the source hangs off the product name — "TLP Only" and the
    /// like. Kept apart from the name, which has to match what the client
    /// writes, but kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecipeBook {
    #[serde(default)]
    pub note: String,
    /// Items a combine returns rather than produces, lowercased on load.
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub recipes: Vec<Recipe>,
    /// Product key -> tradeskill, for products whose recipes agree on one.
    #[serde(skip)]
    skill_by_product: HashMap<String, String>,
}

impl RecipeBook {
    pub fn parse(json: &str) -> serde_json::Result<Self> {
        let mut b: RecipeBook = serde_json::from_str(json)?;
        b.tools = b.tools.iter().map(|t| t.to_lowercase()).collect();
        b.index();
        Ok(b)
    }

    /// Build the product -> skill index.
    ///
    /// A product whose recipes disagree about the tradeskill is left out
    /// rather than resolved arbitrarily: attribution is meant to be the part
    /// that is known for certain, and a coin toss would undermine that.
    fn index(&mut self) {
        let mut seen: HashMap<String, Option<String>> = HashMap::new();
        for r in &self.recipes {
            let key = r.product.to_lowercase();
            match seen.get(&key) {
                None => {
                    seen.insert(key, Some(r.skill.clone()));
                }
                Some(Some(s)) if !s.eq_ignore_ascii_case(&r.skill) => {
                    seen.insert(key, None);
                }
                Some(_) => {}
            }
        }
        self.skill_by_product = seen.into_iter().filter_map(|(k, v)| Some((k, v?))).collect();
    }

    /// The book compiled into this build.
    pub fn embedded() -> &'static RecipeBook {
        static BOOK: OnceLock<RecipeBook> = OnceLock::new();
        BOOK.get_or_init(|| match RecipeBook::parse(EMBEDDED) {
            Ok(b) => b,
            Err(e) => {
                // Shipping a broken asset is a build-time mistake, caught by
                // the test below; carry on without it rather than taking the
                // daemon down over reference data.
                eprintln!("recipes.json is not valid: {e}");
                RecipeBook::default()
            }
        })
    }

    /// The tradeskill that makes this item, when the book knows it.
    pub fn skill_for(&self, item: &str) -> Option<&str> {
        self.skill_by_product
            .get(&item.to_lowercase())
            .map(String::as_str)
    }

    /// Whether a "fashioned" line naming this item is a tool being handed back
    /// rather than something made.
    pub fn is_tool(&self, item: &str) -> bool {
        self.tools.contains(&item.to_lowercase())
    }

    /// Every recorded way of making an item.
    pub fn recipes_for(&self, item: &str) -> impl Iterator<Item = &Recipe> {
        let key = item.to_lowercase();
        self.recipes
            .iter()
            .filter(move |r| r.product.to_lowercase() == key)
    }

    /// Every product the book can attribute, as (item, skill).
    pub fn products(&self) -> impl Iterator<Item = (&str, &str)> {
        self.skill_by_product
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped book has to parse, since [`RecipeBook::embedded`]
    /// deliberately does not panic when it does not.
    #[test]
    fn the_embedded_book_is_valid() {
        let b = RecipeBook::parse(EMBEDDED).expect("assets/recipes.json should parse");
        assert!(!b.recipes.is_empty());
        for r in &b.recipes {
            assert!(!r.product.is_empty(), "a recipe with no product: {r:?}");
            assert!(
                crate::tradeskill::is_tradeskill(&r.skill),
                "{} is attributed to {:?}, which is not a tradeskill the client \
                 reports — check the alias table in tools/import-recipes.py",
                r.product,
                r.skill
            );
        }
    }

    /// The gap this whole file exists to close, on the recipes that prompted
    /// it: ore that never skills you up, and a chisel that is not a product.
    #[test]
    fn the_book_attributes_the_high_quality_ore_line() {
        let b = RecipeBook::embedded();
        assert_eq!(
            b.skill_for("Small Brick of High Quality Ore"),
            Some("Blacksmithing")
        );
        assert_eq!(
            b.skill_for("small piece of high quality ore"),
            Some("Blacksmithing"),
            "lookup is case-insensitive"
        );
        assert_eq!(b.skill_for("Water Flask"), Some("Brewing"));
        assert!(b.is_tool("Smithing Chisel"));
        assert!(!b.is_tool("Small Brick of High Quality Ore"));
        assert_eq!(b.skill_for("Nothing In Particular"), None);
    }

    /// Every recipe has to say which game it describes, or the whole book is
    /// unusable to anyone who later has data from both.
    #[test]
    fn every_shipped_recipe_names_its_game() {
        for r in &RecipeBook::embedded().recipes {
            assert_ne!(
                r.game,
                Game::Unknown,
                "{} does not say which game it is from",
                r.product
            );
        }
    }

    /// An untagged entry is an EQTraders import that predates the tag, so it
    /// describes classic EverQuest — not whatever is being played now.
    #[test]
    fn an_untagged_recipe_defaults_to_classic() {
        let b = RecipeBook::parse(r#"{"recipes":[{"product":"X","skill":"Baking"}]}"#).unwrap();
        assert_eq!(b.recipes[0].game, Game::EverQuest);
        let b = RecipeBook::parse(
            r#"{"recipes":[{"product":"X","skill":"Baking","game":"everquest-legends"}]}"#,
        )
        .unwrap();
        assert_eq!(b.recipes[0].game, Game::EverQuestLegends);
    }

    /// A hand-written entry knows only the tradeskill, and must not be read as
    /// a recipe that takes no components.
    #[test]
    fn a_partial_entry_records_ignorance_rather_than_emptiness() {
        let b = RecipeBook::parse(
            r#"{"recipes":[{"product":"Kilva's Skin of Flame","skill":"Alchemy"}]}"#,
        )
        .unwrap();
        assert_eq!(b.skill_for("Kilva's Skin of Flame"), Some("Alchemy"));
        let r = b.recipes_for("Kilva's Skin of Flame").next().unwrap();
        assert!(r.components.is_none(), "unknown, not empty");
        assert_eq!(r.trivial, None);
    }

    /// One item, two tradeskills, no answer — better than a coin toss.
    #[test]
    fn a_product_two_skills_disagree_on_is_not_attributed() {
        let b = RecipeBook::parse(
            r#"{"recipes":[
                 {"product":"Ambiguous Thing","skill":"Alchemy"},
                 {"product":"Ambiguous Thing","skill":"Baking"}]}"#,
        )
        .unwrap();
        assert_eq!(b.skill_for("Ambiguous Thing"), None);
        assert_eq!(b.recipes_for("Ambiguous Thing").count(), 2);
    }

    #[test]
    fn components_default_to_one_and_not_vendor_sold() {
        let b = RecipeBook::parse(
            r#"{"recipes":[{"product":"X","skill":"Baking",
                 "components":[{"item":"Flour"},{"item":"Egg","qty":4,"vendor":true}]}]}"#,
        )
        .unwrap();
        let c = b.recipes_for("X").next().unwrap().components.as_ref().unwrap();
        assert_eq!((c[0].qty, c[0].vendor), (1, false));
        assert_eq!((c[1].qty, c[1].vendor), (4, true));
    }
}
