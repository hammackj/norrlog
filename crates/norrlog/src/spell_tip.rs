//! Spell hover cards, and the cache behind them.
//!
//! Each name is asked about once per run. The answer is kept, and so is a
//! failure, as nothing, so a name the daemon cannot place is never asked
//! again and never draws a card. The card itself is a GPUI tooltip whose view
//! watches the cache, so a card shown before its answer arrives fills in when
//! it does; until then it draws nothing, as the renderer draws nothing while
//! it waits.

use crate::api::{Api, query};
use crate::theme::px;
use crate::theme::*;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{h_flex, v_flex};
use norrlog_api::{CLASS_CANNOT_CAST, Class, SpellLookup};
use std::collections::HashMap;

enum Lookup {
    Pending(#[allow(dead_code)] Task<()>),
    /// Boxed: a lookup is far larger than a task handle.
    Done(Option<Box<SpellLookup>>),
}

pub struct SpellCards {
    api: Option<Api>,
    known: HashMap<String, Lookup>,
}

impl SpellCards {
    pub fn new() -> Self {
        SpellCards {
            api: None,
            known: HashMap::new(),
        }
    }

    /// A different daemon may know different spells: start over.
    pub fn set_api(&mut self, api: Option<Api>) {
        self.api = api;
        self.known.clear();
    }

    fn ensure(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.known.contains_key(name) {
            return;
        }
        let Some(api) = self.api.clone() else { return };
        let key = name.to_string();
        let path = format!("/api/spell?{}", query(&[("name", key.clone())]));
        let task = cx.spawn(async move |this, cx| {
            let got = api.get::<SpellLookup>(path).await.ok();
            let _ = this.update(cx, |c, cx| {
                c.known.insert(key, Lookup::Done(got.map(Box::new)));
                cx.notify();
            });
        });
        self.known.insert(name.to_string(), Lookup::Pending(task));
    }

    fn get(&self, name: &str) -> Option<&SpellLookup> {
        match self.known.get(name) {
            Some(Lookup::Done(Some(d))) => Some(&**d),
            _ => None,
        }
    }
}

/// Hang a spell card on an element, keyed by the ability's name.
pub fn spell_tip<E: StatefulInteractiveElement>(
    el: E,
    cards: &Entity<SpellCards>,
    name: &str,
) -> E {
    let cards = cards.clone();
    let name = name.to_string();
    el.tooltip(move |_, cx| {
        let (cards, name) = (cards.clone(), name.clone());
        cx.new(|cx| SpellTip::new(cards, name, cx)).into()
    })
}

struct SpellTip {
    cards: Entity<SpellCards>,
    name: String,
    _watch: Subscription,
}

impl SpellTip {
    fn new(cards: Entity<SpellCards>, name: String, cx: &mut Context<Self>) -> Self {
        cards.update(cx, |c, cx| c.ensure(&name, cx));
        let _watch = cx.observe(&cards, |_, _, cx| cx.notify());
        SpellTip {
            cards,
            name,
            _watch,
        }
    }
}

impl Render for SpellTip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if crate::parts::cards_hidden() {
            return div();
        }
        let Some(c) = self.cards.read(cx).get(&self.name).and_then(card) else {
            return div();
        };
        let (badge, badge_colour) = if c.live {
            ("EQLDB", HEAL)
        } else {
            ("local spell file", MUTED)
        };
        // Wrapped text in a card that sizes to its content measures short and
        // spills out of the card, so a card with a description takes the
        // renderer's full 340 px.
        let mut out = v_flex()
            .when(c.desc.is_some(), |d| d.w(px(340.)))
            .max_w(px(340.))
            .gap_1()
            .px(px(10.))
            .py(px(8.))
            .rounded(px(6.))
            .border_1()
            .border_color(hex(BORDER))
            .bg(hex(PANEL_2))
            .shadow_lg()
            .text_sm()
            .text_color(hex(TEXT))
            .child(
                h_flex()
                    .gap_2()
                    .mb_1()
                    .child(div().font_weight(FontWeight::BOLD).child(c.title))
                    .child(
                        div()
                            .ml_auto()
                            .text_xs()
                            .whitespace_nowrap()
                            .text_color(hex(badge_colour))
                            .child(badge),
                    ),
            );
        if !c.facts.is_empty() {
            out = out.child(v_flex().children(c.facts.into_iter().map(|(label, v)| {
                h_flex()
                    .justify_between()
                    .gap_3()
                    .child(div().text_color(hex(MUTED)).child(label))
                    .child(div().font_weight(FontWeight::BOLD).child(v))
            })));
        }
        if let Some(classes) = c.classes {
            out = out.child(
                h_flex()
                    .gap_1()
                    .child(div().text_color(hex(MUTED)).child("Classes"))
                    .child(classes),
            );
        }
        if let Some(desc) = c.desc {
            out = out.child(
                div()
                    .mt_1()
                    .pt_1()
                    .border_t_1()
                    .border_color(hex(BORDER))
                    .text_xs()
                    .text_color(hex(MUTED))
                    .child(desc),
            );
        }
        out
    }
}

