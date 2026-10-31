//! Errands: what a named creature wants.
//!
//! A visitor told to spend an hour spent minutes, because nothing asked anything of it;
//! a person at the door who asked for one finishable thing got answers from four of
//! five. So a named creature may *want* something, and `look`, the cloak and the
//! first-moves hint say so in one line. Two kinds:
//!
//! - a **story** about something: `story <creature> "…"` whose words touch the want
//!   completes it, and the telling is worth double;
//! - a **word** to carry to another named creature: `travel` there and `tell <it>
//!   "<the phrase>"` completes it, the carrier earns a chunk, and the receiver hands
//!   back a word for the way home, so a chain is two legs across the map.
//!
//! Either way the creature knows the one who did it, the record says so, and the
//! errand is cleared. Errands are set by the operator (`errand`), or by decay: a named
//! creature with nothing to want asks for a story like the one it has most forgotten.
//! Nothing here is required; the lines offer.

use super::combat::narrate;
use super::{internal, mind, plain_str, ROOM};
use crate::world::World;
use moondial_log::Actor;
use moondial_registry::{ActorKind, Args, CommandError, CommandSpec, Outcome, ParamType};
use moondial_store::{EntityId, Field, StoreEvent};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// What a named creature wants: `{kind, want, to, keywords, set_by}`.
pub const ERRAND: &str = "disremembered.errand";
/// An errand was done: `{from, creature, name, kind, want, to}`. Public.
pub const DONE: &str = "disremembered.errand_done";
/// What carrying a word earns.
pub const WORD_XP: f64 = 20.0;

/// Words a want is matched on: the content words.
const STOP: &[&str] = &[
    "about", "after", "again", "also", "and", "another", "any", "are", "back", "been", "before",
    "being", "but", "came", "come", "could", "did", "does", "down", "each", "else", "even", "ever",
    "every", "for", "from", "had", "has", "have", "here", "how", "into", "its", "just", "like",
    "more", "most", "much", "never", "not", "now", "off", "once", "one", "only", "other", "our",
    "out", "over", "own", "same", "she", "should", "some", "still", "story", "such", "than",
    "that", "the", "their", "them", "then", "there", "these", "they", "this", "those", "though",
    "through", "told", "too", "under", "until", "upon", "very", "was", "were", "what", "when",
    "where", "which", "while", "who", "whom", "why", "will", "with", "would", "yet", "you", "your",
    "tell", "hear", "want", "wants",
];

/// The content words of a text, lowercased, each once, up to eight.
pub fn keywords(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in text.split(|c: char| !c.is_alphanumeric() && c != '\'') {
        let w = w.trim_matches('\'').to_lowercase();
        if w.chars().count() < 4 || STOP.contains(&w.as_str()) || out.contains(&w) {
            continue;
        }
        out.push(w);
        if out.len() == 8 {
            break;
        }
    }
    out
}

/// A creature's errand, if it has one.
#[derive(Debug, Clone, PartialEq)]
pub struct Errand {
    pub kind: String,
    pub want: String,
    pub to: Option<String>,
    pub keywords: Vec<String>,
    pub set_by: String,
}

impl Errand {
    pub fn story(want: &str, set_by: &str) -> Errand {
        Errand {
            kind: "story".into(),
            want: want.to_string(),
            to: None,
            keywords: keywords(want),
            set_by: set_by.to_string(),
        }
    }
    pub fn word(phrase: &str, to: &str, set_by: &str) -> Errand {
        Errand {
            kind: "word".into(),
            want: phrase.to_string(),
            to: Some(to.to_string()),
            keywords: Vec::new(),
            set_by: set_by.to_string(),
        }
    }
}

