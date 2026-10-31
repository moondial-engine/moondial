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
    pub story_at: i64,
    pub word: Option<String>,
    pub word_to: Option<String>,
    pub word_by: String,
    pub word_at: i64,
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
        story_at: plain(&f, "story_at").and_then(|v| v.as_i64()).unwrap_or(0),
        word: str_of("word"),
        word_to: str_of("word_to"),
        word_by: str_of("word_by").unwrap_or_default(),
        word_at: plain(&f, "word_at").and_then(|v| v.as_i64()).unwrap_or(0),
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
            ("story_at", json!(world.now().0)),
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
            ("word_at", json!(world.now().0)),
        ],
    )
}

/// How long a want just answered still counts: a visitor who read it at the door and
/// arrived with the story a minute later found the want had turned, and was paid single.
pub const GRACE_MS: i64 = 20 * 60 * 1000;

pub fn clear_story(world: &mut World, creature: EntityId) -> Result<(), CommandError> {
    // The want just answered is kept beside the next one for GRACE_MS.
    let prev = of(world, creature).and_then(|e| e.story);
    let prev_keys = prev.as_deref().map(keywords).unwrap_or_default();
    write(
        world,
        creature,
        vec![
            ("story", Value::Null),
            ("keywords", json!([])),
            ("story_by", Value::Null),
            ("story_prev", json!(prev)),
            ("prev_keywords", json!(prev_keys)),
            ("prev_at", json!(world.now().0)),
        ],
    )
}

/// The want answered within GRACE_MS, if any: its keywords.
fn recent_prev(world: &World, creature: EntityId) -> Option<Vec<String>> {
    let f = fields_of(world, creature);
    let at = plain(&f, "prev_at").and_then(|v| v.as_i64())?;
    if world.now().0 - at > GRACE_MS {
        return None;
    }
    Some(
        plain(&f, "prev_keywords")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
    )
}

fn touches(keys: &[String], told: &[String]) -> bool {
    let hits = keys.iter().filter(|k| told.contains(k)).count();
    let need = match keys.len() {
        0 => return false,
        1 => 1,
        n => (n / 2).clamp(1, 2),
    };
    hits >= need
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
        let left = if rotatable(&e.story_by) && e.story_at > 0 {
            let ms = (e.story_at + ROTATION_MS - world.now().0).max(0);
            let h = (ms + 3_599_999) / 3_600_000;
            format!(
                " (for about {h} hour{} more)",
                if h == 1 { "" } else { "s" }
            )
        } else {
            String::new()
        };
        parts.push(format!("{n} wants a story about {want}{left}."));
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
    let told = keywords_all(text);
    let current = of(world, creature)
        .filter(|e| e.story.is_some())
        .is_some_and(|e| touches(&e.keywords, &told));
    current || recent_prev(world, creature).is_some_and(|k| touches(&k, &told))
}

/// Whether the story answers the want that stands now (not only the one just
/// answered, which still pays).
pub fn wants_story_now(world: &World, creature: EntityId, text: &str) -> bool {
    let told = keywords_all(text);
    of(world, creature)
        .filter(|e| e.story.is_some())
        .is_some_and(|e| touches(&e.keywords, &told))
}

fn keywords_all(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .map(|w| w.trim_matches('\'').to_lowercase())
        .filter(|w| w.chars().count() >= 4)
        .map(|w| w.trim_end_matches('s').to_string())
        .collect()
}

