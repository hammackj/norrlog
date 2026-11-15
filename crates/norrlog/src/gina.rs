//! Triggers from GINA, the EverQuest trigger tool, as alerts.
//!
//! GINA shares triggers as a `.gtp` package, a zip holding `ShareData.xml`,
//! and keeps its own in `GINAConfig.xml`; both nest `TriggerGroup`s holding
//! `Trigger`s, and either can be read here. A trigger maps onto an
//! [`AlertRule`] closely:
//!
//! - its text, plain or a regular expression, becomes the rule's trigger.
//!   GINA's tokens become what they stand for: `{C}` stays `{C}`, which
//!   the daemon makes whoever it follows when it compiles the rule, `{S}` (and
//!   `{S1}`…) any text, `{N}` a number, `{TS}` a duration, each captured under
//!   its own name so the banner and the speech can quote it;
//! - the display text, when it has one, is the banner, which is the rule's
//!   name; the text-to-speech words are what is said;
//! - a countdown timer keeps its length.
//!
//! What has no counterpart is said in the notes rather than dropped quietly:
//! a sound file of the trigger's own (a chime plays instead), a stopwatch, a
//! timer's early warnings, a `{N>5}` comparison (any number matches).
//!
//! Every rule gets an id from its group path and name, so importing the same
//! package again replaces its rules instead of adding a second copy.
//!
//! [`to_package`] goes the other way, so alerts made here can be shared with
//! someone using GINA.

use norrlog_api::{AlertRule, Sound, Trigger};
use roxmltree::{Document, Node};
use std::io::Read;

/// What a package held.
#[derive(Debug, Default)]
pub struct Imported {
    pub rules: Vec<AlertRule>,
    pub groups: usize,
    /// One line per trigger that did not come over whole: `name: why`.
    pub notes: Vec<String>,
}

/// Read a `.gtp` package or a GINA XML file.
pub fn read_file(path: &std::path::Path) -> Result<Imported, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let xml = if bytes.starts_with(b"PK") {
        from_zip(&bytes)?
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    from_xml(&xml)
}

/// The XML inside a package: `ShareData.xml`, or failing that its only XML.
fn from_zip(bytes: &[u8]) -> Result<String, String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let names: Vec<String> = zip
        .file_names()
        .filter_map(|n| n.ok().map(|n| n.into_owned()))
        .collect();
    let pick = names
        .iter()
        .find(|n| n.eq_ignore_ascii_case("ShareData.xml"))
        .or_else(|| names.iter().find(|n| n.to_lowercase().ends_with(".xml")))
        .ok_or("the package holds no XML")?
        .clone();
    let mut f = zip.by_name(&pick).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    f.read_to_end(&mut raw).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

pub fn from_xml(xml: &str) -> Result<Imported, String> {
    let xml = xml.trim_start_matches('\u{feff}');
    let doc = Document::parse(xml).map_err(|e| format!("not GINA XML: {e}"))?;
    let mut out = Imported {
        groups: doc
            .descendants()
            .filter(|n| n.has_tag_name("TriggerGroup"))
            .count(),
        ..Default::default()
    };
    for t in doc.descendants().filter(|n| {
        n.has_tag_name("Trigger") && n.parent().is_some_and(|p| p.has_tag_name("Triggers"))
    }) {
        if let Some(rule) = trigger(t, &mut out.notes) {
            out.rules.push(rule);
        }
    }
    if out.rules.is_empty() && out.groups == 0 {
        return Err("no GINA triggers in this file".into());
    }
    Ok(out)
}

/// A direct child's text.
fn field<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.children()
        .find(|c| c.has_tag_name(name))
        .and_then(|c| c.text())
        .map(str::trim)
}