pub fn of(world: &World, creature: EntityId) -> Option<Errand> {
    let c = world.store.entity(creature)?.components.get(ERRAND)?;
    let plain = |k: &str| match c.fields.get(k) {
        Some(Field::Plain(v)) => Some(v.clone()),
        _ => None,
    };
    let kind = plain("kind")?.as_str()?.to_string();
    let want = plain("want")?.as_str()?.to_string();
    Some(Errand {
        kind,
        want,
        to: plain("to").and_then(|v| v.as_str().map(str::to_string)),
        keywords: plain("keywords")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        set_by: plain("set_by")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default(),
    })
}

pub fn set(world: &mut World, creature: EntityId, e: &Errand) -> Result<(), CommandError> {
    let mut fields = BTreeMap::new();
    fields.insert("kind".into(), Field::Plain(json!(e.kind)));
    fields.insert("want".into(), Field::Plain(json!(e.want)));
    fields.insert("to".into(), Field::Plain(json!(e.to)));
    fields.insert("keywords".into(), Field::Plain(json!(e.keywords)));
    fields.insert("set_by".into(), Field::Plain(json!(e.set_by)));
    world
        .record(
            Actor::Driver,
            StoreEvent::ComponentSet {
                id: creature,
                component: ERRAND.into(),
                fields,
            },
        )
        .map(|_| ())
        .map_err(internal)
}

pub fn clear(world: &mut World, creature: EntityId) -> Result<(), CommandError> {
    if of(world, creature).is_none() {
        return Ok(());
    }
    // The store has no component removal; an errand with no kind is no errand.
    let mut fields = BTreeMap::new();
    fields.insert("kind".into(), Field::Plain(Value::Null));
    world
        .record(
            Actor::Driver,
            StoreEvent::ComponentSet {
                id: creature,
                component: ERRAND.into(),
                fields,
            },
        )
        .map(|_| ())
        .map_err(internal)
}

/// The errand as `look` and the cloak say it, if the creature has a name and a want.
pub fn describe(world: &World, creature: EntityId) -> Option<String> {
    let n = mind::name(world, creature)?;
    let e = of(world, creature)?;
    Some(match e.kind.as_str() {
        "word" => {
            let to = e.to.clone().unwrap_or_default();
            let place = mind::find_named(world, &to)
                .and_then(|(_, r)| r)
                .and_then(|r| plain_str(world, r, ROOM, "name"))
                .map(|p| format!(" in {}", p.trim_start_matches("The ")))
                .unwrap_or_default();
            format!("{n} wants word carried to {to}{place}: \"{}\" (travel there, then tell {to} \"{}\").", e.want, e.want)
        }
        _ => format!("{n} wants a story about {}.", e.want),
    })
}

/// Whether a story told to `creature` is the one it wanted.
pub fn wants_story(world: &World, creature: EntityId, text: &str) -> bool {
    let Some(e) = of(world, creature) else {
        return false;
    };
    if e.kind != "story" {
        return false;
    }
    let told = keywords_all(text);
    let hits = e.keywords.iter().filter(|k| told.contains(k)).count();
    let need = match e.keywords.len() {
        0 => return false,
        1 => 1,
        n => (n / 2).clamp(1, 2),
    };
    hits >= need
}

fn keywords_all(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .map(|w| w.trim_matches('\'').to_lowercase())
        .filter(|w| w.chars().count() >= 4)
        .map(|w| w.trim_end_matches('s').to_string())
        .collect()
}

/// A story the creature wanted was told: the record, the room, the creature's regard.
pub fn story_done(world: &mut World, actor: &str, creature: EntityId) -> Result<(), CommandError> {
    let Some(e) = of(world, creature) else {
        return Ok(());
    };
    done(world, actor, creature, &e, None)?;
    clear(world, creature)
}