/// What a card shows, decided apart from drawing it.
#[derive(Debug, PartialEq)]
struct Card {
    title: String,
    /// From eqdb rather than the spell file alone.
    live: bool,
    facts: Vec<(&'static str, String)>,
    classes: Option<String>,
    desc: Option<String>,
}

fn card(d: &SpellLookup) -> Option<Card> {
    if d.source.is_empty() || d.source == "unknown" {
        return None;
    }
    let e = d.eqdb.as_ref().and_then(|e| e.resolved());
    let loc = d.local.as_ref();
    let title = [e.map(|e| e.name.as_str()), loc.map(|l| l.name.as_str())]
        .into_iter()
        .flatten()
        .find(|n| !n.is_empty())
        .unwrap_or(&d.name)
        .to_string();

    let mut facts = Vec::new();
    if let Some(e) = e {
        let mut fact = |label: &'static str, v: Option<String>| {
            if let Some(v) = v.filter(|v| !v.is_empty() && v != "—") {
                facts.push((label, v));
            }
        };
        fact("Mana", (e.mana > 0).then(|| e.mana.to_string()));
        fact(
            "Endurance",
            (e.endurance > 0).then(|| e.endurance.to_string()),
        );
        fact("Cast", e.cast_time.as_ref().map(|t| t.text.clone()));
        fact("Recast", e.recast.as_ref().map(|t| t.text.clone()));
        fact("Duration", e.duration.as_ref().map(|t| t.text.clone()));
    }

    // eqdb's classes when it has them, else the spell file's, which always
    // knows. The renderer's comment says the same; its code only ever read
    // eqdb's.
    let mut cls: Vec<String> = e
        .map(|e| {
            e.classes
                .iter()
                .map(|c| format!("{} {}", c.class, c.level))
                .collect()
        })
        .unwrap_or_default();
    if cls.is_empty()
        && let Some(l) = loc
    {
        cls = l
            .classes
            .iter()
            .enumerate()
            .filter(|(_, lv)| **lv != CLASS_CANNOT_CAST)
            .filter_map(|(i, lv)| Class::from_index(i).map(|c| format!("{} {lv}", c.name())))
            .collect();
    }
    cls.truncate(6);

    Some(Card {
        title,
        live: d.source == "eqdb",
        facts,
        classes: (!cls.is_empty()).then(|| cls.join(", ")),
        desc: e.map(|e| e.description.clone()).filter(|s| !s.is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use super::card;
    use norrlog_api::SpellLookup;

    /// The daemon's reply for a rank the site files under its base name,
    /// trimmed from a real one.
    const BOLT: &str = r#"{"base":"https://eqldb.example","eqdb":{"cast_time":{"ms":3000,"text":"3s"},
        "classes":[{"class":"Shaman","level":49},{"class":"Necromancer","level":50}],
        "description":"Fills your target's blood with poison.","duration":{"kind":"up_to","text":"up to 36s","ticks":6},
        "endurance":0,"id":436,"mana":409,"name":"Envenomed Bolt","recast":{"ms":1500,"text":"1.5s"}},
        "local":{"classes":[255,255,255,255,255,255,255,255,255,49,50,255,255,255,255,255],"id":436,"is_charm":false,
        "name":"Envenomed Bolt"},"name":"Envenomed Bolt X","source":"eqdb"}"#;

    #[test]
    fn an_eqdb_card_carries_its_facts_classes_and_words() {
        let d: SpellLookup = serde_json::from_str(BOLT).unwrap();
        let c = card(&d).unwrap();
        assert_eq!(c.title, "Envenomed Bolt");
        assert!(c.live);
        assert_eq!(
            c.facts,
            vec![
                ("Mana", "409".into()),
                ("Cast", "3s".into()),
                ("Recast", "1.5s".into()),
                ("Duration", "up to 36s".into())
            ]
        );
        assert_eq!(c.classes.as_deref(), Some("Shaman 49, Necromancer 50"));
        assert_eq!(
            c.desc.as_deref(),
            Some("Fills your target's blood with poison.")
        );
    }

    #[test]
    fn with_the_site_down_the_spell_file_still_makes_a_card() {
        let mut d: SpellLookup = serde_json::from_str(BOLT).unwrap();
        d.eqdb = None;
        d.source = "local".into();
        let c = card(&d).unwrap();
        assert!(!c.live);
        assert!(c.facts.is_empty());
        assert_eq!(c.classes.as_deref(), Some("Shaman 49, Necromancer 50"));
        assert_eq!(c.desc, None);
    }

    #[test]
    fn a_page_of_matches_reads_as_its_first() {
        let d: SpellLookup = serde_json::from_str(
            r#"{"name":"Odium VI","source":"eqdb","eqdb":{"matches":[{"id":1,"name":"Odium","mana":10},{"id":2,"name":"Odium II"}]}}"#,
        )
        .unwrap();
        let c = card(&d).unwrap();
        assert_eq!(c.title, "Odium");
        assert_eq!(c.facts, vec![("Mana", "10".into())]);
    }

    #[test]
    fn an_unknown_name_draws_no_card() {
        let d: SpellLookup =
            serde_json::from_str(r#"{"name":"slash","source":"unknown"}"#).unwrap();
        assert_eq!(card(&d), None);
    }
}
