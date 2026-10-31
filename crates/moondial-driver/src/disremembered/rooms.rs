//! The map: fourteen rooms off the Threshold, exits between them, and what spawns where.
//! No room, creature or exit may be named with a CLEAR reserved word or a glossary
//! alias; a test under `disremembered::tests` holds the map to that.
//! Rooms are entities with `disremembered.room`, `disremembered.exits` and, where something lives,
//! `disremembered.spawn`. Travel moves the citizen entity from one room to another.

use super::{internal, me, plain_str, EXITS, ROOM, SPAWN};
use crate::world::World;
use moondial_log::Actor;
use moondial_registry::{CommandSpec, Outcome, ParamType};
use moondial_store::{EntityId, Field, StoreEvent};
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
    /// Keys this room had in older logs; a room found under one is renamed in place.
    pub former: &'static [&'static str],
    /// How a renderer tints the room.
    pub tint: &'static str,
}

pub const ARRIVAL: &str = "threshold";

pub const ROOMS: &[RoomDef] = &[
    RoomDef {
        key: ARRIVAL,
        name: "The Threshold",
        description: "A threshold of pale stone under a moon that is always up. Everything you earn here will fade; what you do with it is yours. Nothing hunts here.",
        exits: &[("east", "fogbank"), ("north", "lantern-row")],
        spawn: None,
        former: &["gate"],
        tint: "#d9d4c7",
    },
    RoomDef {
        key: "fogbank",
        name: "The Fogbank",
        description: "Fog rolls off the stone and swallows the path a few steps ahead. Things move in it, slowly, and mostly away from you.",
        exits: &[("west", ARRIVAL), ("north", "shallows"), ("south", "midden")],
        spawn: Some(("wisp", 2, 60)),
        former: &[],
        tint: "#8a97a8",
    },
    RoomDef {
        key: "shallows",
        name: "The Shallows",
        description: "Black water over pale sand, ankle-deep and perfectly still, until something stands up out of it.",
        exits: &[
            ("south", "fogbank"),
            ("east", "ossuary"),
            ("north", "orchard"),
            ("dive", "undertow"),
        ],
        spawn: Some(("hollow", 2, 120)),
        former: &[],
        tint: "#3f5a5a",
    },
    RoomDef {
        key: "ossuary",
        name: "The Ossuary",
        description: "Niches cut into the rock, each one empty, each one swept clean. Something keeps them that way.",
        exits: &[("west", "shallows"), ("east", "gallery")],
        spawn: Some(("revenant", 1, 300)),
        former: &[],
        tint: "#6b5a4a",
    },
    // North of the Threshold: the quiet side, where things are kept rather than hunted.
    RoomDef {
        key: "lantern-row",
        name: "Lantern Row",
        description: "A row of lanterns on iron hooks, each one lighting as you pass and going dark behind you. They remember being lit. They do not remember by whom. Nothing hunts here.",
        exits: &[("south", ARRIVAL), ("north", "archive"), ("down", "stairwell")],
        spawn: None,
        former: &[],
        tint: "#c9a86a",
    },
    RoomDef {
        key: "archive",
        name: "The Unfiled Archive",
        description: "Shelves to the ceiling and ledgers on every one, but the ink lifts off the page the moment you read it and drifts for the rafters. Some of it has learned to flutter.",
        exits: &[("south", "lantern-row")],
        spawn: Some(("inkling", 3, 45)),
        former: &[],
        tint: "#4a4e6b",
    },
    RoomDef {
        key: "stairwell",
        name: "The Downward Stair",
        description: "A spiral stair cut into the rock, going down further than the hill is tall. Every landing has a door, and every door is bricked up from the other side.",
        exits: &[("up", "lantern-row"), ("down", "cistern")],
        spawn: None,
        former: &[],
        tint: "#5a5a5a",
    },
    RoomDef {
        key: "cistern",
        name: "The Cistern",
        description: "A drowned hall, water to your waist, the ceiling lost in the dark. Every sound comes back twice: once as you made it, and once as somebody else did.",
        exits: &[("up", "stairwell"), ("east", "undertow")],
        spawn: Some(("drowned", 2, 150)),
        former: &[],
        tint: "#2f4858",
    },
    RoomDef {
        key: "undertow",
        name: "The Undertow",
        description: "A current under the Shallows, pulling at your knees toward somewhere it will not name. Light comes down green from the surface. Things that stood up in the water above lie down here.",
        exits: &[("west", "cistern"), ("surface", "shallows")],
        spawn: Some(("hollow", 1, 180)),
        former: &[],
        tint: "#2e6b5e",
    },
    // North of the Shallows: a place that almost ripens.
    RoomDef {
        key: "orchard",
        name: "The Orchard of Unripe Hours",
        description: "Rows of low trees heavy with fruit that is always about to ripen and never does. The air is sweet with it. Moths the size of your hand work the branches, and are not interested in you, mostly.",
        exits: &[("south", "shallows"), ("up", "clocktower")],
        spawn: Some(("mothwing", 3, 40)),
        former: &[],
        tint: "#7d9a5a",
    },
    RoomDef {
        key: "clocktower",
        name: "The Stopped Clocktower",
        description: "Four clock faces, one to each wall, each showing a different hour, and all of them wrong. The works fill the middle of the room, and something in the works is still wound.",
        exits: &[("down", "orchard")],
        spawn: Some(("hourhand", 1, 240)),
        former: &[],
        tint: "#8a6f3a",
    },
    // South of the Fogbank: where everything forgotten ends up.
    RoomDef {
        key: "midden",
        name: "The Midden",
        description: "A heap of everything anyone here has ever forgotten: names, keys, the second verse, the way home. It shifts when you are not looking at it. Something lives in it and sorts.",
        exits: &[("north", "fogbank")],
        spawn: Some(("ragpicker", 1, 90)),
        former: &[],
        tint: "#6e6250",
    },
    // East of the Ossuary: the deep end.
    RoomDef {
        key: "gallery",
        name: "The Gallery of Faces Turned Away",
        description: "A long hall of portraits, every one hung facing the wall. The frames are dusted. The hooks are new. One of the sitters is not entirely in its frame.",
        exits: &[("west", "ossuary"), ("down", "oubliette")],
        spawn: Some(("sitter", 1, 200)),
        former: &[],
        tint: "#5e3f4f",
    },
    RoomDef {
        key: "oubliette",
        name: "The Oubliette",
        description: "A pit with no stair, only the rope you came down. The walls are scored with tally marks in a hand that stopped counting. The Keeper has forgotten what it keeps, but not that it keeps it.",
        exits: &[("up", "gallery")],
        spawn: Some(("keeper", 1, 600)),
        former: &[],
        tint: "#2b2b33",
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

/// The arrival room.
pub fn arrival(world: &World) -> Option<EntityId> {
    room_by_key(world, ARRIVAL)
}

/// Build every room the log does not have, and the exits once all rooms exist. A room
/// from an older log that has a name but no key is claimed by name.
pub fn ensure_rooms(world: &mut World) -> Result<(), String> {
    let mut ids: BTreeMap<&str, EntityId> = BTreeMap::new();
    for def in ROOMS {
        let existing = room_by_key(world, def.key)
            .or_else(|| def.former.iter().find_map(|k| room_by_key(world, k)))
            .or_else(|| {
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
        // The seed exits this room lacks, added to whatever it has (a dug exit stays).
        // Exits that are there already, pointing anywhere, are left alone.
        let mut exits: serde_json::Map<String, Value> = match world
            .store
            .entity(id)
            .and_then(|e| e.components.get(EXITS))
            .and_then(|c| c.fields.get("exits"))
        {
            Some(Field::Plain(Value::Object(m))) => m.clone(),
            _ => serde_json::Map::new(),
        };
        let mut missing = false;
        for (name, key) in def.exits {
            match exits.get(*name).and_then(Value::as_u64) {
                None => {
                    exits.insert(name.to_string(), json!(ids[key].0));
                    missing = true;
                }
                Some(to) if to != ids[key].0 => world.notice(format!(
                    "Room **{}** has exit `{name}` leading to entity {to}, not seed room `{key}`; \
                     the seed exit was not added.",
                    def.key
                )),
                Some(_) => {}
            }
        }
        if missing {
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
        let has_sprite = world
            .store
            .entity(id)
            .is_some_and(|e| e.components.contains_key(super::SPRITE))
            && !super::sprite_is_old(world, id);
        if !has_sprite {
            world
                .record(
                    Actor::Driver,
                    StoreEvent::ComponentSet {
                        id,
                        component: super::SPRITE.into(),
                        fields: super::sprite_fields("#", def.tint, def.key),
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

/// The query behind `look`: the room's name, description and exits, and what is here.
pub fn look_query(room: EntityId) -> Value {
    json!({
        "root": {"entity": room.0},
        "select": {ROOM: ["name", "description", "memory", "dug_with", "kept"], EXITS: "*"},
        "relations": {
            "contents": {
                "kinds": ["citizen", "creature"],
                "select": {super::CITIZEN: ["name"], super::CREATURE: ["name", "hp", "max_hp"], super::mind::MIND: ["name", "tier"]}
            }
        }
    })
}

/// Describe a room as `actor` sees it. Runs the observation query and renders prose
/// from the structure, which is the text client's job: the driver sends components,
/// the client makes sentences.
pub fn describe(
    world: &mut World,
    actor: &str,
    ctx: &moondial_registry::CallContext,
    room: EntityId,
) -> Result<Outcome, moondial_registry::CommandError> {
    let obs = super::observe_as(world, ctx, look_query(room))?;
    let root = &obs.observation.root;
    let name = root["components"][ROOM]["name"]
        .as_str()
        .unwrap_or("?")
        .to_string();
    // A dug room's description is as remembered: whole, dimmed, or faded.
    let memory = root["components"][ROOM]["memory"].as_f64();
    let desc = super::building::remembered_description(
        root["components"][ROOM]["description"]
            .as_str()
            .unwrap_or(""),
        memory,
    );
    let desc = if desc.ends_with(['.', '!', '?', '"', '”']) || desc.is_empty() {
        desc
    } else {
        format!("{desc}.")
    };
    let dug_by = root["owner"].as_str().map(str::to_string);
    let exits: Vec<String> = root["components"][EXITS]["exits"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let here = root["relations"]["contents"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let (others, remembered) = super::presence::split(world, room, actor);
    let creatures: Vec<Value> = here
        .iter()
        .filter(|n| n["kind"] == "creature")
        .map(|n| {
            let c = &n["components"][super::CREATURE];
            let id = EntityId(n["id"].as_u64().unwrap_or(0));
            let mut v =
                json!({"id": n["id"], "name": c["name"], "hp": c["hp"], "max_hp": c["max_hp"]});
            if n["components"][super::mind::MIND].is_object() {
                v["mind"] = super::mind::summary(world, id);
            }
            v
        })
        .collect();
    let regards: Vec<String> = creatures
        .iter()
        .filter_map(|c| c["id"].as_u64())
        .filter_map(|id| super::mind::regard(world, EntityId(id), actor))
        .collect();
    let mut text = format!("{name}. {desc}");
    if let Some(who) = &dug_by {
        let with = root["components"][ROOM]["dug_with"]
            .as_str()
            .map(|s| format!(" with the memory of {s}"))
            .unwrap_or_default();
        text.push_str(&format!(" Dug by {who}{with}."));
        if let Some(kept) = root["components"][ROOM]["kept"].as_array() {
            let keepers: Vec<String> = kept
                .iter()
                .filter_map(|k| {
                    Some(format!(
                        "{} ({})",
                        k["by"].as_str()?,
                        k["with"].as_str().unwrap_or("something")
                    ))
                })
                .collect();
            if !keepers.is_empty() {
                text.push_str(&format!(" Kept in memory by {}.", keepers.join(", ")));
            }
        }
    }
    if !exits.is_empty() {
        text.push_str(&format!(" Exits: {}.", exits.join(", ")));
    }
    if !creatures.is_empty() {
        let list: Vec<String> = creatures
            .iter()
            .map(|c| {
                let kind = c["name"].as_str().unwrap_or("?");
                let twins = creatures
                    .iter()
                    .filter(|o| o["name"] == c["name"] && o["mind"]["name"].is_null())
                    .count();
                let who = match c["mind"]["name"].as_str() {
                    Some(n) => format!("{n} the {kind}"),
                    // Two of a kind: written with their ids, so either can be named.
                    None if twins > 1 => format!("{kind}-{}", c["id"]),
                    None => kind.to_string(),
                };
                let holds = match c["mind"]["level"].as_f64() {
                    Some(l) if l >= 0.5 => format!(", holding {l:.0}"),
                    _ => String::new(),
                };
                format!("{who} ({}/{}{holds})", c["hp"], c["max_hp"])
            })
            .collect();
        text.push_str(&format!(" Here: {}.", list.join(", ")));
        for r in &regards {
            text.push(' ');
            text.push_str(r);
        }
    }
    text.push_str(&if others.is_empty() {
        " You are alone.".to_string()
    } else {
        format!(" Also here: {}.", others.join(", "))
    });
    if !remembered.is_empty() {
        let names: Vec<&str> = remembered.iter().map(|(n, _)| n.as_str()).collect();
        text.push_str(&format!(" Remembered here: {}.", names.join(", ")));
    }
    let remembered: Vec<Value> = remembered
        .iter()
        .map(|(n, t)| json!({"name": n, "trace": t}))
        .collect();
    Ok(Outcome::new(
        text,
        json!({"room": name, "description": desc, "memory": memory, "dug_by": dug_by, "exits": exits, "creatures": creatures, "citizens": others, "remembered": remembered, "observation": obs}),
    ))
}

pub fn register(world: &mut World) {
    world.register(
        CommandSpec::new(
            "disremembered.look",
            "Describe where you are: exits, what is here, who is here.",
        ),
        Box::new(|w: &mut World, ctx, _| {
            let f = me(w, ctx)?;
            let room = room_of(w, f.citizen).ok_or_else(|| internal("citizen has no room"))?;
            describe(w, &ctx.actor, ctx, room)
        }),
    );

    world.register(
        CommandSpec::new(
            "disremembered.travel",
            "Take an exit. You cannot travel while something is fighting you; retreat first.",
        )
        .param("exit", ParamType::String, true, "an exit name from look"),
        Box::new(|w: &mut World, ctx, args| {
            let f = me(w, ctx)?;
            if let Some(foe) = super::combat::engaged_with(w, f.citizen) {
                return Err(moondial_registry::CommandError::precondition(format!(
                    "You are fighting the {}. Retreat first, or finish it.",
                    super::mind::display(w, foe)
                )));
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
            super::combat::on_arrival(w, &ctx.actor, f.citizen, to).map_err(internal)?;
            super::building::visited(w, to).map_err(internal)?;
            super::talk::greet(w, &ctx.actor, to);
            let mut out = describe(w, &ctx.actor, ctx, to)?;
            out.summary = format!("You go {exit}. {}", out.summary);
            Ok(out)
        }),
    );
}