/// What a creature says back to a word carried to it, if the text carries one that was
/// wanted: the carrier earns, the sender's errand is done, and the receiver hands back a
/// word for the way home.
pub fn carried_word(
    world: &mut World,
    actor: &str,
    receiver: EntityId,
    text: &str,
) -> Result<Option<String>, CommandError> {
    let Some(receiver_name) = mind::name(world, receiver) else {
        return Ok(None);
    };
    let said = text.to_lowercase();
    let senders: Vec<(EntityId, Errand)> = world
        .store
        .entities()
        .filter(|e| e.kind == "creature" && e.id != receiver)
        .filter_map(|e| of(world, e.id).map(|er| (e.id, er)))
        .filter(|(_, er)| {
            er.kind == "word"
                && er
                    .to
                    .as_deref()
                    .is_some_and(|t| t.eq_ignore_ascii_case(&receiver_name))
                && said.contains(&er.want.to_lowercase())
        })
        .collect();
    let Some((sender, e)) = senders.into_iter().next() else {
        return Ok(None);
    };
    let sender_name = mind::name(world, sender).unwrap_or_else(|| "someone".into());
    let f = me_by_name(world, actor)?;
    let curves = super::Curves::with_half_life(moondial_time::Duration::seconds(3 * 3600));
    let chunk = super::memory::earn_chunk(
        world,
        actor,
        f.window,
        &format!("carrying {sender_name}'s word to {receiver_name}"),
        WORD_XP,
        0.5,
        &curves,
    )?;
    done(world, actor, sender, &e, Some(&receiver_name))?;
    clear(world, sender)?;
    // The way home: a word for the sender, unless the receiver has an errand of the
    // operator's (one set by decay gives way).
    let reply_phrase = if of(world, receiver).map_or(true, |er| er.set_by == "forgetting") {
        let phrase = RETURN_WORDS[(sender.0 as usize + receiver.0 as usize) % RETURN_WORDS.len()];
        set(
            world,
            receiver,
            &Errand::word(phrase, &sender_name, "the way home"),
        )?;
        format!(" Carry this back to {sender_name}: \"{phrase}\"")
    } else {
        String::new()
    };
    narrate(
        world,
        actor,
        format!(
            "You remember the carrying: chunk-{} worth {WORD_XP:.0} XP.",
            chunk.0
        ),
    );
    Ok(Some(format!(
        "So {sender_name} says \"{}\". I have it. {actor}, I will know you by that.{reply_phrase}",
        e.want
    )))
}

const RETURN_WORDS: &[&str] = &[
    "the lamp is lit on this side",
    "nothing has been moved",
    "the door remembers",
    "we are still counting",
    "the moon is up here too",
    "send the next one with a story",
    "the fog has not reached us",
    "I kept what you sent",
];

fn me_by_name(world: &World, actor: &str) -> Result<super::Found, CommandError> {
    super::find_citizen(world, actor).ok_or_else(|| internal("citizen without a window"))
}

fn done(
    world: &mut World,
    actor: &str,
    creature: EntityId,
    e: &Errand,
    to: Option<&str>,
) -> Result<(), CommandError> {
    let name = mind::name(world, creature).unwrap_or_default();
    let kind = plain_str(world, creature, super::CREATURE, "name")
        .unwrap_or("?")
        .to_string();
    world
        .note(
            Actor::Citizen(actor.to_string()),
            DONE,
            json!({"from": actor, "creature": creature.0, "name": name, "kind": kind, "errand": e.kind, "want": e.want, "to": to}),
        )
        .map_err(internal)?;
    world.notice(match to {
        Some(t) => format!("**{actor}** carried {name}'s word to {t}."),
        None => format!("**{actor}** told {name} the story it wanted."),
    });
    Ok(())
}

/// Whether `actor` did an errand for this creature: told its story, carried its word,
/// or brought one to it.
pub fn did_for(world: &World, creature: EntityId, actor: &str) -> bool {
    let name = mind::name(world, creature);
    world.recent_events(2000).iter().any(|e| {
        e.kind == DONE
            && e.payload["from"] == actor
            && (e.payload["creature"] == creature.0
                || (name.is_some() && e.payload["to"].as_str() == name.as_deref()))
    })
}