fn flag(n: Node, name: &str) -> bool {
    field(n, name).is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

fn number(n: Node, name: &str) -> u64 {
    field(n, name).and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// The names of the groups a trigger sits in, outermost first.
fn group_path(t: Node) -> Vec<String> {
    let mut path: Vec<String> = t
        .ancestors()
        .filter(|a| a.has_tag_name("TriggerGroup"))
        .filter_map(|g| field(g, "Name").map(str::to_string))
        .collect();
    path.reverse();
    path
}

fn trigger(t: Node, notes: &mut Vec<String>) -> Option<AlertRule> {
    let title = field(t, "Name").unwrap_or("").to_string();
    let text = field(t, "TriggerText").unwrap_or("");
    let label = if title.is_empty() {
        text.to_string()
    } else {
        title.clone()
    };
    let mut note = |why: &str| notes.push(format!("{label}: {why}"));
    if text.is_empty() {
        note("no trigger text, skipped");
        return None;
    }
    let (trigger, warnings) = convert(text, flag(t, "EnableRegex"));
    for w in warnings {
        note(&w);
    }
    let display = field(t, "DisplayText").filter(|d| flag(t, "UseText") && !d.is_empty());
    let name = display.map(str::to_string).unwrap_or_else(|| label.clone());
    let speak = flag(t, "UseTextToVoice");
    let say = field(t, "TextToVoiceText")
        .filter(|s| speak && !s.is_empty())
        .map(str::to_string);
    let media = flag(t, "PlayMediaFile");
    if media {
        note("plays its own sound file; a chime plays instead");
    }
    let timer = match field(t, "TimerType").unwrap_or("NoTimer") {
        "Timer" | "RepeatingTimer" => {
            let ms = number(t, "TimerMillisecondDuration");
            let secs = if ms > 0 {
                ms.div_ceil(1000)
            } else {
                number(t, "TimerDuration")
            };
            if t.children()
                .any(|c| c.has_tag_name("TimerEndingTrigger") && flag(c, "UseText"))
            {
                note("its timer's early warning is not carried over");
            }
            secs.min(24 * 3600) as u32
        }
        "Stopwatch" => {
            note("a stopwatch has no counterpart; imported without it");
            0
        }
        _ => 0,
    };
    let mut path = group_path(t);
    path.push(title);
    Some(AlertRule {
        id: format!("gina-{:016x}", fnv(&path.join("\u{1f}"))),
        name,
        enabled: true,
        trigger,
        // A trigger that only speaks or only shows text makes no other sound.
        sound: if media { Sound::Chime } else { Sound::None },
        speak,
        say,
        // GINA fires on every match.
        cooldown: 0,
        timer,
        error: None,
    })
}

/// A trigger's text as a rule's trigger, and anything that did not carry.
///
/// Plain text with no tokens stays plain text. Anything else becomes a
/// pattern: plain text is escaped around its tokens, a regular expression is
/// kept as written. Only the tokens are touched, so `\d{3}` stays a count.
fn convert(text: &str, regex: bool) -> (Trigger, Vec<String>) {
    let mut warnings = Vec::new();
    let mut out = String::new();
    let mut seen: Vec<String> = Vec::new();
    let mut any = false;
    let mut rest = text;
    let literal = |s: &str| {
        if regex {
            s.to_string()
        } else {
            regex::escape(s)
        }
    };
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else { break };
        let inner = &after[..close];
        let Some(part) = token(inner, &mut seen, &mut warnings) else {
            // Not a token: a count in a pattern, or a brace in plain text.
            out.push_str(&literal(&rest[..open + close + 2]));
            rest = &after[close + 1..];
            continue;
        };
        any = true;
        out.push_str(&literal(&rest[..open]));
        out.push_str(&part);
        rest = &after[close + 1..];
    }
    out.push_str(&literal(rest));
    let trigger = if !regex && !any {
        Trigger::Contains {
            text: text.to_string(),
        }
    } else {
        if let Err(e) = regex::Regex::new(&norrlog_api::with_you(&out, "You")) {
            let first = e
                .to_string()
                .lines()
                .last()
                .unwrap_or("")
                .trim()
                .to_string();
            warnings.push(format!("its pattern does not work here ({first})"));
        }
        Trigger::Pattern { pattern: out }
    };
    (trigger, warnings)
}

/// What a GINA token stands for, or `None` for braces that are not one.
fn token(inner: &str, seen: &mut Vec<String>, warnings: &mut Vec<String>) -> Option<String> {
    let up = inner.to_uppercase();
    let (kind, rest) = up.split_at(
        up.find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(up.len()),
    );
    let digits = rest
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    let tail = &rest[digits.len()..];
    let name = format!("{kind}{digits}");
    // The first use is captured under its name; a repeat cannot be (the
    // engine has no back-references) and matches the same kind of thing.
    let mut group = |body: &str| {
        if seen.contains(&name) {
            format!("(?:{body})")
        } else {
            seen.push(name.clone());
            format!("(?<{name}>{body})")
        }
    };
    match (kind, tail) {
        // Kept as written: the daemon makes it whoever it follows when it
        // compiles the rule, so the rule follows a change of character.
        ("C", "") if digits.is_empty() => Some("{C}".to_string()),
        ("S", "") => Some(group(".+?")),
        ("N", "") => Some(group(r"\d+")),
        ("N", cmp) if cmp.starts_with(['<', '>', '=']) => {
            warnings.push(format!("{{{inner}}} matches any number, not only {cmp}"));
            Some(group(r"\d+"))
        }
        ("TS", "") if digits.is_empty() => Some(group(".+?")),
        _ => None,
    }
}

/// A package written for GINA, and what could not go into it.
#[derive(Debug, Default)]
pub struct Exported {
    /// The `.gtp`: a zip holding `ShareData.xml`.
    pub bytes: Vec<u8>,
    pub triggers: usize,
    /// One line per alert that did not go across whole: `name: why`.
    pub notes: Vec<String>,
}

/// The group every exported trigger sits in.
pub const EXPORT_GROUP: &str = "Norrlog";

/// Write `rules` as a GINA package. The import, run backwards:
///
/// - a text alert is a plain trigger; a pattern is a regular expression,
///   its `(?<S>.+?)`, `(?<N>\d+)` and `(?<TS>.+?)` turned back into GINA's
///   `{S}`, `{N}` and `{TS}`, and `{C}` left as it is, which GINA reads the
///   same way;
/// - the name is the display text, and the words to say are the
///   text-to-speech;
/// - a countdown is a timer of the same length, restarted by a repeat.
///
/// What GINA has no counterpart for is listed, not dropped quietly: alerts
/// on a typed happening or a fading buff (GINA reads only lines), this
/// app's sounds, and a cooldown.
pub fn to_package(rules: &[AlertRule]) -> Result<Exported, String> {
    use std::io::Write;
    let (xml, triggers, notes) = to_xml(rules);
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        z.start_file("ShareData.xml", zip::write::SimpleFileOptions::default())
            .map_err(|e| e.to_string())?;
        z.write_all(xml.as_bytes()).map_err(|e| e.to_string())?;
        z.finish().map_err(|e| e.to_string())?;
    }
    Ok(Exported {
        bytes: buf.into_inner(),
        triggers,
        notes,
    })
}