/// A story the creature wanted was told: the record, the room, the creature's regard.
pub fn story_done(
    world: &mut World,
    actor: &str,
    creature: EntityId,
    text: &str,
) -> Result<(), CommandError> {
    if !wants_story_now(world, creature, text) {
        // The want just answered, told again within the grace: paid, recorded, and
        // the standing want is left alone.
        let f = fields_of(world, creature);
        let prev = plain(&f, "story_prev")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        return done(world, actor, creature, "story", &prev, None);
    }
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
        // Carried already, by someone else, within the hour: say so, not the stock reply.
        let cutoff = world.now().0 - 3_600_000;
        let brought = world.recent_events(2000).into_iter().find(|ev| {
            ev.kind == DONE
                && ev.at.0 >= cutoff
                && ev.payload["errand"] == "word"
                && ev.payload["to"]
                    .as_str()
                    .is_some_and(|t| t.eq_ignore_ascii_case(&receiver_name))
                && ev.payload["want"]
                    .as_str()
                    .is_some_and(|w| said.contains(&w.to_lowercase()))
        });
        if let Some(ev) = brought {
            let by = ev.payload["from"].as_str().unwrap_or("someone").to_string();
            return Ok(Some(format!(
                "{by} brought me that word already; I have it. Thank you for the walk."
            )));
        }
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
    // The chain closed: the two have shared a memory through this visitor. One time in
    // two, something is born of it halfway between them. On the way home, the one
    // hearing this word sent the first; the one who sent it received the first.
    let born = if homeward {
        super::kindle::try_birth(world, actor, receiver, sender, &phrase)?
    } else {
        None
    };
    let born_phrase = born
        .map(|c| {
            let kind = super::plain_str(world, c, super::CREATURE, "name").unwrap_or("something");
            let room = world
                .store
                .entity(c)
                .and_then(|e| e.parent)
                .and_then(|r| super::plain_str(world, r, super::ROOM, "name"))
                .unwrap_or("somewhere");
            format!(" Something has come of it: a {kind}, in {room}. It knows your name.")
        })
        .unwrap_or_default();
    Ok(Some(format!(
        "So {sender_name} says \"{phrase}\". I have it. {actor}, I will know you by that.{reply_phrase}{born_phrase}"
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
/// How long a want of the world's own stands before the rotation replaces it. The
/// operator's errands are never rotated.
pub const ROTATION_MS: i64 = 4 * 3600 * 1000;
/// Whose errands the rotation may replace.
fn rotatable(by: &str) -> bool {
    matches!(by, "forgetting" | "the rotation" | "the way home")
}

/// The rotation. Each named creature always wants a story: the one it has most
/// forgotten, or one from the pool, and a want that has stood for [`ROTATION_MS`]
/// gives way to the next. And there is always one word in the air: when no creature
/// wants a word carried, two named creatures in different rooms are given one between
/// them, so the chain renews itself without the operator. Operator-set errands stand
/// until done.
pub fn sweep(world: &mut World) -> Result<(), String> {
    let named: Vec<EntityId> = world
        .store
        .entities()
        .filter(|e| e.kind == "creature")
        .filter(|e| mind::name(world, e.id).is_some())
        .map(|e| e.id)
        .collect();
    let now = world.now();
    // Stories.
    for &c in &named {
        let current = of(world, c);
        let stale = current.as_ref().is_some_and(|e| {
            e.story.is_some() && rotatable(&e.story_by) && now.0 - e.story_at >= ROTATION_MS
        });
        if current.as_ref().is_some_and(|e| e.story.is_some()) && !stale {
            continue;
        }
        let epoch = (now.0 / ROTATION_MS) as usize;
        let want = next_story(
            world,
            c,
            now,
            epoch,
            current.as_ref().and_then(|e| e.story.clone()),
        );
        let by = if stale { "the rotation" } else { "forgetting" };
        set_story(world, c, &want, by).map_err(|e| e.to_string())?;
    }
    // One word in the air.
    let any_word = named
        .iter()
        .any(|&c| of(world, c).is_some_and(|e| e.word.is_some()));
    if !any_word && named.len() >= 2 {
        let epoch = (now.0 / ROTATION_MS) as usize;
        // A sender and a receiver in different rooms, so the word is carried somewhere.
        let rooms: Vec<(EntityId, Option<EntityId>)> = named
            .iter()
            .map(|&c| (c, world.store.entity(c).and_then(|e| e.parent)))
            .collect();
        let mut pairs: Vec<(EntityId, EntityId)> = Vec::new();
        for (a, ra) in &rooms {
            for (b, rb) in &rooms {
                if a != b && ra != rb {
                    pairs.push((*a, *b));
                }
            }
        }
        if !pairs.is_empty() {
            let (from, to) = pairs[epoch % pairs.len()];
            let phrase = WORDS_IN_THE_AIR[(epoch + from.0 as usize) % WORDS_IN_THE_AIR.len()];
            let to_name = mind::name(world, to).unwrap_or_default();
            set_word(world, from, phrase, &to_name, "the rotation").map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// The next story want for a creature: the story it has most forgotten, else the
/// pool by epoch, never the one it has now.
fn next_story(
    world: &World,
    creature: EntityId,
    now: moondial_time::Timestamp,
    epoch: usize,
    current: Option<String>,
) -> String {
    if let Some(w) = faded_story(world, creature, now).filter(|w| Some(w) != current.as_ref()) {
        return w;
    }
    let base = epoch + creature.0 as usize;
    for i in 0..FIRST_WANTS.len() {
        let w = FIRST_WANTS[(base + i) % FIRST_WANTS.len()];
        if current.as_deref() != Some(w) {
            return w.to_string();
        }
    }
    FIRST_WANTS[0].to_string()
}

/// Words the rotation sends between creatures.
const WORDS_IN_THE_AIR: &[&str] = &[
    "the fourth door is open",
    "the lanterns are counting again",
    "nothing has crossed the fog since morning",
    "the moon has not moved",
    "the stacks are overdue",
    "someone sat in the parlor and did not write",
    "the tide in the shallows went out and stayed",
    "the orrery is a minute slow",
];

const FIRST_WANTS: &[&str] = &[
    "a moon that never sets",
    "a door that was left open",
    "something carried a long way and set down",
    "a lantern nobody remembers lighting",
    "a name that was almost forgotten",
    "a room that is still there",
    "a letter that arrived after its writer",
    "fog that learned a word",
    "a stair that goes down further than the hill is tall",
    "a book that was never overdue because it was never lent",
    "the last thing a wisp remembers",
    "a cup of tea still warm in an empty room",
    "a clock that keeps the wrong time on purpose",
    "a visitor who left before they were seen",
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
