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

/// What a named creature wants: a story about something, a word to carry to another
/// named creature, or both at once. The two parts are independent, so carrying a word
/// to a creature does not cost the story it wanted: a visitor did both in one message
/// and was paid single for the story.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Errand {
    pub story: Option<String>,
    pub keywords: Vec<String>,
    pub story_by: String,
    pub word: Option<String>,
    pub word_to: Option<String>,
    pub word_by: String,
}

fn fields_of(world: &World, creature: EntityId) -> BTreeMap<String, Field> {
    world
        .store
        .entity(creature)
        .and_then(|e| e.components.get(ERRAND))
        .map(|c| c.fields.clone())
        .unwrap_or_default()
}

fn plain(fields: &BTreeMap<String, Field>, k: &str) -> Option<Value> {
    match fields.get(k) {
        Some(Field::Plain(v)) if !v.is_null() => Some(v.clone()),
        _ => None,
    }
}

pub fn of(world: &World, creature: EntityId) -> Option<Errand> {
    let f = fields_of(world, creature);
    let str_of = |k: &str| plain(&f, k).and_then(|v| v.as_str().map(str::to_string));
    let e = Errand {
        story: str_of("story"),
        keywords: plain(&f, "keywords")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        story_by: str_of("story_by").unwrap_or_default(),
        word: str_of("word"),
        word_to: str_of("word_to"),
        word_by: str_of("word_by").unwrap_or_default(),
    };
    if e.story.is_none() && e.word.is_none() {
        return None;
    }
    Some(e)
}

