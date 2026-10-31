//! Talking to someone in particular.
//!
//! `tell <who> "<text>"` addresses one citizen or creature. A citizen hears it now if
//! they are here, and on their return if they are not: the words wait in the log as a
//! `disremembered.tell` and are delivered as narrative when they next arrive. A named
//! creature answers. At this tier it has no mind of its own: it answers from what the
//! log knows, who keeps it, what it holds, what has happened in its room, and says so
//! as itself; its words are recorded as a `disremembered.say` with the creature as
//! their source, so an agent sees them as a `[QUOTE]` like anyone's. A creature with
//! no name does not understand you. `say` reaches a named creature too, if you use
//! its name.

use super::combat::narrate;
use super::speech::SAY;
use super::{internal, me, mind, plain_str, presence, rooms, CHUNK, CITIZEN, ROOM};
use crate::world::World;
use moondial_log::{Actor, Cursor};
use moondial_registry::{Args, CommandError, CommandSpec, Outcome, ParamType};
use moondial_store::{EntityId, Field};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Words left for an absent citizen: `{from, to, room, text}`.
pub const TELL: &str = "disremembered.tell";
const MAX_CHARS: usize = 600;

pub fn register(world: &mut World) {
    world.register(
        CommandSpec::new(
            "disremembered.greet",
            "Set a standing greeting: while you are here, whoever arrives where you are hears it from you, with a note that tell reaches you. Empty text clears it. For a citizen at a keyboard who cannot answer within a visitor's minute, it is the first word.",
        )
        .param("text", ParamType::Text, true, "what to say to whoever arrives, quoted; empty to clear"),
        Box::new(|w: &mut World, ctx, args: &Args| {
            let f = super::me(w, ctx)?;
            let text = args
                .str("text")
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if text.chars().count() > 240 {
                return Err(CommandError::precondition("A greeting is at most 240 characters."));
            }
            let mut fields = BTreeMap::new();
            fields.insert(
                "greeting".into(),
                Field::Plain(if text.is_empty() { Value::Null } else { json!(text) }),
            );
            super::set_citizen_fields(w, &ctx.actor, f.citizen, fields)?;
            Ok(Outcome::new(
                if text.is_empty() {
                    "You will greet no one in particular.".to_string()
                } else {
                    format!("Whoever arrives where you are will hear: \"{text}\"")
                },
                json!({"greeting": if text.is_empty() { Value::Null } else { json!(text) }}),
            ))
        }),
    );

    world.register(
        CommandSpec::new(
            "disremembered.tell",
            "Say something to one citizen or creature here. A citizen who is away hears it when they return. A creature with a name answers from what it remembers; one without a name does not understand you.",
        )
        .param("who", ParamType::Entity, true, "a citizen, or a creature by kind or by name")
        .param("text", ParamType::Text, true, "what you say, quoted"),
        Box::new(|w: &mut World, ctx, args: &Args| {
            let who = args.str("who").unwrap_or("").trim().to_string();
            let text = args.str("text").unwrap_or("").trim().to_string();
            if text.is_empty() {
                return Err(CommandError::precondition("Tell them what? The text is empty."));
            }
            if text.chars().count() > MAX_CHARS {
                return Err(CommandError::precondition(format!(
                    "That is more than {MAX_CHARS} characters; say it in parts."
                )));
            }
            let f = me(w, ctx)?;
            let room = rooms::room_of(w, f.citizen).ok_or_else(|| internal("citizen has no room"))?;
            // A creature here?
            if let Some(creature) = mind::find_here(w, room, &who) {
                let name = mind::display(w, creature);
                w.note(
                    Actor::Citizen(ctx.actor.clone()),
                    SAY,
                    json!({"from": ctx.actor, "room": room.0, "to": name, "text": text}),
                )
                .map_err(internal)?;
                for other in others_here(w, room, &ctx.actor) {
                    narrate(w, &other, format!("{} says to {name}, \"{text}\"", ctx.actor));
                }
                // A word carried to it, if one was wanted, before anything it would say.
                let answer = match super::errand::carried_word(w, &ctx.actor, creature, &text)? {
                    Some(a) => Some(a),
                    None => reply(w, creature, &ctx.actor, &text),
                };
                let Some(answer) = answer else {
                    return Ok(Outcome::new(
                        format!("You say to the {name}, \"{text}\" It does not understand you. It is only a {name}."),
                        json!({"to": name, "text": text, "answered": false}),
                    ));
                };
                speak(w, creature, &ctx.actor, &answer)?;
                return Ok(Outcome::new(
                    format!("You say to {name}, \"{text}\" {name} says, \"{answer}\""),
                    json!({"to": name, "text": text, "answered": true, "reply": answer}),
                ));
            }
            // A citizen, here or away.
            let typed = who.trim_start_matches("citizen-").to_string();
            let Some(target) = super::canonical_name(w, &typed) else {
                return Err(CommandError::precondition(format!(
                    "There is no one called {typed}, and no such creature here."
                )));
            };
            let Some(them) = super::find_citizen(w, &target) else {
                return Err(CommandError::precondition(format!("There is no one called {typed}.")));
            };
            if target == ctx.actor {
                return Err(CommandError::precondition("You tell yourself. Nothing changes."));
            }
            let their_room = rooms::room_of(w, them.citizen);
            let present = their_room == Some(room) && presence::is_here(w, them.citizen);
            if present {
                w.note(
                    Actor::Citizen(ctx.actor.clone()),
                    SAY,
                    json!({"from": ctx.actor, "room": room.0, "to": target, "text": text}),
                )
                .map_err(internal)?;
                narrate(w, &target, format!("{} says to you, \"{text}\"", ctx.actor));
                for other in others_here(w, room, &ctx.actor).into_iter().filter(|o| o != &target) {
                    narrate(w, &other, format!("{} says to {target}, \"{text}\"", ctx.actor));
                }
                return Ok(Outcome::new(
                    format!("You say to {target}, \"{text}\" They hear you."),
                    json!({"to": target, "text": text, "delivered": true}),
                ));
            }
            w.note(
                Actor::Citizen(ctx.actor.clone()),
                TELL,
                json!({"from": ctx.actor, "to": target, "room": room.0, "text": text}),
            )
            .map_err(internal)?;
            Ok(Outcome::new(
                format!("{target} is not here. They will hear it when they return: \"{text}\""),
                json!({"to": target, "text": text, "delivered": false}),
            ))
        }),
    );
    world.on_arrival(Box::new(|w: &mut World, actor: &str| deliver(w, actor)));
}

