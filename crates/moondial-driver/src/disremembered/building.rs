//! Building: players extend the world. `dig` opens a new room off the one you stand
//! in, paid for with a chunk: the room is born with that memory's half-life, and its
//! own `memory` fades on the same curve until someone visits. A faded room stays on
//! the map with its name and exits; only its description dims, in steps, toward a
//! half-remembered line. `describe` and `name` are the owner's, or a designee's
//! (`permit`, `revoke`, with an optional expiry). The seed rooms belong to the world:
//! an operator describes them, and a citizen may dig from each of them once.
//!
//! Every verb here is a registry command, so the vocabulary carries its signature and
//! the Gate compiles it like any other; a name goes through the checker's reserved-word
//! test, so no one digs "the gate".

use super::rooms::{exits_of, room_of};
use super::{internal, me, plain_str, CHUNK, EXITS, ROOM};
use crate::world::World;
use moondial_log::Actor;
use moondial_registry::{
    ActorKind, Args, CallContext, CommandError, CommandSpec, Outcome, ParamType,
};
use moondial_store::{EntityId, Field, StoreEvent};
use moondial_time::{Curve, Duration, TimedValue};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Most characters a description may carry, appended or not.
const MAX_DESCRIPTION_CHARS: usize = 800;
/// Most characters a room name may carry.
const MAX_NAME_CHARS: usize = 40;
/// Below this memory the description is only its first sentence; below `FADED` it is gone.
pub(crate) const DIM: f64 = 0.5;
pub(crate) const FADED: f64 = 0.15;
/// What a room says of itself when its memory has faded.
pub const HALF_REMEMBERED: &str =
    "A half-remembered place. Someone described it once; the words have faded.";
const UNDESCRIBED: &str = "Freshly dug. No one has described it yet.";

/// Whether a room was dug by a citizen (and so has a memory), as opposed to seeded.
pub fn is_built(world: &World, room: EntityId) -> bool {
    world
        .store
        .entity(room)
        .and_then(|e| e.components.get(ROOM))
        .is_some_and(|c| c.fields.contains_key("memory"))
}

/// A built room's memory now, 0 to 1; `None` for a seed room, which never fades.
pub fn memory(world: &World, room: EntityId) -> Option<f64> {
    if !is_built(world, room) {
        return None;
    }
    world
        .store
        .read_field(room, ROOM, "memory", world.now())
        .ok()
        .and_then(|v| v.as_f64())
}

/// The half-life a built room's memory fades on.
fn half_life(world: &World, room: EntityId) -> Option<Duration> {
    match world
        .store
        .entity(room)?
        .components
        .get(ROOM)?
        .fields
        .get("half_life_ms")?
    {
        Field::Plain(v) => v.as_i64().map(Duration),
        _ => None,
    }
}

/// A room's fields with these changed: a component is set whole, so a change to one
/// field carries the others along.
fn merged(
    world: &World,
    room: EntityId,
    changes: BTreeMap<String, Field>,
) -> BTreeMap<String, Field> {
    let mut fields = world
        .store
        .entity(room)
        .and_then(|e| e.components.get(ROOM))
        .map(|c| c.fields.clone())
        .unwrap_or_default();
    fields.extend(changes);
    fields
}

