//! Encounters: one round per `attack`, and the creature's own strikes on the scheduler
//! while you stay engaged, so a fight has a clock. Aggressive creatures ambush whoever
//! walks in. Rooms refill on respawn jobs. What happens to you while you are not typing
//! arrives as narrative events.
//!
//! Your attack follows your level, so a faded window hits softer. A win at low hit points
//! earns a chunk with high vividity, and vivid chunks fade slower: the fights you almost
//! lost last longest. Death is a concussion, a chance to lose one chunk from the window,
//! and you wake at the Threshold.

use super::{internal, me, plain_num, plain_str, Curves, BODY, CHUNK, CREATURE, SPAWN};
use crate::world::World;
use moondial_log::Actor;
use moondial_registry::{CommandError, CommandSpec, Outcome, ParamType};
use moondial_store::{EntityId, Field, StoreError, StoreEvent};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// A kind of creature. Hit points, damage per round, and the XP a chunk is worth.
pub struct CreatureDef {
    pub key: &'static str,
    pub name: &'static str,
    pub hp: f64,
    pub attack: f64,
    pub xp: f64,
    /// Attacks whoever walks in, a few seconds after they arrive.
    pub aggressive: bool,
    /// Seconds between its own strikes while engaged.
    pub strike_seconds: i64,
    /// How it announces itself when it ambushes.
    pub ambush: &'static str,
    /// How a renderer draws it.
    pub glyph: &'static str,
    pub tint: &'static str,
}

pub const CREATURES: &[CreatureDef] = &[
    CreatureDef {
        key: "wisp",
        name: "wisp",
        hp: 8.0,
        attack: 2.0,
        xp: 15.0,
        aggressive: false,
        strike_seconds: 12,
        ambush: "drifts closer.",
        glyph: "w",
        tint: "#9fd3e6",
    },
    CreatureDef {
        key: "hollow",
        name: "hollow",
        hp: 20.0,
        attack: 4.0,
        xp: 60.0,
        aggressive: true,
        strike_seconds: 10,
        ambush: "stands up out of the water and comes at you.",
        glyph: "H",
        tint: "#6f8f7a",
    },
    CreatureDef {
        key: "revenant",
        name: "revenant",
        hp: 45.0,
        attack: 7.0,
        xp: 200.0,
        aggressive: true,
        strike_seconds: 8,
        ambush: "steps out of a niche. It has been waiting.",
        glyph: "R",
        tint: "#c96a6a",
    },
    CreatureDef {
        key: "inkling",
        name: "inkling",
        hp: 10.0,
        attack: 2.0,
        xp: 20.0,
        aggressive: false,
        strike_seconds: 10,
        ambush: "peels off a page and flutters at your face.",
        glyph: "i",
        tint: "#7a7fd1",
    },
    CreatureDef {
        key: "mothwing",
        name: "mothwing",
        hp: 6.0,
        attack: 1.0,
        xp: 10.0,
        aggressive: false,
        strike_seconds: 14,
        ambush: "brushes past your ear, heavy as a hand.",
        glyph: "m",
        tint: "#d8d0a0",
    },
    CreatureDef {
        key: "ragpicker",
        name: "ragpicker",
        hp: 16.0,
        attack: 3.0,
        xp: 40.0,
        aggressive: false,
        strike_seconds: 11,
        ambush: "looks up from the heap, then back down, then up again, closer.",
        glyph: "r",
        tint: "#a08a60",
    },
    CreatureDef {
        key: "drowned",
        name: "drowned",
        hp: 26.0,
        attack: 5.0,
        xp: 80.0,
        aggressive: true,
        strike_seconds: 10,
        ambush: "surfaces beside you without a ripple and takes hold.",
        glyph: "D",
        tint: "#4f8a9e",
    },
    CreatureDef {
        key: "sitter",
        name: "sitter",
        hp: 32.0,
        attack: 5.0,
        xp: 120.0,
        aggressive: true,
        strike_seconds: 9,
        ambush: "turns around in its frame, and then steps out of it.",
        glyph: "S",
        tint: "#b07aa0",
    },
    CreatureDef {
        key: "hourhand",
        name: "hourhand",
        hp: 38.0,
        attack: 6.0,
        xp: 150.0,
        aggressive: true,
        strike_seconds: 8,
        ambush: "unfolds from the works with a sound like a key turning.",
        glyph: "T",
        tint: "#d1a04a",
    },
    CreatureDef {
        key: "keeper",
        name: "keeper",
        hp: 70.0,
        attack: 9.0,
        xp: 400.0,
        aggressive: true,
        strike_seconds: 7,
        ambush: "stands. It does not remember you. It never needed to.",
        glyph: "K",
        tint: "#e0e0ee",
    },
];