fn write(
    world: &mut World,
    creature: EntityId,
    changes: Vec<(&str, Value)>,
) -> Result<(), CommandError> {
    let mut fields = fields_of(world, creature);
    for (k, v) in changes {
        fields.insert(k.to_string(), Field::Plain(v));
    }
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

pub fn set_story(
    world: &mut World,
    creature: EntityId,
    want: &str,
    by: &str,
) -> Result<(), CommandError> {
    write(
        world,
        creature,
        vec![
            ("story", json!(want)),
            ("keywords", json!(keywords(want))),
            ("story_by", json!(by)),
        ],
    )
}

pub fn set_word(
    world: &mut World,
    creature: EntityId,
    phrase: &str,
    to: &str,
    by: &str,
) -> Result<(), CommandError> {
    write(
        world,
        creature,
        vec![
            ("word", json!(phrase)),
            ("word_to", json!(to)),
            ("word_by", json!(by)),
        ],
    )
}

pub fn clear_story(world: &mut World, creature: EntityId) -> Result<(), CommandError> {
    write(
        world,
        creature,
        vec![
            ("story", Value::Null),
            ("keywords", json!([])),
            ("story_by", Value::Null),
        ],
    )
}

pub fn clear_word(world: &mut World, creature: EntityId) -> Result<(), CommandError> {
    write(
        world,
        creature,
        vec![
            ("word", Value::Null),
            ("word_to", Value::Null),
            ("word_by", Value::Null),
        ],
    )
}

pub fn clear(world: &mut World, creature: EntityId) -> Result<(), CommandError> {
    if of(world, creature).is_none() {
        return Ok(());
    }
    clear_story(world, creature)?;
    clear_word(world, creature)
}

/// The way from one room to another, as exits to take, by the shortest route.
pub fn route(world: &World, from: EntityId, to: EntityId) -> Option<Vec<String>> {
    use std::collections::{HashMap, VecDeque};
    let mut prev: HashMap<EntityId, (EntityId, String)> = HashMap::new();
    let mut queue = VecDeque::from([from]);
    let mut seen = std::collections::HashSet::from([from]);
    while let Some(r) = queue.pop_front() {
        if r == to {
            let mut path = Vec::new();
            let mut at = r;
            while at != from {
                let (p, via) = prev[&at].clone();
                path.push(via);
                at = p;
            }
            path.reverse();
            return Some(path);
        }
        for (via, next) in super::rooms::exits_of(world, r) {
            if seen.insert(next) {
                prev.insert(next, (r, via));
                queue.push_back(next);
            }
        }
    }
    None
}

/// The errand as `look` and the cloak say it, if the creature has a name and a want:
/// one sentence per part, the word with the way there.
pub fn describe(world: &World, creature: EntityId) -> Option<String> {
    let n = mind::name(world, creature)?;
    let e = of(world, creature)?;
    let mut parts = Vec::new();
    if let Some(want) = &e.story {
        parts.push(format!("{n} wants a story about {want}."));
    }
    if let (Some(phrase), Some(to)) = (&e.word, &e.word_to) {
        let there = mind::find_named(world, to).and_then(|(_, r)| r);
        let place = there
            .and_then(|r| plain_str(world, r, ROOM, "name"))
            .map(|p| format!(" in the {}", p.trim_start_matches("The ")))
            .unwrap_or_default();
        let here = world.store.entity(creature).and_then(|c| c.parent);
        let way = match (here, there) {
            (Some(a), Some(b)) if a == b => " (it is here)".to_string(),
            (Some(a), Some(b)) => route(world, a, b)
                .filter(|p| !p.is_empty())
                .map(|p| {
                    format!(
                        " (travel {}, then tell {to} \"{phrase}\")",
                        p.join(", then ")
                    )
                })
                .unwrap_or_default(),
            _ => String::new(),
        };
        parts.push(format!(
            "{n} wants word carried to {to}{place}: \"{phrase}\"{way}."
        ));
    }
    Some(parts.join(" "))
}

/// Whether a story told to `creature` is the one it wanted.
pub fn wants_story(world: &World, creature: EntityId, text: &str) -> bool {
    let Some(e) = of(world, creature) else {
        return false;
    };
    if e.story.is_none() {
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
    let Some(want) = of(world, creature).and_then(|e| e.story) else {
        return Ok(());
    };
    done(world, actor, creature, "story", &want, None)?;
    clear_story(world, creature)
}

/// What a creature says back to a word carried to it, if the text carries one that was
/// wanted: the carrier earns, the sender's word is done, and the receiver hands back a
/// word for the way home, unless this was the way home already.
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
            er.word_to
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case(&receiver_name))
                && er
                    .word
                    .as_deref()
                    .is_some_and(|w| said.contains(&w.to_lowercase()))
        })
        .collect();
    let Some((sender, e)) = senders.into_iter().next() else {
        return Ok(None);
    };
    let phrase = e.word.clone().unwrap_or_default();
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
    done(world, actor, sender, "word", &phrase, Some(&receiver_name))?;
    clear_word(world, sender)?;
    // The way home: a word for the sender, once. A return leg does not spawn another,
    // or the two would send a visitor back and forth forever; and a word the receiver
    // already wants carried is not replaced.
    let homeward = e.word_by == "the way home";
    let has_word = of(world, receiver).is_some_and(|er| er.word.is_some());
    let reply_phrase = if !homeward && !has_word {
        let back =
            RETURN_WORDS[(sender.0 as usize * 31 + receiver.0 as usize) % RETURN_WORDS.len()];
        set_word(world, receiver, back, &sender_name, "the way home")?;
        format!(" Carry this back to {sender_name}: \"{back}\"")
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
        "So {sender_name} says \"{phrase}\". I have it. {actor}, I will know you by that.{reply_phrase}"
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
    errand: &str,
    want: &str,
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
            json!({"from": actor, "creature": creature.0, "name": name, "kind": kind, "errand": errand, "want": want, "to": to}),
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
        if of(world, c).is_some_and(|e| e.story.is_some()) {
            continue;
        }
        let want = faded_story(world, c, now).unwrap_or_else(|| {
            let n = mind::name(world, c).unwrap_or_default();
            let h = n
                .bytes()
                .fold(7usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
            FIRST_WANTS[h % FIRST_WANTS.len()].to_string()
        });
        set_story(world, c, &want, "forgetting").map_err(|e| e.to_string())?;
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
    // Not a want already answered: the same faded story came back as the next want the
    // moment a visitor told it. What was done for this creature lately is skipped.
    let answered: Vec<String> = world
        .recent_events(2000)
        .iter()
        .filter(|e| e.kind == DONE && e.payload["creature"] == creature.0)
        .filter_map(|e| e.payload["want"].as_str().map(str::to_lowercase))
        .collect();
    for (_, gist) in stories {
        // The things in the story, not what was done to them: a want "about knight and
        // tried" asked for the wrong half of the sentence.
        let all = keywords(&gist);
        let mut words: Vec<String> = all
            .iter()
            .filter(|w| !(w.ends_with("ed") || w.ends_with("ing") || w.ends_with("ly")))
            .cloned()
            .collect();
        if words.len() < 2 {
            words = all;
        }
        if words.len() < 2 {
            continue;
        }
        let want = format!(
            "{} and {}, like the one it has mostly forgotten",
            words[0], words[1]
        );
        if answered.iter().any(|a| a == &want.to_lowercase()) {
            continue;
        }
        return Some(want);
    }
    None
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
            match args.str("to").map(str::trim).filter(|t| !t.is_empty()) {
                Some(to) => {
                    let Some(to) = mind::find_named(w, to).and_then(|(c, _)| mind::name(w, c)) else {
                        return Err(CommandError::precondition(format!("No creature is named {to}.")));
                    };
                    set_word(w, creature, &want, &to, "the operator")?;
                }
                None => set_story(w, creature, &want, "the operator")?,
            };
            let line = describe(w, creature).unwrap_or_default();
            let e = of(w, creature).unwrap_or_default();
            Ok(Outcome::new(
                line,
                json!({"creature": creature.0, "errand": {"story": e.story, "keywords": e.keywords, "word": e.word, "word_to": e.word_to}}),
            ))
        }),
    );
}