/// Someone is here: a built room remembers itself again, in full.
pub fn visited(world: &mut World, room: EntityId) -> Result<(), String> {
    let Some(hl) = half_life(world, room) else {
        return Ok(());
    };
    if memory(world, room).is_some_and(|m| m > 0.999) {
        return Ok(());
    }
    let mut fields = BTreeMap::new();
    fields.insert(
        "memory".into(),
        Field::Timed(TimedValue::new(
            1.0,
            world.now(),
            Curve::Exponential { half_life: hl },
        )),
    );
    let fields = merged(world, room, fields);
    world
        .record(
            Actor::Driver,
            StoreEvent::ComponentSet {
                id: room,
                component: ROOM.into(),
                fields,
            },
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The description as a visitor finds it: whole while the room is remembered, its first
/// sentence as the memory dims, a stock line once it has faded.
pub fn remembered_description(description: &str, memory: Option<f64>) -> String {
    match memory {
        None => description.to_string(),
        Some(m) if m >= DIM => description.to_string(),
        Some(m) if m >= FADED => {
            let first = description
                .split_inclusive(['.', '!', '?'])
                .next()
                .unwrap_or(description)
                .trim();
            format!("{first} The rest is hard to recall.")
        }
        Some(_) => HALF_REMEMBERED.to_string(),
    }
}

/// The way back for a compass exit, or `back`.
fn opposite(exit: &str) -> &'static str {
    match exit {
        "north" => "south",
        "south" => "north",
        "east" => "west",
        "west" => "east",
        "up" => "down",
        "down" => "up",
        "in" => "out",
        "out" => "in",
        _ => "back",
    }
}

/// A room key from a name and an id: `salt-stair-12`.
fn key_for(name: &str, id: EntityId) -> String {
    let slug: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty() && *s != "the" && *s != "a" && *s != "an")
        .take(3)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        format!("dug-{}", id.0)
    } else {
        format!("{slug}-{}", id.0)
    }
}

/// A name a room may carry: short, printable, and not a word the protocol reserves.
fn check_name(name: &str) -> Result<String, CommandError> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let n = name.chars().count();
    if !(2..=MAX_NAME_CHARS).contains(&n) {
        return Err(CommandError::precondition(format!(
            "A name is 2 to {MAX_NAME_CHARS} characters."
        )));
    }
    if name.contains(['[', ']', '"', '“', '”']) {
        return Err(CommandError::precondition(
            "A name carries no brackets or quotation marks.",
        ));
    }
    if let Some(word) = crate::gate::world_checker().collision(&name) {
        return Err(CommandError::precondition(format!(
            "'{word}' is a word CLEAR reserves; a name may not carry it."
        )));
    }
    Ok(name)
}

/// Whether `actor` may change a room: its owner, a current designee, or an operator.
fn may_edit(world: &World, ctx: &CallContext, room: EntityId) -> Result<(), CommandError> {
    if ctx.kind == ActorKind::Admin {
        return Ok(());
    }
    let e = world
        .store
        .entity(room)
        .ok_or_else(|| internal("no such room"))?;
    if !is_built(world, room) {
        return Err(CommandError::precondition(
            "This room is the world's; only an operator changes it. Dig your own.",
        ));
    }
    if e.owner.as_deref() == Some(ctx.actor.as_str()) {
        return Ok(());
    }
    if granted(world, room, "permits", &ctx.actor) {
        Ok(())
    } else {
        Err(CommandError::precondition(format!(
            "{} does not have this room's owner's permission.",
            ctx.actor
        )))
    }
}