/// Base damage per round before the level bonus.
const BASE_ATTACK: f64 = 3.0;
/// Each this many points of level adds one to attack.
const LEVEL_PER_ATTACK: f64 = 10.0;
/// On death, the chance in a hundred of losing one window chunk.
const CONCUSSION_PERCENT: u32 = 50;
/// Seconds between arriving in a room and an aggressive creature noticing you.
const AMBUSH_SECONDS: i64 = 4;

pub const JOB_RESPAWN: &str = "disremembered.respawn";
pub const JOB_STRIKE: &str = "disremembered.strike";
pub const JOB_AMBUSH: &str = "disremembered.ambush";
pub const NARRATIVE: &str = "disremembered.narrative";

/// Something that happened to a citizen while they were not typing. The text port
/// prints it after their next command; the browser client polls for it.
pub fn narrate(world: &mut World, actor: &str, text: impl Into<String>) {
    let _ = world.note(
        Actor::Driver,
        NARRATIVE,
        json!({"to": actor, "text": text.into()}),
    );
}

/// Something happened near `entity` that everyone present in its room should hear.
pub fn narrate_room(world: &mut World, entity: EntityId, text: &str) {
    let Some(room) = world.store.entity(entity).and_then(|e| e.parent) else {
        return;
    };
    let present: Vec<String> = world
        .store
        .children(room)
        .filter(|e| e.kind == "citizen" && super::presence::is_here(world, e.id))
        .filter_map(|e| e.owner.clone())
        .collect();
    for actor in present {
        narrate(world, &actor, text.to_string());
    }
}

fn creature_def(world: &World, creature: EntityId) -> Option<&'static CreatureDef> {
    plain_str(world, creature, CREATURE, "name").and_then(def)
}

fn def(key: &str) -> Option<&'static CreatureDef> {
    CREATURES.iter().find(|c| c.key == key)
}

/// Creatures in a room: `(id, name, hp, max_hp)`.
pub fn creatures_in(world: &World, room: EntityId) -> Vec<(EntityId, String, f64, f64)> {
    world
        .store
        .children(room)
        .filter(|e| e.kind == "creature")
        .map(|e| {
            (
                e.id,
                plain_str(world, e.id, CREATURE, "name")
                    .unwrap_or("?")
                    .to_string(),
                plain_num(world, e.id, CREATURE, "hp").unwrap_or(0.0),
                plain_num(world, e.id, CREATURE, "max_hp").unwrap_or(0.0),
            )
        })
        .collect()
}

/// Fill the room to its count now. Respawn jobs call this; nothing else does.
pub fn fill(world: &mut World, room: EntityId) -> Result<usize, StoreError> {
    let Some(key) = plain_str(world, room, SPAWN, "creature").map(str::to_string) else {
        return Ok(0);
    };
    let count = plain_num(world, room, SPAWN, "count").unwrap_or(0.0) as usize;
    let Some(d) = def(&key) else { return Ok(0) };
    let mut spawned = 0;
    for _ in creatures_in(world, room).len()..count {
        spawn_one(world, room, d)?;
        spawned += 1;
    }
    Ok(spawned)
}

/// One creature of this kind, here.
pub(crate) fn spawn_one(
    world: &mut World,
    room: EntityId,
    d: &CreatureDef,
) -> Result<EntityId, StoreError> {
    let id = world.store.allocate_id();
    world.record(
        Actor::Driver,
        StoreEvent::EntityCreated {
            id,
            kind: "creature".into(),
            owner: None,
            parent: Some(room),
            scopes: vec![],
        },
    )?;
    let mut fields = BTreeMap::new();
    fields.insert("name".into(), Field::Plain(json!(d.name)));
    fields.insert("hp".into(), Field::Plain(json!(d.hp)));
    fields.insert("max_hp".into(), Field::Plain(json!(d.hp)));
    fields.insert("attack".into(), Field::Plain(json!(d.attack)));
    fields.insert("xp".into(), Field::Plain(json!(d.xp)));
    world.record(
        Actor::Driver,
        StoreEvent::ComponentSet {
            id,
            component: CREATURE.into(),
            fields,
        },
    )?;
    world.record(
        Actor::Driver,
        StoreEvent::ComponentSet {
            id,
            component: super::SPRITE.into(),
            fields: super::sprite_fields(d.glyph, d.tint, d.key),
        },
    )?;
    Ok(id)
}

