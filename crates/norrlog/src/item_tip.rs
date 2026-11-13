//! Item hover cards, the cache behind them, and the icons they show.
//!
//! A card prefers eqdb field by field over the copy bundled in the daemon:
//! eqdb knows where an item drops and what it is used in, the bundled copy
//! has the client's own tooltip text and an icon for items eqdb has not got
//! to yet. Each name is asked about once per run, failures included, as the
//! spell cards are.
//!
//! An eqdb icon is one 40×40 cell of a sheet. Sheets come through the
//! daemon's `/api/item-icon` proxy, one fetch per sheet, and each cell is
//! cut out once and kept.

use crate::api::{Api, query};
use crate::theme::px;
use crate::theme::*;
use base64::Engine as _;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{h_flex, v_flex};
use norrlog_api::{EqdbItem, ItemBrief, ItemBriefReply, ItemLookup, SpriteIcon, WikiItem};
use std::collections::HashMap;
use std::sync::Arc;

enum Slot<T> {
    Pending(#[allow(dead_code)] Task<()>),
    Done(Option<T>),
}

pub struct ItemCards {
    api: Option<Api>,
    known: HashMap<String, Slot<ItemLookup>>,
    sheets: HashMap<String, Slot<Arc<image::RgbaImage>>>,
    /// Cut cells and decoded bundled icons, by sheet path and offset or by
    /// the data URI itself.
    icons: HashMap<String, Option<Arc<Image>>>,
    /// Row briefs (`/api/item-brief`), by the name as asked. A name asked
    /// about and not answered is kept as `None`, so it is not asked again.
    briefs: HashMap<String, Option<ItemBrief>>,
    brief_task: Option<Task<()>>,
    /// The bundled item data (`/api/wiki`), by lowercased name. Fetched once;
    /// a failure leaves it empty.
    wiki: Option<Slot<HashMap<String, WikiItem>>>,
}

/// Names per `/api/item-brief` request: under the endpoint's own cap.
const BRIEF_CHUNK: usize = 150;

impl ItemCards {
    pub fn new() -> Self {
        ItemCards {
            api: None,
            known: HashMap::new(),
            sheets: HashMap::new(),
            icons: HashMap::new(),
            briefs: HashMap::new(),
            brief_task: None,
            wiki: None,
        }
    }

    /// A different daemon may answer differently: start over.
    pub fn set_api(&mut self, api: Option<Api>) {
        self.api = api;
        self.known.clear();
        self.sheets.clear();
        self.icons.clear();
        self.clear_briefs();
        self.wiki = None;
    }

    /// Forget every brief, so the next look asks again.
    pub fn clear_briefs(&mut self) {
        self.briefs.clear();
        self.brief_task = None;
    }

    /// Fetch the bundled item data, once.
    pub fn load_wiki(&mut self, cx: &mut Context<Self>) {
        if self.wiki.is_some() {
            return;
        }
        let Some(api) = self.api.clone() else { return };
        let task = cx.spawn(async move |this, cx| {
            let got = api
                .get::<HashMap<String, WikiItem>>("/api/wiki".to_string())
                .await
                .unwrap_or_default();
            let _ = this.update(cx, |c, cx| {
                c.wiki = Some(Slot::Done(Some(got)));
                cx.notify();
            });
        });
        self.wiki = Some(Slot::Pending(task));
    }

    fn wiki_of(&self, name: &str) -> Option<&WikiItem> {
        match &self.wiki {
            Some(Slot::Done(Some(w))) => w.get(&name.to_lowercase()),
            _ => None,
        }
    }

    /// Ask about any of `names` not asked about yet: in chunks, one after
    /// another, one batch at a time. Notifies when any chunk was answered.
    pub fn load_briefs(&mut self, names: impl IntoIterator<Item = String>, cx: &mut Context<Self>) {
        if self.brief_task.is_some() {
            return;
        }
        let mut want: Vec<String> = Vec::new();
        for n in names {
            if !n.is_empty() && !self.briefs.contains_key(&n) && !want.contains(&n) {
                want.push(n);
            }
        }
        if want.is_empty() {
            return;
        }
        let Some(api) = self.api.clone() else { return };
        for n in &want {
            self.briefs.insert(n.clone(), None);
        }
        self.brief_task = Some(cx.spawn(async move |this, cx| {
            let mut got = Vec::new();
            for chunk in want.chunks(BRIEF_CHUNK) {
                let path = format!("/api/item-brief?{}", query(&[("names", chunk.join("|"))]));
                if let Ok(r) = api.get::<ItemBriefReply>(path).await {
                    got.push(r);
                }
            }
            let _ = this.update(cx, |c, cx| {
                c.brief_task = None;
                if !got.is_empty() {
                    for r in got {
                        for (k, b) in r.items {
                            c.briefs.insert(k, Some(b));
                        }
                    }
                    cx.notify();
                }
            });
        }));
    }