/// `ShareData.xml` for `rules`, how many triggers it holds, and the notes.
pub fn to_xml(rules: &[AlertRule]) -> (String, usize, Vec<String>) {
    let mut notes = Vec::new();
    let mut body = String::new();
    let mut count = 0;
    for r in rules {
        let mut note = |why: &str| notes.push(format!("{}: {why}", r.name));
        let (text, regex) = match &r.trigger {
            // GINA reads braces in plain text as tokens, so text holding one
            // goes as an escaped pattern instead.
            Trigger::Contains { text } if text.contains('{') => (regex::escape(text), true),
            Trigger::Contains { text } => (text.clone(), false),
            Trigger::Pattern { pattern } => (to_tokens(pattern), true),
            Trigger::Signal { .. } => {
                note(
                    "watches a typed happening, not a line; GINA has nothing like it, so it was left out",
                );
                continue;
            }
            Trigger::BuffExpiring { .. } => {
                note("watches a buff running out; GINA reads only lines, so it was left out");
                continue;
            }
        };
        if r.sound != Sound::None {
            note("this app's sound does not go across; give it one in GINA");
        }
        if r.cooldown > 0 {
            note("GINA has no cooldown, so it fires on every match");
        }
        if !r.enabled {
            note("is turned off here; GINA will have it on");
        }
        let say = r.say.clone().unwrap_or_else(|| r.name.clone());
        for (what, words) in [("name", &r.name), ("words to say", &say)] {
            if let Some(q) = quotes_beyond_gina(words) {
                note(&format!(
                    "its {what} quote {{{q}}}, which GINA leaves as written"
                ));
            }
        }
        let timer = r.timer > 0;
        let b = |v: bool| if v { "True" } else { "False" };
        body.push_str(&format!(
            "        <Trigger>
          <Name>{name}</Name>
          <TriggerText>{text}</TriggerText>
          <Comments />
          <EnableRegex>{regex}</EnableRegex>
          <UseText>True</UseText>
          <DisplayText>{name}</DisplayText>
          <CopyToClipboard>False</CopyToClipboard>
          <ClipboardText />
          <UseTextToVoice>{speak}</UseTextToVoice>
          <InterruptSpeech>False</InterruptSpeech>
          <TextToVoiceText>{say}</TextToVoiceText>
          <PlayMediaFile>False</PlayMediaFile>
          <TimerType>{timer_type}</TimerType>
          <TimerName>{name}</TimerName>
          <RestartBasedOnTimerName>False</RestartBasedOnTimerName>
          <TimerMillisecondDuration>{ms}</TimerMillisecondDuration>
          <TimerDuration>{secs}</TimerDuration>
          <TimerVisibleDuration>0</TimerVisibleDuration>
          <TimerStartBehavior>RestartTimer</TimerStartBehavior>
          <TimerEndingTime>0</TimerEndingTime>
          <UseTimerEnding>False</UseTimerEnding>
          <UseTimerEnded>False</UseTimerEnded>
          <UseCounterResetTimer>False</UseCounterResetTimer>
          <CounterResetDuration>0</CounterResetDuration>
          <Category>Default</Category>
        </Trigger>
",
            name = xml_escape(&r.name),
            text = xml_escape(&text),
            regex = b(regex),
            speak = b(r.speak),
            say = xml_escape(if r.speak { &say } else { "" }),
            timer_type = if timer { "Timer" } else { "NoTimer" },
            ms = if timer { r.timer as u64 * 1000 } else { 0 },
            secs = if timer { r.timer } else { 0 },
        ));
        count += 1;
    }
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>
<SharedData>
  <TriggerGroups>
    <TriggerGroup>
      <Name>{EXPORT_GROUP}</Name>
      <Comments />
      <SelfCommented>False</SelfCommented>
      <EnableByDefault>True</EnableByDefault>
      <TriggerGroups />
      <Triggers>
{body}      </Triggers>
    </TriggerGroup>
  </TriggerGroups>
</SharedData>
"
    );
    (xml, count, notes)
}