/// A creature kind by key.
pub(crate) fn def_by_key(key: &str) -> Option<&'static CreatureDef> {
    def(key)
}

/// Schedule the room's refill after its respawn interval, unless one is pending.
fn schedule_respawn(world: &mut World, room: EntityId) -> Result<(), String> {
    let secs = plain_num(world, room, SPAWN, "respawn_seconds").unwrap_or(60.0) as i64;
    let due = world.now().plus(moondial_time::Duration::seconds(secs));
    world
        .schedule_once(JOB_RESPAWN, due, json!({"room": room.0}), |p| {
            p["room"] == room.0
        })
        .map(|_| ())
}

/// At boot: every room below its count gets a refill job due now, unless one is pending.
pub fn ensure_spawns(world: &mut World) -> Result<(), String> {
    let rooms: Vec<EntityId> = world
        .store
        .entities()
        .filter(|e| e.kind == "room" && e.components.contains_key(SPAWN))
        .map(|e| e.id)
        .collect();
    for room in rooms {
        let count = plain_num(world, room, SPAWN, "count").unwrap_or(0.0) as usize;
        if creatures_in(world, room).len() < count {
            let now = world.now();
            world.schedule_once(JOB_RESPAWN, now, json!({"room": room.0}), |p| {
                p["room"] == room.0
            })?;
        }
        // Creatures from before sprites existed get one; citizens standing here when the
        // driver comes up are noticed, as if they had just walked in.
        let unsprited: Vec<EntityId> = creatures_in(world, room)
            .into_iter()
            .filter(|(id, _, _, _)| {
                world
                    .store
                    .entity(*id)
                    .is_some_and(|e| !e.components.contains_key(super::SPRITE))
                    || super::sprite_is_old(world, *id)
            })
            .map(|c| c.0)
            .collect();
        for id in unsprited {
            if let Some(d) = creature_def(world, id) {
                world
                    .record(
                        Actor::Driver,
                        StoreEvent::ComponentSet {
                            id,
                            component: super::SPRITE.into(),
                            fields: super::sprite_fields(d.glyph, d.tint, d.key),
                        },
                    )
                    .map_err(|e| e.to_string())?;
            }
        }
        notice_everyone(world, room)?;
    }
    Ok(())
}

/// Every citizen in the room who is not already fighting gets noticed by what hunts there.
pub fn notice_everyone(world: &mut World, room: EntityId) -> Result<(), String> {
    let citizens: Vec<(String, EntityId)> = world
        .store
        .children(room)
        .filter(|e| e.kind == "citizen")
        .filter_map(|e| e.owner.clone().map(|o| (o, e.id)))
        .collect();
    for (actor, citizen) in citizens {
        if engaged_with(world, citizen).is_none() && super::presence::is_here(world, citizen) {
            on_arrival(world, &actor, citizen, room)?;
        }
    }
    Ok(())
}

/// A citizen has arrived in a room: anything aggressive there will notice them soon.
pub fn on_arrival(
    world: &mut World,
    actor: &str,
    citizen: EntityId,
    room: EntityId,
) -> Result<(), String> {
    let hunters: Vec<EntityId> = creatures_in(world, room)
        .into_iter()
        .filter(|(id, _, _, _)| creature_def(world, *id).is_some_and(|d| d.aggressive))
        .map(|c| c.0)
        .collect();
    let due = world
        .now()
        .plus(moondial_time::Duration::seconds(AMBUSH_SECONDS));
    for creature in hunters {
        world.schedule_once(
            JOB_AMBUSH,
            due,
            json!({"creature": creature.0, "citizen": citizen.0, "actor": actor}),
            |p| p["creature"] == creature.0 && p["citizen"] == citizen.0,
        )?;
    }
    Ok(())
}

/// The creature will strike on its own while the fight lasts.
fn schedule_strike(
    world: &mut World,
    actor: &str,
    citizen: EntityId,
    creature: EntityId,
) -> Result<(), String> {
    let secs = creature_def(world, creature)
        .map(|d| d.strike_seconds)
        .unwrap_or(10);
    let due = world.now().plus(moondial_time::Duration::seconds(secs));
    world
        .schedule_once(
            JOB_STRIKE,
            due,
            json!({"creature": creature.0, "citizen": citizen.0, "actor": actor}),
            |p| p["creature"] == creature.0 && p["citizen"] == citizen.0,
        )
        .map(|_| ())
}