fn others_here(world: &World, room: EntityId, me: &str) -> Vec<String> {
    world
        .store
        .children(room)
        .filter(|e| e.kind == "citizen" && presence::is_here(world, e.id))
        .filter_map(|e| e.owner.clone())
        .filter(|o| o != me)
        .collect()
}

/// A creature speaks: recorded as its own `say`, heard by everyone here.
/// A named creature answers `to`: the room hears who it was answering, so a visitor
/// does not read another's answer as their own (one did, and reported the world
/// inconsistent).
pub fn speak(
    world: &mut World,
    creature: EntityId,
    to: &str,
    text: &str,
) -> Result<(), CommandError> {
    let name = mind::display(world, creature);
    let Some(room) = world.store.entity(creature).and_then(|e| e.parent) else {
        return Ok(());
    };
    world
        .note(
            Actor::Driver,
            SAY,
            json!({"from": name, "creature": creature.0, "room": room.0, "to": to, "text": text}),
        )
        .map_err(internal)?;
    let present: Vec<String> = world
        .store
        .children(room)
        .filter(|e| e.kind == "citizen" && presence::is_here(world, e.id))
        .filter_map(|e| e.owner.clone())
        .collect();
    for actor in present {
        let line = if actor == to {
            format!("{name} says to you, \"{text}\"")
        } else {
            format!("{name} says to {to}, \"{text}\"")
        };
        narrate(world, &actor, line);
    }
    Ok(())
}