    fn brief(&self, name: &str) -> Option<&ItemBrief> {
        self.briefs.get(name).and_then(Option::as_ref)
    }

    /// A row's icon: the site's sprite cell, else the bundled image.
    pub fn row_icon(&mut self, name: &str, cx: &mut Context<Self>) -> Option<Arc<Image>> {
        if let Some(s) = self.brief(name).and_then(|b| b.icon.clone()) {
            return self.sprite(&s, cx);
        }
        let uri = self.wiki_of(name)?.icon.clone()?;
        self.icons
            .entry(uri.clone())
            .or_insert_with(|| {
                data_uri_png(&uri).map(|b| Arc::new(Image::from_bytes(ImageFormat::Png, b)))
            })
            .clone()
    }

    /// Where an item's name links: the site's page when it has one (false),
    /// else the wiki's (true), else nowhere.
    pub fn link(&self, name: &str, site_base: &str) -> Option<(String, bool)> {
        if let Some(b) = self
            .brief(name)
            .filter(|b| b.on_site && !b.slug.is_empty() && !site_base.is_empty())
        {
            return Some((format!("{site_base}/item/{}", b.slug), false));
        }
        self.wiki_of(name)
            .and_then(|w| w.url.clone())
            .map(|u| (u, true))
    }

    fn ensure(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.known.contains_key(name) {
            return;
        }
        let Some(api) = self.api.clone() else { return };
        let key = name.to_string();
        let path = format!("/api/item?{}", query(&[("name", key.clone())]));
        let task = cx.spawn(async move |this, cx| {
            let got = api.get::<ItemLookup>(path).await.ok();
            let _ = this.update(cx, |c, cx| {
                c.known.insert(key, Slot::Done(got));
                cx.notify();
            });
        });
        self.known.insert(name.to_string(), Slot::Pending(task));
    }

    fn get(&self, name: &str) -> Option<&ItemLookup> {
        match self.known.get(name) {
            Some(Slot::Done(Some(d))) => Some(d),
            _ => None,
        }
    }

    /// The card's icon: eqdb's sprite cell, else the bundled image, else
    /// none. A sheet still on its way gives none now and a notify later.
    fn icon(&mut self, d: &ItemLookup, cx: &mut Context<Self>) -> Option<Arc<Image>> {
        if let Some(s) = first_match(d).and_then(|m| m.icon.as_ref()) {
            return self.sprite(s, cx);
        }
        let uri = d.wiki.as_ref()?.icon.as_ref()?;
        self.icons
            .entry(uri.clone())
            .or_insert_with(|| {
                data_uri_png(uri).map(|b| Arc::new(Image::from_bytes(ImageFormat::Png, b)))
            })
            .clone()
    }