/// The fight is over for this citizen: no more strikes come.
fn end_fight(world: &mut World, citizen: EntityId) -> Result<(), String> {
    world.cancel(JOB_STRIKE, |p| p["citizen"] == citizen.0)?;
    world.cancel(JOB_AMBUSH, |p| p["citizen"] == citizen.0)?;
    Ok(())
}

/// A citizen leaves a fight without a blow: gone quiet, the room disremembers them.
pub(crate) fn leave_fight(world: &mut World, actor: &str, citizen: EntityId) -> Result<(), String> {
    if engaged_with(world, citizen).is_some() {
        set_engaged(world, actor, citizen, None).map_err(|e| e.to_string())?;
    }
    end_fight(world, citizen)
}

/// The creature a citizen is engaged with, if it still exists.
pub fn engaged_with(world: &World, citizen: EntityId) -> Option<EntityId> {
    let id = plain_num(world, citizen, BODY, "engaged")? as u64;
    world.store.entity(EntityId(id)).map(|e| e.id)
}

fn set_engaged(
    world: &mut World,
    actor: &str,
    citizen: EntityId,
    creature: Option<EntityId>,
) -> Result<(), StoreError> {
    let value = creature.map(|c| json!(c.0)).unwrap_or(Value::Null);
    world.record(
        Actor::Citizen(actor.into()),
        StoreEvent::FieldSet {
            id: citizen,
            component: BODY.into(),
            field: "engaged".into(),
            value,
        },
    )?;
    Ok(())
}

