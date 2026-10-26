//! Encounters: turn-based, one round per `attack`. Creatures only act when struck or
//! fled from, because the driver has no scheduler yet; when it does, they will hunt.
//!
//! Your attack follows your level, so a faded window hits softer. A win at low hit points
//! earns a chunk with high vividity, and vivid chunks fade slower: the fights you almost
//! lost last longest. Death is a concussion, a chance to lose one chunk from the window,
//! and you wake at the Gate.

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
}

pub const CREATURES: &[CreatureDef] = &[
    CreatureDef {
        key: "wisp",
        name: "wisp",
        hp: 8.0,
        attack: 2.0,
        xp: 15.0,
    },
    CreatureDef {
        key: "hollow",
        name: "hollow",
        hp: 20.0,
        attack: 4.0,
        xp: 60.0,
    },
    CreatureDef {
        key: "revenant",
        name: "revenant",
        hp: 45.0,
        attack: 7.0,
        xp: 200.0,
    },
];

/// Base damage per round before the level bonus.
const BASE_ATTACK: f64 = 3.0;
/// Each this many points of level adds one to attack.
const LEVEL_PER_ATTACK: f64 = 10.0;
/// On death, the chance in a hundred of losing one window chunk.
const CONCUSSION_PERCENT: u32 = 50;

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

/// Fill the room to its count once its respawn time has come. Called on look and attack,
/// since the driver has no scheduler yet.
pub fn maybe_spawn(world: &mut World, room: EntityId) -> Result<(), StoreError> {
    let Some(key) = plain_str(world, room, SPAWN, "creature").map(str::to_string) else {
        return Ok(());
    };
    let count = plain_num(world, room, SPAWN, "count").unwrap_or(0.0) as usize;
    let next_at = plain_num(world, room, SPAWN, "next_at").unwrap_or(0.0) as i64;
    if world.now().0 < next_at {
        return Ok(());
    }
    let Some(d) = def(&key) else { return Ok(()) };
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
    }
    Ok(())
}

/// A kill starts the room's respawn timer.
fn start_respawn(world: &mut World, room: EntityId) -> Result<(), StoreError> {
    let respawn_ms =
        (plain_num(world, room, SPAWN, "respawn_seconds").unwrap_or(60.0) * 1000.0) as i64;
    let next = world.now().0 + respawn_ms;
    world.record(
        Actor::Driver,
        StoreEvent::FieldSet {
            id: room,
            component: SPAWN.into(),
            field: "next_at".into(),
            value: json!(next),
        },
    )?;
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

/// Death: concussion, then wake at the Gate at full health, disengaged.
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
    let gate = super::rooms::the_gate(world).ok_or_else(|| internal("the Gate is missing"))?;
    set_engaged(world, actor, f.citizen, None).map_err(internal)?;
    world
        .record(
            Actor::Driver,
            StoreEvent::EntityMoved {
                id: f.citizen,
                parent: Some(gate),
            },
        )
        .map_err(internal)?;
    commit_hp(world, f.citizen, super::MAX_HP).map_err(internal)?;
    Ok(lost)
}

pub fn register(world: &mut World, curves: Curves) {
    world.registry.register(
        CommandSpec::new("entropic.attack", "Fight a creature here, one round: you strike, it strikes back. Win and you remember it; the closer you came to dying, the longer the memory lasts.")
            .param("creature", ParamType::Entity, false, "a creature name from look; defaults to the one you are fighting"),
        Box::new(move |w: &mut World, ctx, args| {
            let f = me(w, ctx)?;
            let room = super::rooms::room_of(w, f.citizen).ok_or_else(|| internal("citizen has no room"))?;
            maybe_spawn(w, room).map_err(internal)?;
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
                start_respawn(w, room).map_err(internal)?;
                set_engaged(w, &ctx.actor, f.citizen, None).map_err(internal)?;
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
                    Some(c) => format!("Concussion: you have lost {c}. You wake at the Gate."),
                    None => "You wake at the Gate with everything you had, which is luck, not skill.".into(),
                });
                return Ok(Outcome::new(lines.join(" "), json!({"creature": cname, "killed": false, "died": true, "lost": lost})));
            }
            let my_hp = hp(w, f.citizen);
            lines.push(format!("You have {my_hp:.0}/{:.0}.", super::MAX_HP));
            Ok(Outcome::new(lines.join(" "), json!({"creature": cname, "killed": false, "died": false, "creature_hp": chp, "hp": my_hp})))
        }),
    );

    world.registry.register(
        CommandSpec::new("entropic.retreat", "Break off the fight. The creature gets one free strike as you go."),
        Box::new(|w: &mut World, ctx, _| {
            let f = me(w, ctx)?;
            let Some(creature) = engaged_with(w, f.citizen) else {
                return Err(CommandError::precondition("You are not fighting anything."));
            };
            let cname = plain_str(w, creature, CREATURE, "name").unwrap_or("?").to_string();
            let (taken, dead) = creature_strikes(w, creature, f.citizen).map_err(internal)?;
            set_engaged(w, &ctx.actor, f.citizen, None).map_err(internal)?;
            if dead {
                let lost = die(w, &ctx.actor, &f)?;
                let msg = match &lost {
                    Some(c) => format!("The {cname} catches you as you turn, for {taken:.0}. You go down. Concussion: you have lost {c}. You wake at the Gate."),
                    None => format!("The {cname} catches you as you turn, for {taken:.0}. You go down, and wake at the Gate with everything you had."),
                };
                return Ok(Outcome::new(msg, json!({"died": true, "lost": lost})));
            }
            Ok(Outcome::new(format!("You back away from the {cname}; it catches you once for {taken:.0}. You have {:.0}/{:.0}.", hp(w, f.citizen), super::MAX_HP), json!({"died": false, "hp": hp(w, f.citizen)})))
        }),
    );

    world.registry.register(
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