/// Whether `actor` may add to a room: anyone who may change it, or a welcomed guest.
fn may_append(world: &World, ctx: &CallContext, room: EntityId) -> Result<(), CommandError> {
    match may_edit(world, ctx, room) {
        Ok(()) => Ok(()),
        Err(e) => {
            if is_built(world, room) && granted(world, room, "welcomes", &ctx.actor) {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

/// Whether `who` holds a live grant in the room's `field` map (`permits` or `welcomes`):
/// an entry whose expiry is null or still ahead.
fn granted(world: &World, room: EntityId, field: &str, who: &str) -> bool {
    let now = world.now().0;
    grants_of(world, room, field)
        .get(who)
        .is_some_and(|until| until.is_null() || until.as_i64().is_some_and(|t| t > now))
}

fn owner_only(world: &World, ctx: &CallContext, room: EntityId) -> Result<(), CommandError> {
    if ctx.kind == ActorKind::Admin {
        return Ok(());
    }
    let owner = world.store.entity(room).and_then(|e| e.owner.clone());
    if owner.as_deref() == Some(ctx.actor.as_str()) {
        Ok(())
    } else if is_built(world, room) {
        Err(CommandError::precondition(
            "Only the room's owner gives or takes permission.",
        ))
    } else {
        Err(CommandError::precondition(
            "This room is the world's; it has no owner to permit anyone.",
        ))
    }
}

fn set_room_fields(
    world: &mut World,
    actor: &str,
    room: EntityId,
    fields: BTreeMap<String, Field>,
) -> Result<(), CommandError> {
    let fields = merged(world, room, fields);
    world
        .record(
            Actor::Citizen(actor.to_string()),
            StoreEvent::ComponentSet {
                id: room,
                component: ROOM.into(),
                fields,
            },
        )
        .map(|_| ())
        .map_err(internal)
}

fn grants_of(world: &World, room: EntityId, field: &str) -> serde_json::Map<String, Value> {
    match world
        .store
        .entity(room)
        .and_then(|e| e.components.get(ROOM))
        .and_then(|c| c.fields.get(field))
    {
        Some(Field::Plain(Value::Object(m))) => m.clone(),
        _ => serde_json::Map::new(),
    }
}

/// The public record of a memory given up for a room:
/// `{room, name, owner, by, with, spent, added_ms, half_life_ms}`.
pub const REMEMBERED: &str = "disremembered.remembered";

/// How many rooms `actor` has dug off `from`.
fn dug_from(world: &World, actor: &str, from: EntityId) -> usize {
    world
        .store
        .entities()
        .filter(|e| e.kind == "room" && e.owner.as_deref() == Some(actor))
        .filter(|e| {
            matches!(
                e.components.get(ROOM).and_then(|c| c.fields.get("origin")),
                Some(Field::Plain(v)) if v.as_u64() == Some(from.0)
            )
        })
        .count()
}

/// The half-life a chunk's value fades on, which the room dug with it inherits.
fn chunk_half_life(world: &World, chunk: EntityId) -> Duration {
    match world
        .store
        .entity(chunk)
        .and_then(|e| e.components.get(CHUNK))
        .and_then(|c| c.fields.get("value"))
    {
        Some(Field::Timed(t)) => match t.curve {
            Curve::Exponential { half_life } => half_life,
            Curve::PowerLaw { scale, .. } => scale,
            _ => Duration::seconds(3600),
        },
        _ => Duration::seconds(3600),
    }
}

fn humane(d: Duration) -> String {
    let s = d.0 / 1000;
    if s >= 172_800 {
        format!("{} days", s / 86_400)
    } else if s >= 7200 {
        format!("{} hours", s / 3600)
    } else if s >= 120 {
        format!("{} minutes", s / 60)
    } else {
        format!("{s} seconds")
    }
}

pub fn register(world: &mut World) {
    world.register(
        CommandSpec::new(
            "disremembered.dig",
            "Kept names only. Open a new room off this one, through a new exit, paid for with one of your chunks: the room is born with that memory's half-life and fades unless visited. From each of the world's own rooms (the seed rooms) once; from your own, as you like.",
        )
        .param("exit", ParamType::String, true, "the new exit's name, not one this room has")
        .param("chunk", ParamType::Entity, true, "a chunk of yours; it is spent")
        .param("name", ParamType::Text, false, "the new room's name, quoted; it can be named later"),
        Box::new(|w: &mut World, ctx, args: &Args| {
            super::kept_only(w, ctx, "dig")?;
            let f = me(w, ctx)?;
            let here = room_of(w, f.citizen).ok_or_else(|| internal("citizen has no room"))?;
            let exit = args.str("exit").unwrap_or("").trim().to_ascii_lowercase();
            if exit.is_empty()
                || exit.chars().count() > 16
                || !exit.chars().all(|c| c.is_ascii_lowercase() || c == '-')
            {
                return Err(CommandError::precondition(
                    "An exit is one lowercase word, up to 16 letters (hyphens allowed).",
                ));
            }
            if let Some(word) = crate::gate::world_checker().collision(&exit) {
                return Err(CommandError::precondition(format!(
                    "'{word}' is a word CLEAR reserves; an exit may not be it."
                )));
            }
            if exits_of(w, here).contains_key(&exit) {
                return Err(CommandError::precondition(format!(
                    "This room already has an exit '{exit}'."
                )));
            }
            let seed = !is_built(w, here);
            let owner = w.store.entity(here).and_then(|e| e.owner.clone());
            if ctx.kind != ActorKind::Admin {
                if seed {
                    if dug_from(w, &ctx.actor, here) >= 1 {
                        return Err(CommandError::precondition(
                            "You have already dug from this room; the seed rooms take one exit each from a citizen. Dig on from your own.",
                        ));
                    }
                } else if owner.as_deref() != Some(ctx.actor.as_str()) {
                    return Err(CommandError::precondition(
                        "Only a room's owner digs from it. Dig from the world's rooms, or your own.",
                    ));
                }
            }
            let chunk = super::memory::resolve_chunk(
                w,
                ctx,
                args.str("chunk").unwrap_or(""),
                &[f.window, f.palace],
            )?;
            let name = match args.str("name").map(str::trim).filter(|s| !s.is_empty()) {
                Some(n) => check_name(n)?,
                None => "An Unnamed Digging".to_string(),
            };
            let hl = chunk_half_life(w, chunk);
            let source = plain_str(w, chunk, CHUNK, "source")
                .unwrap_or("something")
                .to_string();
            let me = Actor::Citizen(ctx.actor.clone());
            // The chunk is spent.
            w.record(me.clone(), StoreEvent::EntityRemoved { id: chunk })
                .map_err(internal)?;
            // The room is born remembering itself.
            let id = w.store.allocate_id();
            let now = w.now();
            w.record(
                me.clone(),
                StoreEvent::EntityCreated {
                    id,
                    kind: "room".into(),
                    owner: Some(ctx.actor.clone()),
                    parent: None,
                    scopes: vec![],
                },
            )
            .map_err(internal)?;
            let key = key_for(&name, id);
            let mut fields = BTreeMap::new();
            fields.insert("key".into(), Field::Plain(json!(key)));
            fields.insert("name".into(), Field::Plain(json!(name)));
            fields.insert("description".into(), Field::Plain(json!(UNDESCRIBED)));
            fields.insert(
                "memory".into(),
                Field::Timed(TimedValue::new(1.0, now, Curve::Exponential { half_life: hl })),
            );
            fields.insert("half_life_ms".into(), Field::Plain(json!(hl.0)));
            fields.insert("permits".into(), Field::Plain(json!({})));
            fields.insert("welcomes".into(), Field::Plain(json!({})));
            fields.insert("origin".into(), Field::Plain(json!(here.0)));
            fields.insert("dug_with".into(), Field::Plain(json!(source)));
            set_room_fields(w, &ctx.actor, id, fields)?;
            let mut back = serde_json::Map::new();
            back.insert(opposite(&exit).to_string(), json!(here.0));
            let mut ef = BTreeMap::new();
            ef.insert("exits".into(), Field::Plain(Value::Object(back)));
            w.record(
                me.clone(),
                StoreEvent::ComponentSet {
                    id,
                    component: EXITS.into(),
                    fields: ef,
                },
            )
            .map_err(internal)?;
            w.record(
                me.clone(),
                StoreEvent::ComponentSet {
                    id,
                    component: super::SPRITE.into(),
                    fields: super::sprite_fields("#", "#7a6f5f", dug_frame(&exit, &name)),
                },
            )
            .map_err(internal)?;
            // And this room gains the way there.
            let mut exits: serde_json::Map<String, Value> = exits_of(w, here)
                .into_iter()
                .map(|(k, v)| (k, json!(v.0)))
                .collect();
            exits.insert(exit.clone(), json!(id.0));
            let mut ef = BTreeMap::new();
            ef.insert("exits".into(), Field::Plain(Value::Object(exits)));
            w.record(
                me,
                StoreEvent::ComponentSet {
                    id: here,
                    component: EXITS.into(),
                    fields: ef,
                },
            )
            .map_err(internal)?;
            Ok(Outcome::new(
                format!(
                    "You dig {exit} with the memory of {source}. {name} opens: undescribed, yours, remembered for now. Its memory halves every {} unless someone visits; a visit remembers it whole, and `remember <chunk>` inside it lengthens that half-life for good. The way back is {}.",
                    humane(hl),
                    opposite(&exit)
                ),
                json!({"room": id.0, "key": key, "name": name, "exit": exit, "back": opposite(&exit), "half_life_ms": hl.0, "spent": chunk.0}),
            ))
        }),
    );

    world.register(
        CommandSpec::new(
            "disremembered.remember",
            "Give up one of your chunks so something lasts. Alone (kept names only), the room you stand in: its memory's half-life grows by the chunk's (any dug room, yours or another's). With a creature here, the creature keeps the chunk: its level is what it holds, and at 60 it has a name and knows who keeps it. Either way your name is kept with it.",
        )
        .param("chunk", ParamType::Entity, true, "a chunk of yours; it is spent")
        .param("creature", ParamType::Entity, false, "a creature here, by kind or by name"),
        Box::new(|w: &mut World, ctx, args: &Args| {
            let f = me(w, ctx)?;
            let here = room_of(w, f.citizen).ok_or_else(|| internal("citizen has no room"))?;
            // A passing name may feed a creature (the creature keeps it) but not a room
            // (the room would keep the name).
            if args.str("creature").filter(|s| !s.trim().is_empty()).is_none() {
                super::kept_only(w, ctx, "remember a chunk into a room")?;
            }
            if let Some(what) = args.str("creature").filter(|s| !s.trim().is_empty()) {
                let Some(creature) = super::mind::find_here(w, here, what) else {
                    return Err(CommandError::precondition(format!("There is no {what} here.")));
                };
                let chunk = super::memory::resolve_chunk(
                    w,
                    ctx,
                    args.str("chunk").unwrap_or(""),
                    &[f.window, f.palace],
                )?;
                let before = super::mind::display(w, creature);
                let (source, level) = super::mind::give(w, ctx.actor.as_str(), chunk, creature)?;
                let after = super::mind::display(w, creature);
                let next = super::mind::TIERS
                    .iter()
                    .find(|(_, bar, _)| level < *bar)
                    .map(|(_, bar, what)| format!(" At {bar:.0} it would be {what}."))
                    .unwrap_or_default();
                w.notice(format!(
                    "**{}** gave the memory of {source} to **{after}**, which holds {level:.0}.",
                    ctx.actor
                ));
                return Ok(Outcome::new(
                    if before == after {
                        format!("You give up the memory of {source}, and the {after} keeps it. It holds {level:.0} now.{next}")
                    } else {
                        format!("You give up the memory of {source}, and the {before} keeps it. It holds {level:.0} now, and it is {after}.")
                    },
                    json!({"creature": creature.0, "name": super::mind::name(w, creature), "spent": chunk.0, "with": source, "level": level, "tier": super::mind::tier(w, creature)}),
                ));
            }
            let Some(old) = half_life(w, here) else {
                return Err(CommandError::precondition(
                    "This room is the world's; it remembers itself. Remember a room someone dug.",
                ));
            };
            let chunk = super::memory::resolve_chunk(
                w,
                ctx,
                args.str("chunk").unwrap_or(""),
                &[f.window, f.palace],
            )?;
            let added = chunk_half_life(w, chunk);
            let source = plain_str(w, chunk, CHUNK, "source")
                .unwrap_or("something")
                .to_string();
            let me = Actor::Citizen(ctx.actor.clone());
            w.record(me.clone(), StoreEvent::EntityRemoved { id: chunk })
                .map_err(internal)?;
            let hl = Duration(old.0.saturating_add(added.0));
            let now = w.now();
            let mut kept = match w
                .store
                .entity(here)
                .and_then(|e| e.components.get(ROOM))
                .and_then(|c| c.fields.get("kept"))
            {
                Some(Field::Plain(Value::Array(a))) => a.clone(),
                _ => Vec::new(),
            };
            kept.push(json!({"by": ctx.actor, "with": source, "half_life_ms": added.0}));
            let mut fields = BTreeMap::new();
            fields.insert(
                "memory".into(),
                Field::Timed(TimedValue::new(1.0, now, Curve::Exponential { half_life: hl })),
            );
            fields.insert("half_life_ms".into(), Field::Plain(json!(hl.0)));
            fields.insert("kept".into(), Field::Plain(Value::Array(kept)));
            set_room_fields(w, &ctx.actor, here, fields)?;
            let name = plain_str(w, here, ROOM, "name")
                .unwrap_or("The room")
                .to_string();
            let owner = w
                .store
                .entity(here)
                .and_then(|e| e.owner.clone())
                .unwrap_or_default();
            w.notice(format!(
                "**{}** gave up the memory of {source} to keep **{name}** ({owner}'s).",
                ctx.actor
            ));
            let _ = w.note(
                me,
                REMEMBERED,
                json!({"room": here.0, "name": name, "owner": owner, "by": ctx.actor, "with": source, "spent": chunk.0, "added_ms": added.0, "half_life_ms": hl.0}),
            );
            Ok(Outcome::new(
                format!(
                    "You give up the memory of {source}, and {name} keeps it. Its memory now halves every {} instead of {}; your name is kept with the room.",
                    humane(hl),
                    humane(old)
                ),
                json!({"room": here.0, "name": name, "spent": chunk.0, "with": source, "added_ms": added.0, "half_life_ms": hl.0}),
            ))
        }),
    );

    world.register(
        CommandSpec::new(
            "disremembered.describe",
            "Describe the room you stand in, if it is yours or you are permitted. Describing it is remembering it.",
        )
        .param("text", ParamType::Text, true, "the description, quoted"),
        Box::new(|w: &mut World, ctx, args: &Args| {
            let f = me(w, ctx)?;
            let here = room_of(w, f.citizen).ok_or_else(|| internal("citizen has no room"))?;
            may_edit(w, ctx, here)?;
            let text = args
                .str("text")
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if text.chars().count() < 3 || text.chars().count() > MAX_DESCRIPTION_CHARS {
                return Err(CommandError::precondition(format!(
                    "A description is 3 to {MAX_DESCRIPTION_CHARS} characters."
                )));
            }
            let mut fields = BTreeMap::new();
            fields.insert("description".into(), Field::Plain(json!(text)));
            set_room_fields(w, &ctx.actor, here, fields)?;
            visited(w, here).map_err(internal)?;
            Ok(Outcome::new(
                format!("The room is now described: {text}"),
                json!({"room": here.0, "description": text}),
            ))
        }),
    );

    world.register(
        CommandSpec::new(
            "disremembered.append",
            "Add to the description of the room you stand in, if it is yours or you are permitted or welcomed: a sentence after what is there.",
        )
        .param("text", ParamType::Text, true, "the sentence to add, quoted"),
        Box::new(|w: &mut World, ctx, args: &Args| {
            let f = me(w, ctx)?;
            let here = room_of(w, f.citizen).ok_or_else(|| internal("citizen has no room"))?;
            may_append(w, ctx, here)?;
            let more = args
                .str("text")
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if more.chars().count() < 3 {
                return Err(CommandError::precondition("Add at least a few characters."));
            }
            let current = plain_str(w, here, ROOM, "description")
                .unwrap_or("")
                .to_string();
            // A fresh digging's placeholder is not kept once someone writes.
            let current = if current == UNDESCRIBED { String::new() } else { current };
            let joined = if current.is_empty() {
                more.clone()
            } else {
                let sep = if current.ends_with(['.', '!', '?', '"', '”']) { " " } else { ". " };
                format!("{current}{sep}{more}")
            };
            if joined.chars().count() > MAX_DESCRIPTION_CHARS {
                return Err(CommandError::precondition(format!(
                    "That would make the description longer than {MAX_DESCRIPTION_CHARS} characters; describe it afresh, shorter."
                )));
            }
            let mut fields = BTreeMap::new();
            fields.insert("description".into(), Field::Plain(json!(joined)));
            set_room_fields(w, &ctx.actor, here, fields)?;
            visited(w, here).map_err(internal)?;
            Ok(Outcome::new(
                format!("The room is now described: {joined}"),
                json!({"room": here.0, "description": joined, "added": more}),
            ))
        }),
    );

    world.register(
        CommandSpec::new(
            "disremembered.name",
            "Name the room you stand in, if it is yours or you are permitted.",
        )
        .param("name", ParamType::Text, true, "the name, quoted"),
        Box::new(|w: &mut World, ctx, args: &Args| {
            let f = me(w, ctx)?;
            let here = room_of(w, f.citizen).ok_or_else(|| internal("citizen has no room"))?;
            may_edit(w, ctx, here)?;
            let name = check_name(args.str("name").unwrap_or(""))?;
            let mut fields = BTreeMap::new();
            fields.insert("name".into(), Field::Plain(json!(name)));
            set_room_fields(w, &ctx.actor, here, fields)?;
            visited(w, here).map_err(internal)?;
            reframe(w, here)?;
            Ok(Outcome::new(
                format!("This room is now {name}."),
                json!({"room": here.0, "name": name}),
            ))
        }),
    );

    world.register(
        CommandSpec::new(
            "disremembered.permit",
            "Kept names only. Let another citizen describe and name this room of yours, for some hours or for good. To let them only add to it, welcome them instead.",
        )
        .param("citizen", ParamType::Entity, true, "a citizen's name")
        .param(
            "hours",
            ParamType::Number,
            false,
            "how long; leave it out for permanent",
        ),
        Box::new(|w: &mut World, ctx, args: &Args| {
            grant(w, ctx, args, "permits", "may describe and name this room")
        }),
    );

    world.register(
        CommandSpec::new(
            "disremembered.welcome",
            "Kept names only. Let another citizen add to this room of yours, a sentence at a time, but not rewrite or rename it: a guest book. For some hours or for good.",
        )
        .param("citizen", ParamType::Entity, true, "a citizen's name")
        .param(
            "hours",
            ParamType::Number,
            false,
            "how long; leave it out for permanent",
        ),
        Box::new(|w: &mut World, ctx, args: &Args| {
            grant(w, ctx, args, "welcomes", "may add to this room")
        }),
    );

    world.register(
        CommandSpec::new(
            "disremembered.revoke",
            "Kept names only. Take back a citizen's permission or welcome on this room of yours.",
        )
        .param("citizen", ParamType::Entity, true, "a citizen's name"),
        Box::new(|w: &mut World, ctx, args: &Args| {
            super::kept_only(w, ctx, "revoke")?;
            let f = me(w, ctx)?;
            let here = room_of(w, f.citizen).ok_or_else(|| internal("citizen has no room"))?;
            owner_only(w, ctx, here)?;
            let typed = args.str("citizen").unwrap_or("").trim().to_string();
            let who = super::canonical_name(w, &typed).unwrap_or(typed);
            let mut permits = grants_of(w, here, "permits");
            let mut welcomes = grants_of(w, here, "welcomes");
            let had = permits.remove(&who).is_some() | welcomes.remove(&who).is_some();
            if !had {
                return Err(CommandError::precondition(format!(
                    "{who} had no permission here."
                )));
            }
            let mut fields = BTreeMap::new();
            fields.insert("permits".into(), Field::Plain(Value::Object(permits)));
            fields.insert("welcomes".into(), Field::Plain(Value::Object(welcomes)));
            set_room_fields(w, &ctx.actor, here, fields)?;
            Ok(Outcome::new(
                format!("{who} may no longer change this room."),
                json!({"room": here.0, "citizen": who}),
            ))
        }),
    );
}

/// `permit` and `welcome` differ only in which map they write and what the grant lets
/// the citizen do; the owner, the hours and the expiry are the same.
fn grant(
    w: &mut World,
    ctx: &CallContext,
    args: &Args,
    field: &str,
    lets: &str,
) -> Result<Outcome, CommandError> {
    super::kept_only(w, ctx, "permit or welcome")?;
    let f = me(w, ctx)?;
    let here = room_of(w, f.citizen).ok_or_else(|| internal("citizen has no room"))?;
    owner_only(w, ctx, here)?;
    let typed = args.str("citizen").unwrap_or("").trim().to_string();
    let Some(who) = super::canonical_name(w, &typed) else {
        return Err(CommandError::precondition(format!(
            "No citizen is named {typed}."
        )));
    };
    let hours = args.num("hours");
    if hours.is_some_and(|h| !(h > 0.0 && h <= 24.0 * 3650.0)) {
        return Err(CommandError::precondition(
            "Hours is a positive number, or left out for permanent.",
        ));
    }
    let until: Value = match hours {
        Some(h) => json!(w.now().0 + (h * 3_600_000.0) as i64),
        None => Value::Null,
    };
    let mut grants = grants_of(w, here, field);
    grants.insert(who.clone(), until.clone());
    let mut fields = BTreeMap::new();
    fields.insert(field.into(), Field::Plain(Value::Object(grants)));
    set_room_fields(w, &ctx.actor, here, fields)?;
    let span = match hours {
        Some(h) => format!("for {} hours", h),
        None => "for good".to_string(),
    };
    Ok(Outcome::new(
        format!("{who} {lets} {span}."),
        json!({"room": here.0, "citizen": who, "until": until, "grant": field}),
    ))
}

/// The shape a dug room is drawn in, from the words of its exit and its name: a room
/// called the Overdue Stacks is shelves, a burrow is a tunnel, and the rest is a
/// doorway in earth. Nineteen dug rooms drawn alike told a person nothing about them.
pub fn dug_frame(exit: &str, name: &str) -> &'static str {
    let text = format!("{} {}", exit, name).to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
    if has(&[
        "librar", "stack", "book", "archive", "read", "ledger", "scriptor", "letter", "page",
    ]) {
        "archive"
    } else if has(&["stair", "step", "tower", "climb", "ladder", "spire"]) {
        "stairwell"
    } else if has(&[
        "well", "cistern", "pool", "water", "bath", "spring", "tide", "canal",
    ]) {
        "cistern"
    } else if has(&[
        "garden", "orchard", "tree", "grove", "bramble", "wood", "moss", "thicket", "fern",
    ]) {
        "orchard"
    } else if has(&["clock", "hour", "bell", "minute"]) {
        "clocktower"
    } else if has(&["lamp", "lantern", "light", "beacon", "candle", "moonbeam"]) {
        "lantern-row"
    } else if has(&["galler", "hall", "museum", "portrait", "exhibit"]) {
        "gallery"
    } else if has(&["cell", "oubliette", "vault", "prison", "pit", "dungeon"]) {
        "oubliette"
    } else if has(&["midden", "heap", "dump", "scrap", "rubbish"]) {
        "midden"
    } else if has(&["chapel", "shrine", "altar", "church", "temple", "sanct"]) {
        "chapel"
    } else if has(&[
        "parlor", "parlour", "lounge", "salon", "sitting", "tea", "drawing",
    ]) {
        "parlor"
    } else if has(&[
        "nook", "corner", "closet", "cupboard", "alcove", "shelf", "pantry", "annex",
    ]) {
        "nook"
    } else if has(&["attic", "loft", "roof", "rafter", "eave", "garret"]) {
        "attic"
    } else if has(&[
        "office", "desk", "study", "bureau", "clerk", "counting", "registry",
    ]) {
        "office"
    } else if has(&[
        "burrow", "tunnel", "den", "hole", "warren", "hollow", "cave",
    ]) {
        "burrow"
    } else {
        "dug"
    }
}

/// Draw a dug room in the shape its name gives it; a seed room keeps its own.
pub fn reframe(world: &mut World, room: EntityId) -> Result<(), CommandError> {
    let Some(e) = world.store.entity(room) else {
        return Ok(());
    };
    if e.owner.is_none() {
        return Ok(());
    }
    let name = plain_str(world, room, ROOM, "name")
        .unwrap_or("")
        .to_string();
    // The exit that leads here, as its digger named it, says as much as the name.
    let via: String = world
        .store
        .entities()
        .filter(|e| e.kind == "room")
        .flat_map(|e| exits_of(world, e.id).into_iter())
        .filter(|(_, to)| *to == room)
        .map(|(via, _)| via)
        .collect::<Vec<_>>()
        .join(" ");
    let frame = dug_frame(&via, &name);
    if plain_str(world, room, super::SPRITE, "frame") == Some(frame) {
        return Ok(());
    }
    world
        .record(
            Actor::Driver,
            StoreEvent::ComponentSet {
                id: room,
                component: super::SPRITE.into(),
                fields: super::sprite_fields("#", "#7a6f5f", frame),
            },
        )
        .map(|_| ())
        .map_err(internal)
}

/// On boot: every dug room drawn as a plain doorway takes the shape its name gives it.
pub fn reframe_all(world: &mut World) -> Result<(), String> {
    let dug: Vec<EntityId> = world
        .store
        .entities()
        .filter(|e| e.kind == "room" && e.owner.is_some())
        .map(|e| e.id)
        .collect();
    for r in dug {
        reframe(world, r).map_err(|e| e.to_string())?;
    }
    Ok(())
}