    pub(crate) fn sprite(&mut self, s: &SpriteIcon, cx: &mut Context<Self>) -> Option<Arc<Image>> {
        let key = format!("{}@{},{}", s.path, s.x, s.y);
        if let Some(i) = self.icons.get(&key) {
            return i.clone();
        }
        match self.sheets.get(&s.path) {
            Some(Slot::Done(Some(sheet))) => {
                let cell = cut(sheet, s).map(|b| Arc::new(Image::from_bytes(ImageFormat::Png, b)));
                self.icons.insert(key, cell.clone());
                cell
            }
            Some(Slot::Done(None)) => None,
            Some(Slot::Pending(_)) => None,
            None => {
                let api = self.api.clone()?;
                let path = s.path.clone();
                let url = format!("/api/item-icon?{}", query(&[("path", path.clone())]));
                let task = cx.spawn(async move |this, cx| {
                    let sheet = api
                        .get_bytes(url)
                        .await
                        .ok()
                        .and_then(|b| {
                            image::load_from_memory_with_format(&b, image::ImageFormat::Png).ok()
                        })
                        .map(|i| Arc::new(i.to_rgba8()));
                    let _ = this.update(cx, |c, cx| {
                        c.sheets.insert(path, Slot::Done(sheet));
                        cx.notify();
                    });
                });
                self.sheets.insert(s.path.clone(), Slot::Pending(task));
                None
            }
        }
    }
}

/// One cell of a sheet, as PNG bytes. The offsets are CSS's: zero or negative.
fn cut(sheet: &image::RgbaImage, s: &SpriteIcon) -> Option<Vec<u8>> {
    let (x, y) = (u32::try_from(-s.x).ok()?, u32::try_from(-s.y).ok()?);
    if x + s.size > sheet.width() || y + s.size > sheet.height() {
        return None;
    }
    let cell = image::imageops::crop_imm(sheet, x, y, s.size, s.size).to_image();
    let mut out = std::io::Cursor::new(Vec::new());
    cell.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

fn data_uri_png(uri: &str) -> Option<Vec<u8>> {
    let b64 = uri.strip_prefix("data:image/png;base64,")?;
    base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()
}

fn first_match(d: &ItemLookup) -> Option<&EqdbItem> {
    d.eqdb.as_ref().and_then(|p| p.matches.first())
}

/// Hang an item card on an element.
pub fn item_tip<E: StatefulInteractiveElement>(el: E, cards: &Entity<ItemCards>, name: &str) -> E {
    let cards = cards.clone();
    let name = name.to_string();
    el.tooltip(move |_, cx| {
        let (cards, name) = (cards.clone(), name.clone());
        cx.new(|cx| ItemTip::new(cards, name, cx)).into()
    })
}

struct ItemTip {
    cards: Entity<ItemCards>,
    name: String,
    _watch: Subscription,
}

impl ItemTip {
    fn new(cards: Entity<ItemCards>, name: String, cx: &mut Context<Self>) -> Self {
        cards.update(cx, |c, cx| c.ensure(&name, cx));
        let _watch = cx.observe(&cards, |_, _, cx| cx.notify());
        ItemTip {
            cards,
            name,
            _watch,
        }
    }
}

impl Render for ItemTip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if crate::parts::cards_hidden() {
            return div();
        }
        let Some(d) = self.cards.read(cx).get(&self.name).cloned() else {
            return div();
        };
        let Some(c) = card(&d) else { return div() };
        let icon = self.cards.update(cx, |cards, cx| cards.icon(&d, cx));
        let (badge, badge_colour) = match c.badge {
            Badge::Live => ("EQLDB", HEAL),
            Badge::Unreachable => ("bundled — EQLDB unreachable", MUTED),
            Badge::Bundled => ("bundled", MUTED),
        };
        let mut out = v_flex()
            .w(px(340.))
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
                    .when_some(icon, |d, i| {
                        d.child(img(i).size(px(40.)).flex_none().rounded(px(4.)))
                    })
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
        match c.stats {
            Stats::Map(rows) => {
                out = out.child(v_flex().mb_1().children(rows.into_iter().map(|(k, v)| {
                    h_flex()
                        .justify_between()
                        .gap_3()
                        .child(div().text_color(hex(MUTED)).child(k))
                        .child(div().font_weight(FontWeight::BOLD).child(v))
                })));
            }
            Stats::Raw(text) => {
                out = out.child(
                    v_flex()
                        .mb_1()
                        .text_xs()
                        .text_color(hex(MUTED))
                        .children(text.lines().map(|l| div().child(l.to_string()))),
                );
            }
            Stats::None => {}
        }
        // One run of text with a muted label, so a long list wraps as a
        // sentence does instead of running out of the card.
        let label_style = HighlightStyle {
            color: Some(hex(MUTED)),
            ..Default::default()
        };
        for (label, line) in c.lines {
            let text = format!("{label} {line}");
            out = out.child(
                div()
                    .w_full()
                    .child(StyledText::new(text).with_highlights([(0..label.len(), label_style)])),
            );
        }
        out
    }
}

#[derive(Debug, PartialEq)]
enum Badge {
    Live,
    /// Bundled because the configured site did not answer.
    Unreachable,
    /// Bundled because the lookup is off.
    Bundled,
}

#[derive(Debug, PartialEq)]
enum Stats {
    Map(Vec<(String, String)>),
    Raw(String),
    None,
}