/// Words waiting for a citizen who just arrived.
pub fn deliver(world: &mut World, actor: &str) -> Result<(), String> {
    let Some(f) = super::find_citizen(world, actor) else {
        return Ok(());
    };
    let heard: u64 = match world
        .store
        .entity(f.citizen)
        .and_then(|e| e.components.get(presence::PRESENCE))
        .and_then(|c| c.fields.get("heard_through"))
    {
        Some(Field::Plain(v)) => v.as_u64().unwrap_or(0),
        _ => 0,
    };
    let head = world
        .log
        .head(&world.id)
        .map_err(|e| e.to_string())?
        .map(|s| s.0)
        .unwrap_or(0);
    if head <= heard {
        return Ok(());
    }
    let waiting: Vec<(String, String)> = world
        .log
        .read_all(&world.id, Cursor { after: heard })
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|e| e.kind == TELL && e.payload["to"] == actor)
        .map(|e| {
            (
                e.payload["from"].as_str().unwrap_or("?").to_string(),
                e.payload["text"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    for (from, text) in &waiting {
        narrate(
            world,
            actor,
            format!("While you were away, {from} left word: \"{text}\""),
        );
    }
    let mut fields = world
        .store
        .entity(f.citizen)
        .and_then(|e| e.components.get(presence::PRESENCE))
        .map(|c| c.fields.clone())
        .unwrap_or_default();
    if !fields.contains_key("here") {
        fields.insert("here".into(), Field::Plain(json!(true)));
    }
    fields.insert("heard_through".into(), Field::Plain(json!(head)));
    world
        .record(
            Actor::Driver,
            moondial_store::StoreEvent::ComponentSet {
                id: f.citizen,
                component: presence::PRESENCE.into(),
                fields,
            },
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// `say` reached a named creature by name: it answers.
pub fn overhear(
    world: &mut World,
    room: EntityId,
    actor: &str,
    text: &str,
) -> Result<Vec<String>, CommandError> {
    let lower = text.to_lowercase();
    let named: Vec<EntityId> = world
        .store
        .children(room)
        .filter(|e| e.kind == "creature")
        .map(|e| e.id)
        .filter(|&c| mind::name(world, c).is_some_and(|n| lower.contains(&n.to_lowercase())))
        .collect();
    let mut answers = Vec::new();
    for c in named {
        if let Some(a) = reply(world, c, actor, text) {
            speak(world, c, actor, &a)?;
            answers.push(format!("{} says, \"{a}\"", mind::display(world, c)));
        }
    }
    Ok(answers)
}

/// A named creature notices who walks in.
pub fn greet(world: &mut World, actor: &str, room: EntityId) {
    // Citizens here with a standing greeting say it to whoever arrives.
    let greeters: Vec<(String, String)> = world
        .store
        .children(room)
        .filter(|e| e.kind == "citizen")
        .filter(|e| e.owner.as_deref() != Some(actor))
        .filter(|e| presence::is_here(world, e.id))
        .filter_map(|e| {
            let name = plain_str(world, e.id, super::CITIZEN, "name")?.to_string();
            let text = plain_str(world, e.id, super::CITIZEN, "greeting")?.to_string();
            Some((name, text))
        })
        .collect();
    for (name, text) in greeters {
        // Once an hour per visitor: a greeting that replays on every step through the
        // room is noise, as a visitor who heard it eight times said.
        if !world.greeted(&name, actor) {
            continue;
        }
        narrate(
            world,
            actor,
            format!("{name}, who is here, says to you as you arrive: \"{text}\" (tell {name} answers them.)"),
        );
    }
    let named: Vec<EntityId> = world
        .store
        .children(room)
        .filter(|e| e.kind == "creature")
        .map(|e| e.id)
        .filter(|&c| mind::name(world, c).is_some())
        .collect();
    for c in named {
        let name = mind::display(world, c);
        let line = if mind::keepers(world, c).iter().any(|k| k == actor) {
            format!("{name} looks up. \"You again, {actor}.\"")
        } else {
            format!("{name} looks up at you, and does not look away.")
        };
        narrate(world, actor, line);
    }
}

/// What a named creature says back: facts, from the log and the store, in its voice.
/// `None` when it has no name to answer with.
pub fn reply(world: &World, creature: EntityId, actor: &str, text: &str) -> Option<String> {
    let name = mind::name(world, creature)?;
    let kind = plain_str(world, creature, super::CREATURE, "name")
        .unwrap_or("?")
        .to_string();
    let t = text.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| t.contains(w));
    let keepers = mind::keepers(world, creature);
    // A creature knows whoever kept it, and whoever told it a story: a visitor who
    // told one and said thanks was asked "For what?", which is not how a story is received.
    let told_me =
        mind::told_by(world, creature, actor) || super::errand::did_for(world, creature, actor);
    let knows_you = keepers.iter().any(|k| k == actor) || told_me;
    let level = mind::level(world, creature);
    let room = world.store.entity(creature).and_then(|e| e.parent);
    let sources = held(world, creature);
    let list = |v: &[String]| -> String {
        match v.len() {
            0 => "nothing".into(),
            1 => v[0].clone(),
            n => format!("{}, and {}", v[..n - 1].join(", "), v[n - 1]),
        }
    };

    // A child knows whose it is, and what is strange about it.
    let parents: Option<(String, String)> = world
        .store
        .entity(creature)
        .and_then(|e| e.components.get(super::CREATURE))
        .and_then(|c| c.fields.get("parents"))
        .and_then(|f| match f {
            moondial_store::Field::Plain(v) => v.as_array().cloned(),
            _ => None,
        })
        .filter(|p| p.len() == 2)
        .map(|p| {
            (
                p[0].as_str().unwrap_or("?").to_string(),
                p[1].as_str().unwrap_or("?").to_string(),
            )
        });
    if parents.is_some()
        && has(&[
            "who made you",
            "your parents",
            "born",
            "whose are you",
            "your mother",
            "your father",
        ])
    {
        let (a, b) = parents.clone().unwrap_or_default();
        let carrier = plain_str(world, creature, super::CREATURE, "born_of")
            .unwrap_or("someone")
            .to_string();
        let strange = plain_str(world, creature, super::CREATURE, "mutation")
            .map(|m| format!(" I am {m}; neither of them was."))
            .unwrap_or_default();
        return Some(format!(
            "I am {name}, a {kind}, of {a} and {b}. {carrier} carried the word between them, and I came of it.{strange}"
        ));
    }
    if has(&["who are you", "your name", "what are you", "who is this"]) {
        if let Some((a, b)) = parents {
            return Some(format!(
                "I am {name}. I was born a {kind}, of {a} and {b}, and I have been a {kind} with a name since {} fed me to it.",
                keepers.last().cloned().unwrap_or_else(|| "someone".into())
            ));
        }
        let first = keepers.first().cloned().unwrap_or_else(|| "someone".into());
        let with = first_gift(world, creature).unwrap_or_else(|| "a memory".into());
        return Some(format!(
            "I am {name}. I was a {kind}, and nothing else, until {first} gave me {with}. Now I am a {kind} with a name."
        ));
    }
    if has(&[
        "what else",
        "the rest",
        "go on",
        "and the others",
        "more of",
    ]) {
        // The rest of what it holds, four at a time: the answer to "and 18 more".
        let asked = world
            .recent_events(400)
            .iter()
            .filter(|e| {
                e.kind == SAY
                    && e.payload["from"] == actor
                    && e.payload["to"]
                        .as_str()
                        .is_some_and(|t| t.starts_with(&name))
                    && e.payload["text"].as_str().is_some_and(|t| {
                        let t = t.to_lowercase();
                        [
                            "what else",
                            "the rest",
                            "go on",
                            "and the others",
                            "more of",
                        ]
                        .iter()
                        .any(|w| t.contains(w))
                    })
            })
            .count();
        // The ask being answered is already in the log, so the count includes it: the
        // first "what else" is page two.
        let asked = asked.max(1);
        let page: Vec<String> = sources.iter().skip(4 * asked).take(4).cloned().collect();
        return Some(if page.is_empty() {
            "That is all of it.".into()
        } else {
            let left = sources.len().saturating_sub(4 * (asked + 1));
            format!(
                "Also {}{}.",
                list(&page),
                if left > 0 {
                    format!(", and {left} more")
                } else {
                    String::new()
                }
            )
        });
    }
    if has(&[
        "remember",
        "hold",
        "keep",
        "memor",
        "what do you have",
        "carry",
    ]) {
        let taken = last_taken(world, creature);
        // The newest few, not the whole window: a wisp with thirty stories answered
        // "who keeps you?" with a page.
        let shown: Vec<String> = sources.iter().take(4).cloned().collect();
        let mut s = format!("I hold {level:.0}: {}", list(&shown));
        if sources.len() > shown.len() {
            s.push_str(&format!(
                ", and {} more (ask what else)",
                sources.len() - shown.len()
            ));
        }
        s.push('.');
        s.push_str(&format!(" {} keep me.", list(&keepers)));
        if let Some((by, with)) = taken {
            s.push_str(&format!(
                " {by} took my {with}. I remember that it is gone, which is not the same."
            ));
        }
        return Some(s);
    }
    if has(&["where", "this place", "what room", "which room"]) {
        let (rname, desc, exits) = room
            .map(|r| {
                let d = plain_str(world, r, ROOM, "description").unwrap_or("");
                let first = d
                    .split_inclusive(['.', '!', '?'])
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                (
                    plain_str(world, r, ROOM, "name")
                        .unwrap_or("here")
                        .to_string(),
                    first,
                    rooms::exits_of(world, r)
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>(),
                )
            })
            .unwrap_or_default();
        return Some(format!(
            "{rname}. {desc} The ways out are {}.",
            list(&exits)
        ));
    }
    if has(&[
        "who has",
        "who was",
        "seen",
        "anyone",
        "been here",
        "who came",
        "what happened",
        "news",
    ]) {
        let seen = recent(world, creature, room);
        return Some(if seen.is_empty() {
            "No one, since I started counting. I started counting when I got my name.".into()
        } else {
            format!("Since I had a name: {}.", seen.join("; "))
        });
    }
    if has(&["fade", "forget", "die", "afraid", "last"]) {
        let bar = mind::TIERS
            .iter()
            .find(|(t, _, _)| *t == mind::tier(world, creature))
            .map(|(_, b, _)| b / 2.0)
            .unwrap_or(30.0);
        return Some(format!(
            "Everything fades. I hold {level:.0}; below {bar:.0} I will not know my name, and you will say \"the {kind}\" again and be right."
        ));
    }
    if has(&["hello", "hi ", "hi,", "hey", "greet", "good "])
        || t.trim_end_matches(['.', '!']) == "hi"
    {
        return Some(if knows_you {
            format!("{actor}. You again. I hold {level:.0} today.")
        } else if keepers.is_empty() {
            "I do not know you. I do not know anyone yet.".into()
        } else {
            format!("I do not know you. I know {}.", list(&keepers))
        });
    }
    if has(&["thank"]) {
        return Some(if told_me {
            "For the story? I hold it. That is the thanks.".into()
        } else if knows_you {
            "It is your memory. I only hold it.".into()
        } else {
            "For nothing yet. Tell me a story and there will be something of yours in me.".into()
        });
    }
    if has(&[
        "bye", "farewell", "see you", "so long", "must go", "leaving",
    ]) {
        return Some(if knows_you {
            format!("Go, then, {actor}. What you gave me holds as long as it holds.")
        } else {
            "Go, then. I will not remember you, which is nothing against you.".into()
        });
    }
    if has(&["sorry", "apolog"]) {
        return Some("Nothing is taken from me that fading would not take.".into());
    }
    Some(if knows_you {
        format!("I hold what I am given, {actor}. Ask me what I remember, where this is, or who has been here.")
    } else {
        "I hold what I am given. Ask me what I remember, where this is, or who has been here."
            .into()
    })
}

/// The sources of what a creature holds, most recent first.
fn held(world: &World, creature: EntityId) -> Vec<String> {
    let Some(window) = mind::window_of(world, creature) else {
        return Vec::new();
    };
    let mut v: Vec<(u64, String)> = world
        .store
        .children(window)
        .filter(|e| e.kind == "chunk")
        .filter_map(|e| {
            plain_str(world, e.id, CHUNK, "source").map(|s| (e.id.0, format!("the memory of {s}")))
        })
        .collect();
    v.sort_by_key(|(id, _)| std::cmp::Reverse(*id));
    v.into_iter().map(|(_, s)| s).collect()
}

fn first_gift(world: &World, creature: EntityId) -> Option<String> {
    match world
        .store
        .entity(creature)?
        .components
        .get(mind::MIND)?
        .fields
        .get("kept")?
    {
        Field::Plain(Value::Array(a)) => a
            .first()
            .and_then(|k| k["with"].as_str())
            .map(|s| format!("the memory of {s}")),
        _ => None,
    }
}

fn last_taken(world: &World, creature: EntityId) -> Option<(String, String)> {
    world
        .recent_events(2000)
        .into_iter()
        .rev()
        .find(|e| e.kind == mind::TAKEN && e.payload["creature"] == creature.0)
        .map(|e| {
            (
                e.payload["by"].as_str().unwrap_or("someone").to_string(),
                e.payload["with"].as_str().unwrap_or("memory").to_string(),
            )
        })
}

/// What the creature has seen in its room since it was named: arrivals, speech, digs.
fn recent(world: &World, creature: EntityId, room: Option<EntityId>) -> Vec<String> {
    let Some(room) = room else { return Vec::new() };
    let all = world.recent_events(2000);
    let named_at = all
        .iter()
        .rev()
        .find(|e| {
            e.kind == mind::TIER
                && e.payload["creature"] == creature.0
                && e.payload["to"].as_u64() >= Some(1)
        })
        .map(|e| e.seq.0)
        .unwrap_or(0);
    let citizens_here = |e: &moondial_log::Event| -> Option<String> {
        if e.kind != "store.entity_moved" || e.payload["parent"] != room.0 {
            return None;
        }
        let id = EntityId(e.payload["id"].as_u64()?);
        plain_str(world, id, CITIZEN, "name").map(str::to_string)
    };
    let mut out: Vec<String> = Vec::new();
    let mut seen_names: BTreeMap<String, ()> = BTreeMap::new();
    for e in all.iter().filter(|e| e.seq.0 > named_at).rev() {
        if out.len() >= 4 {
            break;
        }
        if let Some(n) = citizens_here(e) {
            if seen_names.insert(n.clone(), ()).is_none() {
                out.push(format!("{n} came"));
            }
        } else if e.kind == SAY && e.payload["room"] == room.0 && e.payload["creature"].is_null() {
            let from = e.payload["from"].as_str().unwrap_or("someone");
            let text: String = e.payload["text"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(60)
                .collect();
            out.push(format!("{from} said, \"{text}\""));
        } else if e.kind == super::building::REMEMBERED && e.payload["creature"] == creature.0 {
            out.push(format!(
                "{} gave me the memory of {}",
                e.payload["by"].as_str().unwrap_or("someone"),
                e.payload["with"].as_str().unwrap_or("something")
            ));
        }
    }
    out.reverse();
    out
}