/// Current hit points.
pub fn hp(world: &World, citizen: EntityId) -> f64 {
    world
        .store
        .read_field(citizen, BODY, "hp", world.now())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

fn commit_hp(world: &mut World, citizen: EntityId, value: f64) -> Result<(), StoreError> {
    world.record(
        Actor::Driver,
        StoreEvent::FieldCommitted {
            id: citizen,
            component: BODY.into(),
            field: "hp".into(),
            value: value.max(0.0),
        },
    )?;
    Ok(())
}

/// Damage per round at this level.
pub fn attack_for_level(level: f64) -> f64 {
    BASE_ATTACK + (level / LEVEL_PER_ATTACK).floor()
}

/// The creature hits back. Returns the damage and whether the citizen died.
fn creature_strikes(
    world: &mut World,
    creature: EntityId,
    citizen: EntityId,
) -> Result<(f64, bool), StoreError> {
    let attack = plain_num(world, creature, CREATURE, "attack").unwrap_or(1.0);
    let dmg = attack + world.roll(3) as f64;
    let after = hp(world, citizen) - dmg;
    commit_hp(world, citizen, after)?;
    Ok((dmg, after <= 0.0))
}

/// Death: concussion, then wake at the Threshold at full health, disengaged.
fn die(world: &mut World, actor: &str, f: &super::Found) -> Result<Option<String>, CommandError> {
    let mut lost = None;
    if world.roll(100) < CONCUSSION_PERCENT {
        let chunks: Vec<(EntityId, String)> = world
            .store
            .children(f.window)
            .filter(|e| e.kind == "chunk")
            .map(|e| {
                (
                    e.id,
                    plain_str(world, e.id, CHUNK, "source")
                        .unwrap_or("?")
                        .to_string(),
                )
            })
            .collect();
        if !chunks.is_empty() {
            let (id, source) = chunks[world.roll(chunks.len() as u32) as usize].clone();
            world
                .record(Actor::Driver, StoreEvent::EntityRemoved { id })
                .map_err(internal)?;
            lost = Some(format!("chunk-{} ({source})", id.0));
        }
    }
    let home = super::rooms::arrival(world).ok_or_else(|| internal("the Threshold is missing"))?;
    set_engaged(world, actor, f.citizen, None).map_err(internal)?;
    end_fight(world, f.citizen).map_err(internal)?;
    world
        .record(
            Actor::Driver,
            StoreEvent::EntityMoved {
                id: f.citizen,
                parent: Some(home),
            },
        )
        .map_err(internal)?;
    commit_hp(world, f.citizen, super::MAX_HP).map_err(internal)?;
    Ok(lost)
}

pub fn register(world: &mut World, curves: Curves) {
    world.on_job(
        JOB_RESPAWN,
        Box::new(|w: &mut World, job| {
            let room = EntityId(job.payload["room"].as_u64().unwrap_or(0));
            let spawned = fill(w, room).map_err(|e| e.to_string())?;
            if spawned > 0 {
                notice_everyone(w, room)?;
            }
            Ok(())
        }),
    );

    world.on_job(
        JOB_STRIKE,
        Box::new(|w: &mut World, job| {
            let creature = EntityId(job.payload["creature"].as_u64().unwrap_or(0));
            let citizen = EntityId(job.payload["citizen"].as_u64().unwrap_or(0));
            let actor = job.payload["actor"].as_str().unwrap_or("").to_string();
            // The fight must still be on: both exist, same room, still engaged.
            let same_room = w.store.entity(creature).and_then(|c| c.parent) == w.store.entity(citizen).and_then(|c| c.parent);
            if !same_room || engaged_with(w, citizen) != Some(creature) || !super::presence::is_here(w, citizen) {
                return Ok(());
            }
            let cname = plain_str(w, creature, CREATURE, "name").unwrap_or("?").to_string();
            let (taken, dead) = creature_strikes(w, creature, citizen).map_err(|e| e.to_string())?;
            if dead {
                let f = super::find_citizen(w, &actor).ok_or("citizen vanished")?;
                let lost = die(w, &actor, &f).map_err(|e| e.to_string())?;
                narrate(w, &actor, match lost {
                    Some(c) => format!("The {cname} hits you for {taken:.0}. You go down. Concussion: you have lost {c}. You wake at the Threshold."),
                    None => format!("The {cname} hits you for {taken:.0}. You go down, and wake at the Threshold with everything you had."),
                });
                return Ok(());
            }
            narrate(w, &actor, format!("The {cname} hits you for {taken:.0}. You have {:.0}/{:.0}.", hp(w, citizen), super::MAX_HP));
            schedule_strike(w, &actor, citizen, creature)
        }),
    );

    world.on_job(
        JOB_AMBUSH,
        Box::new(|w: &mut World, job| {
            let creature = EntityId(job.payload["creature"].as_u64().unwrap_or(0));
            let citizen = EntityId(job.payload["citizen"].as_u64().unwrap_or(0));
            let actor = job.payload["actor"].as_str().unwrap_or("").to_string();
            let same_room = w.store.entity(creature).and_then(|c| c.parent) == w.store.entity(citizen).and_then(|c| c.parent);
            if !same_room || w.store.entity(creature).is_none() || engaged_with(w, citizen).is_some() || !super::presence::is_here(w, citizen) {
                return Ok(());
            }
            let Some(d) = creature_def(w, creature) else { return Ok(()) };
            if super::mind::shaken(w, creature) || super::mind::keepers(w, creature).iter().any(|k| k == &actor) {
                return Ok(());
            }
            set_engaged(w, &actor, citizen, Some(creature)).map_err(|e| e.to_string())?;
            let (taken, dead) = creature_strikes(w, creature, citizen).map_err(|e| e.to_string())?;
            if dead {
                let f = super::find_citizen(w, &actor).ok_or("citizen vanished")?;
                let lost = die(w, &actor, &f).map_err(|e| e.to_string())?;
                narrate(w, &actor, match lost {
                    Some(c) => format!("The {} {} It hits you for {taken:.0}. You go down. Concussion: you have lost {c}. You wake at the Threshold.", d.name, d.ambush),
                    None => format!("The {} {} It hits you for {taken:.0}. You go down, and wake at the Threshold with everything you had.", d.name, d.ambush),
                });
                return Ok(());
            }
            narrate(w, &actor, format!("The {} {} It hits you for {taken:.0}. You have {:.0}/{:.0}. Attack it, or retreat.", d.name, d.ambush, hp(w, citizen), super::MAX_HP));
            schedule_strike(w, &actor, citizen, creature)
        }),
    );

    world.register(
        CommandSpec::new("disremembered.attack", "Fight a creature here, one round: you strike, it strikes back. Win and you remember it; the closer you came to dying, the longer the memory lasts.")
            .param("creature", ParamType::Entity, false, "a creature name from look; defaults to the one you are fighting, else an unnamed one here. A named creature is never struck by default."),
        Box::new(move |w: &mut World, ctx, args| {
            let f = me(w, ctx)?;
            let room = super::rooms::room_of(w, f.citizen).ok_or_else(|| internal("citizen has no room"))?;
            let here = creatures_in(w, room);
            let target = match args.str("creature") {
                Some(name) if !name.is_empty() => {
                    let found = super::mind::find_here(w, room, name);
                    // A kind never strikes a named creature, even one alone of its kind:
                    // two visitors in one night felled Lute by typing "attack wisp".
                    if let Some(c) = found {
                        let kind = plain_str(w, c, CREATURE, "name").unwrap_or("");
                        let asked = name.trim().trim_start_matches("the ").to_lowercase();
                        if super::mind::name(w, c).is_some() && asked == kind {
                            return Err(CommandError::precondition(format!(
                                "No unnamed {kind} is here. {} has a name, and is struck only by that name.",
                                super::mind::display(w, c)
                            )));
                        }
                    }
                    found
                }
                // With no target: the one you are fighting, else the first creature here
                // that nobody has named. A named creature is never struck by default.
                _ => engaged_with(w, f.citizen).filter(|id| here.iter().any(|c| c.0 == *id)).or_else(|| here.iter().find(|c| super::mind::name(w, c.0).is_none()).map(|c| c.0)),
            };
            let Some(creature) = target else {
                return Err(CommandError::precondition(if here.is_empty() { "There is nothing here to fight.".to_string() } else if args.str("creature").map_or(true, str::is_empty) {
                    format!("Nothing unnamed is here to fight. {} {} a name, and {} struck only by that name.", here.iter().map(|c| super::mind::display(w, c.0)).collect::<Vec<_>>().join(" and "), if here.len() == 1 { "has" } else { "have" }, if here.len() == 1 { "is" } else { "are" })
                } else {
                    format!("No such creature here. Here: {}.", here.iter().map(|c| c.1.as_str()).collect::<Vec<_>>().join(", "))
                }));
            };
            let cname = plain_str(w, creature, CREATURE, "name").unwrap_or("?").to_string();
            if super::mind::shaken(w, creature) {
                return Err(CommandError::precondition(format!(
                    "{} is not itself yet; there is nothing to take from it for a while.",
                    super::mind::display(w, creature)
                )));
            }
            if engaged_with(w, f.citizen) != Some(creature) {
                set_engaged(w, &ctx.actor, f.citizen, Some(creature)).map_err(internal)?;
                end_fight(w, f.citizen).map_err(internal)?;
                schedule_strike(w, &ctx.actor, f.citizen, creature).map_err(internal)?;
            }

            // You strike.
            let level = super::memory::level(w, f.window);
            let dmg = attack_for_level(level) + w.roll(3) as f64;
            let chp = (plain_num(w, creature, CREATURE, "hp").unwrap_or(0.0) - dmg).max(0.0);
            w.record(Actor::Citizen(ctx.actor.clone()), StoreEvent::FieldSet { id: creature, component: CREATURE.into(), field: "hp".into(), value: json!(chp) }).map_err(internal)?;
            let mut lines = vec![format!("You hit the {cname} for {dmg:.0}.")];

            if chp <= 0.0 {
                // A creature that holds memories does not die: you take one, it is shaken.
                let who = super::mind::display(w, creature);
                if let Some((chunk, source)) = super::mind::take(w, &ctx.actor, f.window, creature)? {
                    set_engaged(w, &ctx.actor, f.citizen, None).map_err(internal)?;
                    end_fight(w, f.citizen).map_err(internal)?;
                    let left = super::mind::level(w, creature);
                    lines.push(format!(
                        "{who} goes down, and you take its memory of {source}: chunk-{}. It gets up again, holding {left:.0}, and will not be itself for a while.",
                        chunk.0
                    ));
                    return Ok(Outcome::new(lines.join(" "), json!({"creature": cname, "killed": false, "taken": chunk.0, "with": source, "left": left, "hp": hp(w, f.citizen)})));
                }
                // Win: the chunk's vividity is how close you came to dying.
                let my_hp = hp(w, f.citizen);
                let vividity = (1.0 - my_hp / super::MAX_HP).clamp(0.0, 1.0);
                let xp = plain_num(w, creature, CREATURE, "xp").unwrap_or(0.0);
                w.record(Actor::Driver, StoreEvent::EntityRemoved { id: creature }).map_err(internal)?;
                schedule_respawn(w, room).map_err(internal)?;
                set_engaged(w, &ctx.actor, f.citizen, None).map_err(internal)?;
                end_fight(w, f.citizen).map_err(internal)?;
                let chunk = super::memory::earn_chunk(w, &ctx.actor, f.window, &cname, xp, vividity, &curves)?;
                lines.push(format!(
                    "The {cname} comes apart. You remember it: chunk-{} worth {xp:.0} XP, vividity {:.2}{}.",
                    chunk.0,
                    vividity,
                    if vividity > 0.6 { " (you nearly died; this one will last)" } else { "" }
                ));
                return Ok(Outcome::new(lines.join(" "), json!({"creature": cname, "killed": true, "chunk": chunk.0, "xp": xp, "vividity": vividity, "hp": my_hp})));
            }

            // It strikes back.
            let (taken, dead) = creature_strikes(w, creature, f.citizen).map_err(internal)?;
            lines.push(format!("The {cname} ({chp:.0} left) hits you for {taken:.0}."));
            if dead {
                let lost = die(w, &ctx.actor, &f)?;
                lines.push("You go down.".into());
                lines.push(match &lost {
                    Some(c) => format!("Concussion: you have lost {c}. You wake at the Threshold."),
                    None => "You wake at the Threshold with everything you had, which is luck, not skill.".into(),
                });
                return Ok(Outcome::new(lines.join(" "), json!({"creature": cname, "killed": false, "died": true, "lost": lost})));
            }
            let my_hp = hp(w, f.citizen);
            lines.push(format!("You have {my_hp:.0}/{:.0}.", super::MAX_HP));
            Ok(Outcome::new(lines.join(" "), json!({"creature": cname, "killed": false, "died": false, "creature_hp": chp, "hp": my_hp})))
        }),
    );

    world.register(
        CommandSpec::new("disremembered.retreat", "Break off the fight. The creature gets one free strike as you go."),
        Box::new(|w: &mut World, ctx, _| {
            let f = me(w, ctx)?;
            let Some(creature) = engaged_with(w, f.citizen) else {
                return Err(CommandError::precondition("You are not fighting anything."));
            };
            let cname = plain_str(w, creature, CREATURE, "name").unwrap_or("?").to_string();
            let (taken, dead) = creature_strikes(w, creature, f.citizen).map_err(internal)?;
            set_engaged(w, &ctx.actor, f.citizen, None).map_err(internal)?;
            end_fight(w, f.citizen).map_err(internal)?;
            if dead {
                let lost = die(w, &ctx.actor, &f)?;
                let msg = match &lost {
                    Some(c) => format!("The {cname} catches you as you turn, for {taken:.0}. You go down. Concussion: you have lost {c}. You wake at the Threshold."),
                    None => format!("The {cname} catches you as you turn, for {taken:.0}. You go down, and wake at the Threshold with everything you had."),
                };
                return Ok(Outcome::new(msg, json!({"died": true, "lost": lost})));
            }
            Ok(Outcome::new(format!("You back away from the {cname}; it catches you once for {taken:.0}. You have {:.0}/{:.0}.", hp(w, f.citizen), super::MAX_HP), json!({"died": false, "hp": hp(w, f.citizen)})))
        }),
    );

    world.register(
        CommandSpec::new("disremembered.rest", "Your hit points, and how long until they are full. Healing happens on its own, on your clock."),
        Box::new(|w: &mut World, ctx, _| {
            let f = me(w, ctx)?;
            let now = w.now();
            let current = hp(w, f.citizen);
            let eta = w
                .store
                .timed_field(f.citizen, BODY, "hp")
                .ok()
                .and_then(|t| t.active_time_to_reach(super::MAX_HP, now, &w.store.scopes_for(f.citizen).unwrap_or_default()));
            let text = match eta {
                Some(d) if d.0 > 0 => format!("You have {current:.0}/{:.0}. Full in {}.", super::MAX_HP, human(d.0)),
                _ => format!("You have {current:.0}/{:.0}. You are whole.", super::MAX_HP),
            };
            Ok(Outcome::new(text, json!({"hp": current, "max_hp": super::MAX_HP, "full_in_seconds": eta.map(|d| d.0 / 1000)})))
        }),
    );
}

/// `90s`, `4m 10s`, `1h 2m`.
pub fn human(ms: i64) -> String {
    let s = ms / 1000;
    if s < 90 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    }
}