#[derive(Debug, PartialEq)]
struct Card {
    title: String,
    badge: Badge,
    stats: Stats,
    lines: Vec<(&'static str, String)>,
}

fn card(d: &ItemLookup) -> Option<Card> {
    if d.source.is_empty() || d.source == "unknown" {
        return None;
    }
    let m = first_match(d);
    let stats = match (
        m.and_then(|m| m.stats.as_ref())
            .filter(|s| !s.stats.is_empty()),
        d.wiki.as_ref().and_then(|w| w.stats.clone()),
    ) {
        (Some(s), _) => Stats::Map(
            s.stats
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| v.to_string()),
                    )
                })
                .collect(),
        ),
        (None, Some(raw)) if !raw.trim().is_empty() => Stats::Raw(raw),
        _ => Stats::None,
    };
    let mut lines = Vec::new();
    if let Some(m) = m {
        for (label, list) in [
            ("Drops from", &m.sources),
            ("Sold by", &m.vendors),
            ("Reward from", &m.rewarded_by),
            ("Used in", &m.used_in),
        ] {
            if list.is_empty() {
                continue;
            }
            let mut s = list
                .iter()
                .take(4)
                .map(EqdbItem::named)
                .collect::<Vec<_>>()
                .join(", ");
            if list.len() > 4 {
                s.push_str(&format!(" +{}", list.len() - 4));
            }
            lines.push((label, s));
        }
    }
    let badge = if d.source == "eqdb" {
        Badge::Live
    } else if d.base.as_deref().is_some_and(|b| !b.is_empty()) {
        Badge::Unreachable
    } else {
        Badge::Bundled
    };
    Some(Card {
        title: d.name.clone(),
        badge,
        stats,
        lines,
    })
}

#[cfg(test)]
mod tests {
    use super::{Badge, Stats, card, cut, data_uri_png};
    use norrlog_api::{ItemLookup, SpriteIcon};

    #[test]
    fn an_eqdb_card_takes_its_stats_and_names_each_list_by_what_it_holds() {
        let d: ItemLookup = serde_json::from_str(
            r#"{"name":"Hierophant`s Crook","source":"eqdb","base":"https://eqldb.example","eqdb":{"matches":[{
              "name":"Hierophant`s Crook","icon":{"path":"/static/items/017.png","x":-40,"y":-40,"size":40},
              "stats":{"stats":{"Strength":9,"Dexterity":9,"Wisdom":9}},
              "sources":[{"npc":"Lord Nagafen"},{"npc":"a"},{"npc":"b"},{"npc":"c"},{"npc":"d"}],
              "vendors":[{"merchant":"Innkeep Blaise","unit_copper":78}],"rewarded_by":[],"used_in":[{"name":"A Recipe"}]}]}}"#,
        )
        .unwrap();
        let c = card(&d).unwrap();
        assert_eq!(c.badge, Badge::Live);
        // The site's order, not the alphabet's.
        assert_eq!(
            c.stats,
            Stats::Map(vec![
                ("Strength".into(), "9".into()),
                ("Dexterity".into(), "9".into()),
                ("Wisdom".into(), "9".into())
            ])
        );
        assert_eq!(
            c.lines,
            vec![
                ("Drops from", "Lord Nagafen, a, b, c +1".into()),
                // The renderer reads `npc` or `name` and printed a vendor as
                // "[object Object]".
                ("Sold by", "Innkeep Blaise".into()),
                ("Used in", "A Recipe".into()),
            ]
        );
    }

    #[test]
    fn a_bundled_card_says_why_it_is_bundled() {
        let mut d: ItemLookup = serde_json::from_str(
            r#"{"name":"Adamantium Earring","source":"bundled","base":"https://eqldb.example",
                "wiki":{"stats":"MAGIC ITEM\nSlot: EAR"}}"#,
        )
        .unwrap();
        let c = card(&d).unwrap();
        assert_eq!(c.badge, Badge::Unreachable);
        assert_eq!(c.stats, Stats::Raw("MAGIC ITEM\nSlot: EAR".into()));
        assert!(c.lines.is_empty());
        d.base = None;
        assert_eq!(card(&d).unwrap().badge, Badge::Bundled);
        d.source = "unknown".into();
        assert_eq!(card(&d), None);
    }

    #[test]
    fn a_cell_is_cut_from_its_sheet_at_the_css_offset() {
        let mut sheet = image::RgbaImage::new(80, 80);
        sheet.put_pixel(40, 40, image::Rgba([255, 0, 0, 255]));
        let png = cut(
            &sheet,
            &SpriteIcon {
                path: String::new(),
                x: -40,
                y: -40,
                size: 40,
            },
        )
        .unwrap();
        let cell = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!((cell.width(), cell.height()), (40, 40));
        assert_eq!(cell.get_pixel(0, 0), &image::Rgba([255, 0, 0, 255]));
        assert!(
            cut(
                &sheet,
                &SpriteIcon {
                    path: String::new(),
                    x: -80,
                    y: 0,
                    size: 40
                }
            )
            .is_none(),
            "off the sheet"
        );
    }

    #[test]
    fn a_bundled_icon_decodes_from_its_data_uri() {
        assert_eq!(
            data_uri_png("data:image/png;base64,iVBORw0K"),
            Some(vec![0x89, b'P', b'N', b'G', 13, 10])
        );
        assert_eq!(data_uri_png("https://example/x.png"), None);
    }
}