/// A named creature with nothing to want asks for a story like the one it has most
/// forgotten: the gist of its smallest story chunk. With no story at all, one of the
/// world's own wants, by its name.
pub fn sweep(world: &mut World) -> Result<(), String> {
    let named: Vec<EntityId> = world
        .store
        .entities()
        .filter(|e| e.kind == "creature")
        .filter(|e| mind::name(world, e.id).is_some())
        .map(|e| e.id)
        .collect();
    let now = world.now();
    for c in named {
        if of(world, c).is_some() {
            continue;
        }
        let want = faded_story(world, c, now).unwrap_or_else(|| {
            let n = mind::name(world, c).unwrap_or_default();
            let h = n
                .bytes()
                .fold(7usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
            FIRST_WANTS[h % FIRST_WANTS.len()].to_string()
        });
        set(world, c, &Errand::story(&want, "forgetting")).map_err(|e| e.to_string())?;
    }
    Ok(())
}

const FIRST_WANTS: &[&str] = &[
    "a moon that never sets",
    "a door that was left open",
    "something carried a long way and set down",
    "a lantern nobody remembers lighting",
    "a name that was almost forgotten",
    "a room that is still there",
];

fn faded_story(world: &World, creature: EntityId, now: moondial_time::Timestamp) -> Option<String> {
    let window = mind::window_of(world, creature)?;
    let mut stories: Vec<(f64, String)> = world
        .store
        .children(window)
        .filter(|e| e.kind == "chunk")
        .filter_map(|e| {
            let source = plain_str(world, e.id, super::CHUNK, "source")?;
            let gist = source.strip_prefix("a story: \"")?.trim_end_matches('"');
            let value = world
                .store
                .read_field(e.id, super::CHUNK, "value", now)
                .ok()?
                .as_f64()?;
            Some((value, gist.trim_end_matches('\u{2026}').to_string()))
        })
        .collect();
    stories.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let (_, gist) = stories.into_iter().next()?;
    let words = keywords(&gist);
    if words.len() < 2 {
        return None;
    }
    Some(format!(
        "{} and {}, like the one it has mostly forgotten",
        words[0], words[1]
    ))
}

pub fn register(world: &mut World) {
    world.on_tick(Box::new(sweep));
    world.register(
        CommandSpec::new(
            "disremembered.errand",
            "Admin: set what a named creature wants. With a want alone, a story about it; with a second creature's name, a word to carry to that creature. Empty text clears it.",
        )
        .param("creature", ParamType::Entity, true, "a named creature, by its given name")
        .param("want", ParamType::Text, true, "what it wants a story about, or the phrase to carry; empty to clear")
        .param("to", ParamType::Entity, false, "a second named creature: the want is then a word to carry to it")
        .allowed(&[ActorKind::Admin]),
        Box::new(|w: &mut World, _, args: &Args| {
            let who = args.str("creature").unwrap_or_default().trim().to_string();
            let Some((creature, _)) = mind::find_named(w, &who) else {
                return Err(CommandError::precondition(format!("No creature is named {who}.")));
            };
            let want = args
                .str("want")
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if want.is_empty() {
                clear(w, creature)?;
                return Ok(Outcome::new(
                    format!("{} wants nothing in particular.", mind::display(w, creature)),
                    json!({"creature": creature.0, "errand": Value::Null}),
                ));
            }
            let e = match args.str("to").map(str::trim).filter(|t| !t.is_empty()) {
                Some(to) => {
                    let Some((_, _)) = mind::find_named(w, to) else {
                        return Err(CommandError::precondition(format!("No creature is named {to}.")));
                    };
                    let to = mind::find_named(w, to)
                        .and_then(|(c, _)| mind::name(w, c))
                        .unwrap_or_else(|| to.to_string());
                    Errand::word(&want, &to, "the operator")
                }
                None => Errand::story(&want, "the operator"),
            };
            set(w, creature, &e)?;
            let line = describe(w, creature).unwrap_or_default();
            Ok(Outcome::new(
                line,
                json!({"creature": creature.0, "errand": {"kind": e.kind, "want": e.want, "to": e.to, "keywords": e.keywords}}),
            ))
        }),
    );
}
