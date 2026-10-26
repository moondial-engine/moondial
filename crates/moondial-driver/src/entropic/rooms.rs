//! The map: four rooms off the Gate, exits between them, and what spawns where.
//! Rooms are entities with `entropic.room`, `entropic.exits` and, where something lives,
//! `entropic.spawn`. Travel moves the citizen entity from one room to another.

use super::{internal, me, plain_str, EXITS, ROOM, SPAWN};
use crate::world::World;
use moondial_log::Actor;
use moondial_registry::{CommandSpec, Outcome, ParamType};
use moondial_store::{EntityId, Field, StoreEvent, Viewer};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// A room as the slice defines it. The log is the source of truth once built; this table
/// only seeds it and fills in what an older log lacks.
pub struct RoomDef {
    pub key: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// `(exit name, room key)`.
    pub exits: &'static [(&'static str, &'static str)],
    /// `(creature key, how many at once, seconds between respawns)`.
    pub spawn: Option<(&'static str, u32, i64)>,
}

pub const GATE: &str = "gate";

pub const ROOMS: &[RoomDef] = &[
    RoomDef {
        key: GATE,
        name: "The Gate",
        description: "A threshold of pale stone under a moon that is always up. Everything you earn here will fade; what you do with it is yours. Nothing hunts here.",
        exits: &[("east", "fogbank")],
        spawn: None,
    },
    RoomDef {
        key: "fogbank",
        name: "The Fogbank",
        description: "Fog rolls off the stone and swallows the path a few steps ahead. Things move in it, slowly, and mostly away from you.",
        exits: &[("west", GATE), ("north", "shallows")],
        spawn: Some(("wisp", 2, 60)),
    },
    RoomDef {
        key: "shallows",
        name: "The Shallows",
        description: "Black water over pale sand, ankle-deep and perfectly still, until something stands up out of it.",
        exits: &[("south", "fogbank"), ("east", "ossuary")],
        spawn: Some(("hollow", 2, 120)),
    },
    RoomDef {
        key: "ossuary",
        name: "The Ossuary",
        description: "Niches cut into the rock, each one empty, each one swept clean. Something keeps them that way.",
        exits: &[("west", "shallows")],
        spawn: Some(("revenant", 1, 300)),
    },
];

/// The room entity with this key.
pub fn room_by_key(world: &World, key: &str) -> Option<EntityId> {
    world
        .store
        .entities()
        .find(|e| e.kind == "room" && plain_str(world, e.id, ROOM, "key") == Some(key))
        .map(|e| e.id)
}

/// The Gate.
pub fn the_gate(world: &World) -> Option<EntityId> {
    room_by_key(world, GATE)
}

/// Build every room the log does not have, and the exits once all rooms exist. A room
/// from an older log that has a name but no key is claimed by name.
pub fn ensure_rooms(world: &mut World) -> Result<(), String> {
    let mut ids: BTreeMap<&str, EntityId> = BTreeMap::new();
    for def in ROOMS {
        let existing = room_by_key(world, def.key).or_else(|| {
            world
                .store
                .entities()
                .find(|e| {
                    e.kind == "room"
                        && plain_str(world, e.id, ROOM, "key").is_none()
                        && plain_str(world, e.id, ROOM, "name") == Some(def.name)
                })
                .map(|e| e.id)
        });
        let id = match existing {
            Some(id) if plain_str(world, id, ROOM, "key") == Some(def.key) => id,
            Some(id) => {
                world
                    .record(
                        Actor::Driver,
                        StoreEvent::ComponentSet {
                            id,
                            component: ROOM.into(),
                            fields: room_fields(def),
                        },
                    )
                    .map_err(|e| e.to_string())?;
                id
            }
            None => {
                let id = world.store.allocate_id();
                world
                    .record(
                        Actor::Driver,
                        StoreEvent::EntityCreated {
                            id,
                            kind: "room".into(),
                            owner: None,
                            parent: None,
                            scopes: vec![],
                        },
                    )
                    .map_err(|e| e.to_string())?;
                world
                    .record(
                        Actor::Driver,
                        StoreEvent::ComponentSet {
                            id,
                            component: ROOM.into(),
                            fields: room_fields(def),
                        },
                    )
                    .map_err(|e| e.to_string())?;
                id
            }
        };
        ids.insert(def.key, id);
    }
    for def in ROOMS {
        let id = ids[def.key];
        let has_exits = world
            .store
            .entity(id)
            .is_some_and(|e| e.components.contains_key(EXITS));
        if !has_exits {
            let exits: serde_json::Map<String, Value> = def
                .exits
                .iter()
                .map(|(name, key)| (name.to_string(), json!(ids[key].0)))
                .collect();
            let mut fields = BTreeMap::new();
            fields.insert("exits".into(), Field::Plain(Value::Object(exits)));
            world
                .record(
                    Actor::Driver,
                    StoreEvent::ComponentSet {
                        id,
                        component: EXITS.into(),
                        fields,
                    },
                )
                .map_err(|e| e.to_string())?;
        }
        let has_spawn = world
            .store
            .entity(id)
            .is_some_and(|e| e.components.contains_key(SPAWN));
        if let (Some((creature, count, respawn)), false) = (def.spawn, has_spawn) {
            let mut fields = BTreeMap::new();
            fields.insert("creature".into(), Field::Plain(json!(creature)));
            fields.insert("count".into(), Field::Plain(json!(count)));
            fields.insert("respawn_seconds".into(), Field::Plain(json!(respawn)));
            fields.insert("next_at".into(), Field::Plain(json!(0)));
            world
                .record(
                    Actor::Driver,
                    StoreEvent::ComponentSet {
                        id,
                        component: SPAWN.into(),
                        fields,
                    },
                )
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn room_fields(def: &RoomDef) -> BTreeMap<String, Field> {
    let mut fields = BTreeMap::new();
    fields.insert("key".into(), Field::Plain(json!(def.key)));
    fields.insert("name".into(), Field::Plain(json!(def.name)));
    fields.insert("description".into(), Field::Plain(json!(def.description)));
    fields
}

/// The exits of a room: name to room id.
pub fn exits_of(world: &World, room: EntityId) -> BTreeMap<String, EntityId> {
    let Some(Field::Plain(Value::Object(m))) = world
        .store
        .entity(room)
        .and_then(|e| e.components.get(EXITS))
        .and_then(|c| c.fields.get("exits"))
    else {
        return BTreeMap::new();
    };
    m.iter()
        .filter_map(|(k, v)| v.as_u64().map(|id| (k.clone(), EntityId(id))))
        .collect()
}

/// The room a citizen stands in.
pub fn room_of(world: &World, citizen: EntityId) -> Option<EntityId> {
    world.store.entity(citizen).and_then(|e| e.parent)
}

/// Describe a room as `actor` sees it: name, description, exits, creatures, others here.
pub fn describe(
    world: &mut World,
    actor: &str,
    room: EntityId,
) -> Result<Outcome, moondial_registry::CommandError> {
    super::combat::maybe_spawn(world, room).map_err(internal)?;
    let now = world.now();
    let view = world
        .store
        .view(room, &Viewer::actor(actor), now)
        .ok_or_else(|| internal("room vanished"))?;
    let name = view["components"][ROOM]["name"]
        .as_str()
        .unwrap_or("?")
        .to_string();
    let desc = view["components"][ROOM]["description"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let exits: Vec<String> = exits_of(world, room).keys().cloned().collect();
    let others: Vec<String> = world
        .store
        .children(room)
        .filter(|e| e.kind == "citizen")
        .filter_map(|e| e.owner.clone())
        .filter(|n| n != actor)
        .collect();
    let creatures: Vec<Value> = super::combat::creatures_in(world, room)
        .into_iter()
        .map(|(id, name, hp, max_hp)| json!({"id": id.0, "name": name, "hp": hp, "max_hp": max_hp}))
        .collect();
    let mut text = format!("{name}. {desc}");
    if !exits.is_empty() {
        text.push_str(&format!(" Exits: {}.", exits.join(", ")));
    }
    if !creatures.is_empty() {
        let list: Vec<String> = creatures
            .iter()
            .map(|c| {
                format!(
                    "{} ({}/{})",
                    c["name"].as_str().unwrap_or("?"),
                    c["hp"],
                    c["max_hp"]
                )
            })
            .collect();
        text.push_str(&format!(" Here: {}.", list.join(", ")));
    }
    text.push_str(&if others.is_empty() {
        " You are alone.".to_string()
    } else {
        format!(" Also here: {}.", others.join(", "))
    });
    Ok(Outcome::new(
        text,
        json!({"room": name, "description": desc, "exits": exits, "creatures": creatures, "citizens": others}),
    ))
}

pub fn register(world: &mut World) {
    world.registry.register(
        CommandSpec::new(
            "entropic.look",
            "Describe where you are: exits, what is here, who is here.",
        ),
        Box::new(|w: &mut World, ctx, _| {
            let f = me(w, ctx)?;
            let room = room_of(w, f.citizen).ok_or_else(|| internal("citizen has no room"))?;
            describe(w, &ctx.actor, room)
        }),
    );

    world.registry.register(
        CommandSpec::new(
            "entropic.travel",
            "Take an exit. You cannot travel while something is fighting you; retreat first.",
        )
        .param("exit", ParamType::String, true, "an exit name from look"),
        Box::new(|w: &mut World, ctx, args| {
            let f = me(w, ctx)?;
            if super::combat::engaged_with(w, f.citizen).is_some() {
                return Err(moondial_registry::CommandError::precondition(
                    "You are in a fight. Retreat first, or finish it.",
                ));
            }
            let room = room_of(w, f.citizen).ok_or_else(|| internal("citizen has no room"))?;
            let exit = args.str("exit").unwrap_or("").to_ascii_lowercase();
            let exits = exits_of(w, room);
            let Some(&to) = exits.get(&exit) else {
                let names: Vec<&String> = exits.keys().collect();
                return Err(moondial_registry::CommandError::precondition(format!(
                    "There is no exit '{exit}'. Exits: {}.",
                    names
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            };
            w.record(
                Actor::Citizen(ctx.actor.clone()),
                StoreEvent::EntityMoved {
                    id: f.citizen,
                    parent: Some(to),
                },
            )
            .map_err(internal)?;
            let mut out = describe(w, &ctx.actor, to)?;
            out.summary = format!("You go {exit}. {}", out.summary);
            Ok(out)
        }),
    );
}
