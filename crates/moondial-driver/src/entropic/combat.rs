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

pub const JOB_RESPAWN: &str = "entropic.respawn";
pub const JOB_STRIKE: &str = "entropic.strike";
pub const JOB_AMBUSH: &str = "entropic.ambush";
pub const NARRATIVE: &str = "entropic.narrative";

/// Something that happened to a citizen while they were not typing. The text port
/// prints it after their next command; the browser client polls for it.
pub fn narrate(world: &mut World, actor: &str, text: impl Into<String>) {
    let _ = world.note(
        Actor::Driver,
        NARRATIVE,
        json!({"to": actor, "text": text.into()}),
    );
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
        spawned += 1;
    }
    Ok(spawned)
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
            fill(w, room).map(|_| ()).map_err(|e| e.to_string())
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
            if !same_room || engaged_with(w, citizen) != Some(creature) {
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
            if !same_room || w.store.entity(creature).is_none() || engaged_with(w, citizen).is_some() {
                return Ok(());
            }
            let Some(d) = creature_def(w, creature) else { return Ok(()) };
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
        CommandSpec::new("entropic.attack", "Fight a creature here, one round: you strike, it strikes back. Win and you remember it; the closer you came to dying, the longer the memory lasts.")
            .param("creature", ParamType::Entity, false, "a creature name from look; defaults to the one you are fighting"),
        Box::new(move |w: &mut World, ctx, args| {
            let f = me(w, ctx)?;
            let room = super::rooms::room_of(w, f.citizen).ok_or_else(|| internal("citizen has no room"))?;
            let here = creatures_in(w, room);
            let target = match args.str("creature") {
                Some(name) if !name.is_empty() => {
                    let name = name.trim_start_matches("creature-").to_ascii_lowercase();
                    here.iter().find(|(id, n, _, _)| *n == name || id.0.to_string() == name).map(|c| c.0)
                }
                _ => engaged_with(w, f.citizen).filter(|id| here.iter().any(|c| c.0 == *id)).or_else(|| here.first().map(|c| c.0)),
            };
            let Some(creature) = target else {
                return Err(CommandError::precondition(if here.is_empty() { "There is nothing here to fight.".to_string() } else {
                    format!("No such creature here. Here: {}.", here.iter().map(|c| c.1.as_str()).collect::<Vec<_>>().join(", "))
                }));
            };
            let cname = plain_str(w, creature, CREATURE, "name").unwrap_or("?").to_string();
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
        CommandSpec::new("entropic.retreat", "Break off the fight. The creature gets one free strike as you go."),
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
        CommandSpec::new("entropic.rest", "Your hit points, and how long until they are full. Healing happens on its own, on your clock."),
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