/// A pattern with the captures the import makes out of GINA's tokens
/// turned back into them. Any other named group is kept; .NET reads
/// `(?<name>…)` as this app does, and `(?P<name>…)` is respelled to it.
fn to_tokens(pattern: &str) -> String {
    let token = regex::Regex::new(r"\(\?P?<(S\d*|N\d*|TS)>(\.\+\?|\\d\+)\)").expect("valid");
    let out = token.replace_all(pattern, |c: &regex::Captures| {
        let (name, body) = (&c[1], &c[2]);
        let fits = if name.starts_with('N') {
            body == r"\d+"
        } else {
            body == ".+?"
        };
        if fits {
            format!("{{{name}}}")
        } else {
            c[0].to_string()
        }
    });
    out.replace("(?P<", "(?<")
}

/// A `{name}` quoted in `words` that GINA would not fill in: anything but
/// its own `{S}`, `{N}`, `{TS}` (with a number or not) and `{C}`.
fn quotes_beyond_gina(words: &str) -> Option<String> {
    let q = regex::Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\}").expect("valid");
    q.captures_iter(words).map(|c| c[1].to_string()).find(|n| {
        let up = n.to_uppercase();
        let kind = up.trim_end_matches(|c: char| c.is_ascii_digit());
        !matches!(kind, "S" | "N" | "TS" | "C")
    })
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// FNV-1a, for ids that do not change from one import to the next.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHARE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<SharedData>
  <TriggerGroups>
    <TriggerGroup>
      <Name>Raid</Name>
      <TriggerGroups>
        <TriggerGroup>
          <Name>Nagafen</Name>
          <Triggers>
            <Trigger>
              <Name>Breath</Name>
              <TriggerText>Lord Nagafen rears back</TriggerText>
              <EnableRegex>False</EnableRegex>
              <UseText>True</UseText>
              <DisplayText>BREATH</DisplayText>
              <UseTextToVoice>True</UseTextToVoice>
              <TextToVoiceText>breath incoming</TextToVoiceText>
              <PlayMediaFile>False</PlayMediaFile>
              <TimerType>Timer</TimerType>
              <TimerDuration>0</TimerDuration>
              <TimerMillisecondDuration>30000</TimerMillisecondDuration>
              <TimerEndingTrigger>
                <UseText>True</UseText>
                <DisplayText>breath soon</DisplayText>
              </TimerEndingTrigger>
            </Trigger>
          </Triggers>
        </TriggerGroup>
      </TriggerGroups>
      <Triggers>
        <Trigger>
          <Name>Heal</Name>
          <TriggerText>^{S} begins to cast Complete Heal on {C}\.$</TriggerText>
          <EnableRegex>True</EnableRegex>
          <UseText>True</UseText>
          <DisplayText>{S} is healing you</DisplayText>
          <UseTextToVoice>False</UseTextToVoice>
          <PlayMediaFile>True</PlayMediaFile>
          <TimerType>Stopwatch</TimerType>
        </Trigger>
        <Trigger>
          <Name>Big hit</Name>
          <TriggerText>hits YOU for {N>1000} points</TriggerText>
          <EnableRegex>False</EnableRegex>
          <UseText>False</UseText>
          <UseTextToVoice>True</UseTextToVoice>
          <TextToVoiceText>ouch {N}</TextToVoiceText>
          <TimerType>NoTimer</TimerType>
        </Trigger>
        <Trigger>
          <Name>Count</Name>
          <TriggerText>\d{3} {s} and {S}</TriggerText>
          <EnableRegex>True</EnableRegex>
        </Trigger>
      </Triggers>
    </TriggerGroup>
  </TriggerGroups>
</SharedData>"#;

    fn by_name<'a>(i: &'a Imported, name: &str) -> &'a AlertRule {
        i.rules
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("{name}: {:?}", i.rules))
    }

    #[test]
    fn a_share_package_becomes_alerts() {
        let i = from_xml(SHARE).unwrap();
        assert_eq!((i.rules.len(), i.groups), (4, 2));

        let breath = by_name(&i, "BREATH");
        assert!(
            matches!(&breath.trigger, Trigger::Contains { text } if text == "Lord Nagafen rears back")
        );
        assert_eq!(breath.timer, 30, "milliseconds win over the seconds field");
        assert!(breath.speak && breath.say.as_deref() == Some("breath incoming"));
        assert_eq!((breath.sound, breath.cooldown), (Sound::None, 0));

        let heal = by_name(&i, "{S} is healing you");
        let Trigger::Pattern { pattern } = &heal.trigger else {
            panic!()
        };
        assert_eq!(
            pattern, r"^(?<S>.+?) begins to cast Complete Heal on {C}\.$",
            "{{C}} stays, for the daemon to make whoever it follows"
        );
        assert_eq!(heal.sound, Sound::Chime);

        let hit = by_name(&i, "Big hit");
        let Trigger::Pattern { pattern } = &hit.trigger else {
            panic!()
        };
        assert_eq!(
            pattern, r"hits YOU for (?<N>\d+) points",
            "plain text, escaped around the token"
        );
        assert_eq!(hit.say.as_deref(), Some("ouch {N}"));

        let count = by_name(&i, "Count");
        let Trigger::Pattern { pattern } = &count.trigger else {
            panic!()
        };
        assert_eq!(
            pattern, r"\d{3} (?<S>.+?) and (?:.+?)",
            "a count is not a token; a repeat is not captured twice"
        );

        for want in [
            "Breath: its timer's early warning",
            "Heal: plays its own sound",
            "Heal: a stopwatch",
            "Big hit: {N>1000} matches any number",
        ] {
            assert!(
                i.notes.iter().any(|n| n.starts_with(want)),
                "{want}: {:?}",
                i.notes
            );
        }
    }

    #[test]
    fn the_same_trigger_keeps_its_id_and_groups_tell_apart() {
        let a = from_xml(SHARE).unwrap();
        let b = from_xml(SHARE).unwrap();
        assert_eq!(
            a.rules.iter().map(|r| &r.id).collect::<Vec<_>>(),
            b.rules.iter().map(|r| &r.id).collect::<Vec<_>>()
        );
        let mut ids: Vec<_> = a.rules.iter().map(|r| r.id.clone()).collect();
        ids.dedup();
        assert_eq!(ids.len(), 4);
    }

    #[test]
    fn a_package_is_a_zip_around_the_same_xml() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("ShareData.xml", zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(SHARE.as_bytes()).unwrap();
            z.finish().unwrap();
        }
        let dir = std::env::temp_dir().join(format!("norrlog-gina-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("raid.gtp");
        std::fs::write(&p, buf.into_inner()).unwrap();
        let i = read_file(&p).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(i.rules.len(), 4);
    }

    fn rule(name: &str, trigger: Trigger) -> AlertRule {
        AlertRule {
            id: name.to_lowercase(),
            name: name.into(),
            enabled: true,
            trigger,
            sound: Sound::None,
            speak: false,
            say: None,
            cooldown: 0,
            timer: 0,
            error: None,
        }
    }

    /// What goes out comes back the same through the import, and what GINA
    /// cannot hold is named.
    #[test]
    fn an_export_reads_back_as_the_same_alerts() {
        let mut breath = rule(
            "BREATH",
            Trigger::Contains {
                text: "Lord Nagafen rears back".into(),
            },
        );
        breath.timer = 30;
        breath.speak = true;
        breath.say = Some("breath incoming".into());
        let heal = rule(
            "{S} is healing you",
            Trigger::Pattern {
                pattern: r"^(?<S>.+?) begins to cast Complete Heal on {C}\.$".into(),
            },
        );
        let mut braces = rule(
            "Braces & <more>",
            Trigger::Contains {
                text: "says {hello}".into(),
            },
        );
        braces.sound = Sound::Bell;
        braces.cooldown = 10;
        let charm = rule(
            "Charm broke",
            Trigger::Signal {
                kind: norrlog_api::SignalKind::CharmBroken,
                name: None,
            },
        );
        let rules = vec![breath, heal, braces, charm];

        let (xml, n, notes) = to_xml(&rules);
        assert_eq!(n, 3, "the happening has no GINA form");
        assert!(
            xml.contains(r"<TriggerText>^{S} begins to cast Complete Heal on {C}\.$</TriggerText>"),
            "{xml}"
        );

        let back = from_xml(&xml).unwrap();
        assert_eq!(back.rules.len(), 3, "{:?}", back.notes);
        let b = by_name(&back, "BREATH");
        assert!(
            matches!(&b.trigger, Trigger::Contains { text } if text == "Lord Nagafen rears back")
        );
        assert_eq!(
            (b.timer, b.speak, b.say.as_deref()),
            (30, true, Some("breath incoming"))
        );
        let h = by_name(&back, "{S} is healing you");
        assert!(
            matches!(&h.trigger, Trigger::Pattern { pattern } if pattern == r"^(?<S>.+?) begins to cast Complete Heal on {C}\.$"),
            "{:?}",
            h.trigger
        );
        let br = by_name(&back, "Braces & <more>");
        let Trigger::Pattern { pattern } = &br.trigger else {
            panic!("{:?}", br.trigger)
        };
        assert!(
            regex::Regex::new(pattern)
                .unwrap()
                .is_match("Tharn says {hello}")
        );

        for want in [
            "Charm broke: watches a typed happening",
            "Braces & <more>: this app's sound",
            "Braces & <more>: GINA has no cooldown",
        ] {
            assert!(
                notes.iter().any(|n| n.starts_with(want)),
                "{want}: {notes:?}"
            );
        }
        let pkg = to_package(&rules).unwrap();
        assert!(pkg.bytes.starts_with(b"PK"));
        assert_eq!(from_zip(&pkg.bytes).unwrap(), xml);
    }

    #[test]
    fn captures_turn_back_into_tokens_and_others_stay() {
        assert_eq!(to_tokens(r"(?<N>\d+) of (?<S2>.+?)"), "{N} of {S2}");
        assert_eq!(
            to_tokens(r"(?P<who>\w+) and (?<N>.+?)"),
            r"(?<who>\w+) and (?<N>.+?)"
        );
        assert_eq!(quotes_beyond_gina("{S} hits {who}").as_deref(), Some("who"));
        assert_eq!(quotes_beyond_gina("{s1} {N} {TS} {C}"), None);
    }

    #[test]
    fn what_is_not_gina_says_so() {
        assert!(from_xml("<html/>").is_err());
        assert!(from_xml("not xml").is_err());
    }

    #[test]
    fn a_dotnet_only_pattern_is_imported_and_named() {
        let xml = r#"<SharedData><TriggerGroups><TriggerGroup><Name>g</Name><Triggers><Trigger>
            <Name>Back</Name><TriggerText>(?&lt;a&gt;x)\k&lt;a&gt;</TriggerText><EnableRegex>True</EnableRegex>
            </Trigger></Triggers></TriggerGroup></TriggerGroups></SharedData>"#;
        let i = from_xml(xml).unwrap();
        assert_eq!(i.rules.len(), 1);
        assert!(
            i.notes[0].starts_with("Back: its pattern does not work here"),
            "{:?}",
            i.notes
        );
    }
}
