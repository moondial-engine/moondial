//! The Entropic text slice: one room, chunks that decay, a window and a palace per
//! citizen, PTO, and a world freeze. Everything here is a *module* in the architecture's
//! sense: it registers schemas and commands and records events; the driver knows none of
//! these words. When the WASM module runtime exists this file becomes the first module.

use crate::world::World;
use moondial_log::Actor;
use moondial_registry::{
    ActorKind, Args, CallContext, CommandError, CommandSpec, Outcome, ParamType,
};
use moondial_store::{
    ClockId, ComponentSchema, EntityId, Field, FieldKind, StoreEvent, Viewer, Visibility,
    WORLD_CLOCK,
};
use moondial_time::{Curve, Duration, TimedValue};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Decay curves for the slice. Half-lives are short so decay is visible in a session;
/// `MOONDIAL_ACTIVE_HALF_LIFE_SECONDS` overrides the active one.
pub struct Curves {
    pub active: Curve,
    pub palace: Curve,
}

impl Curves {
    pub fn from_env() -> Self {
        let secs: i64 = std::env::var("MOONDIAL_ACTIVE_HALF_LIFE_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3600);
        Curves {
            active: Curve::Exponential {
                half_life: Duration::seconds(secs),
            },
            // Palace: strong early fade, long tail; takes about 5x the active half-life to halve.
            palace: Curve::PowerLaw {
                scale: Duration::seconds(secs * 5),
                exponent: 1.0,
            },
        }
    }
}

const CHUNK: &str = "entropic.chunk";
const ROOM: &str = "entropic.room";
const CITIZEN: &str = "entropic.citizen";

pub fn schemas() -> Vec<ComponentSchema> {
    vec![
        ComponentSchema::new(CHUNK, 1)
            .field("value", FieldKind::Timed, Visibility::Owner)
            .field("size", FieldKind::Plain, Visibility::Owner)
            .field("source", FieldKind::Plain, Visibility::Public)
            .field("tier", FieldKind::Plain, Visibility::Public),
        ComponentSchema::new(ROOM, 1)
            .field("name", FieldKind::Plain, Visibility::Public)
            .field("description", FieldKind::Plain, Visibility::Public),
        ComponentSchema::new(CITIZEN, 1)
            .field("name", FieldKind::Plain, Visibility::Public)
            .field("origin", FieldKind::Plain, Visibility::Public),
    ]
}

fn account_scope(actor: &str) -> ClockId {
    ClockId(format!("account:{actor}"))
}

/// Entities the slice needs: the one room, and per citizen a citizen entity (in the
/// room), a window and a palace (both under the citizen).
pub struct Found {
    pub citizen: EntityId,
    pub window: EntityId,
    pub palace: EntityId,
}

fn the_room(world: &World) -> Option<EntityId> {
    world
        .store
        .entities()
        .find(|e| e.kind == "room")
        .map(|e| e.id)
}

fn find_citizen(world: &World, actor: &str) -> Option<Found> {
    let citizen = world
        .store
        .entities()
        .find(|e| e.kind == "citizen" && e.owner.as_deref() == Some(actor))?;
    let mut window = None;
    let mut palace = None;
    for c in world.store.children(citizen.id) {
        match c.kind.as_str() {
            "window" => window = Some(c.id),
            "palace" => palace = Some(c.id),
            _ => {}
        }
    }
    Some(Found {
        citizen: citizen.id,
        window: window?,
        palace: palace?,
    })
}

/// Create the room on first boot.
pub fn ensure_room(world: &mut World) -> Result<EntityId, String> {
    if let Some(id) = the_room(world) {
        return Ok(id);
    }
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
    let mut fields = BTreeMap::new();
    fields.insert("name".into(), Field::Plain(json!("The Gate")));
    fields.insert("description".into(), Field::Plain(json!("A threshold of pale stone under a moon that is always up. Everything you earn here will fade; what you do with it is yours.")));
    world
        .record(
            Actor::Driver,
            StoreEvent::ComponentSet {
                id,
                component: ROOM.into(),
                fields,
            },
        )
        .map_err(|e| e.to_string())?;
    Ok(id)
}

/// Create a citizen with a window and a palace on first login. Idempotent.
pub fn ensure_citizen(world: &mut World, actor: &str, origin: &str) -> Result<Found, String> {
    if let Some(f) = find_citizen(world, actor) {
        return Ok(f);
    }
    let room = ensure_room(world)?;
    let me = Actor::Citizen(actor.to_string());
    let citizen = world.store.allocate_id();
    world
        .record(
            me.clone(),
            StoreEvent::EntityCreated {
                id: citizen,
                kind: "citizen".into(),
                owner: Some(actor.into()),
                parent: Some(room),
                scopes: vec![account_scope(actor)],
            },
        )
        .map_err(|e| e.to_string())?;
    let mut fields = BTreeMap::new();
    fields.insert("name".into(), Field::Plain(json!(actor)));
    fields.insert("origin".into(), Field::Plain(json!(origin)));
    world
        .record(
            me.clone(),
            StoreEvent::ComponentSet {
                id: citizen,
                component: CITIZEN.into(),
                fields,
            },
        )
        .map_err(|e| e.to_string())?;
    let window = world.store.allocate_id();
    world
        .record(
            me.clone(),
            StoreEvent::EntityCreated {
                id: window,
                kind: "window".into(),
                owner: Some(actor.into()),
                parent: Some(citizen),
                scopes: vec![],
            },
        )
        .map_err(|e| e.to_string())?;
    let palace = world.store.allocate_id();
    world
        .record(
            me,
            StoreEvent::EntityCreated {
                id: palace,
                kind: "palace".into(),
                owner: Some(actor.into()),
                parent: Some(citizen),
                scopes: vec![],
            },
        )
        .map_err(|e| e.to_string())?;
    Ok(Found {
        citizen,
        window,
        palace,
    })
}

fn me(world: &World, ctx: &CallContext) -> Result<Found, CommandError> {
    find_citizen(world, &ctx.actor).ok_or_else(|| {
        CommandError::precondition(format!("{} has not arrived; log in first.", ctx.actor))
    })
}

fn chunk_rows(world: &World, container: EntityId, viewer: &Viewer) -> Vec<Value> {
    let now = world.now();
    world
        .store
        .children(container)
        .filter(|e| e.kind == "chunk")
        .filter_map(|e| world.store.view(e.id, viewer, now))
        .map(|v| {
            let c = &v["components"][CHUNK];
            json!({
                "id": v["id"],
                "source": c["source"],
                "tier": c["tier"],
                "size": c["size"],
                "value": c["value"],
            })
        })
        .collect()
}

fn resolve_chunk(
    world: &World,
    ctx: &CallContext,
    arg: &str,
    within: &[EntityId],
) -> Result<EntityId, CommandError> {
    let id: u64 = arg.trim_start_matches("chunk-").parse().map_err(|_| {
        CommandError::precondition(format!("'{arg}' is not a chunk id; chunks shows yours."))
    })?;
    let id = EntityId(id);
    let e = world
        .store
        .entity(id)
        .ok_or_else(|| CommandError::precondition(format!("There is no chunk {}.", id.0)))?;
    if e.kind != "chunk" || e.owner.as_deref() != Some(ctx.actor.as_str()) {
        return Err(CommandError::precondition(format!(
            "Chunk {} is not yours.",
            id.0
        )));
    }
    if !within.contains(&e.parent.unwrap_or(EntityId(0))) {
        return Err(CommandError::precondition(format!(
            "Chunk {} is not where that command expects it.",
            id.0
        )));
    }
    Ok(id)
}

fn internal(e: impl std::fmt::Display) -> CommandError {
    CommandError::internal(e.to_string())
}

/// Register the slice's commands.
pub fn register(world: &mut World, curves: Curves) {
    let active = curves.active.clone();
    let palace_curve = curves.palace.clone();

    world.registry.register(
        CommandSpec::new("entropic.look", "Describe where you are and who is here."),
        Box::new(|w: &mut World, ctx, _| {
            let f = me(w, ctx)?;
            let room = w
                .store
                .entity(f.citizen)
                .and_then(|e| e.parent)
                .ok_or_else(|| internal("citizen has no room"))?;
            let now = w.now();
            let view = w
                .store
                .view(room, &Viewer::actor(&ctx.actor), now)
                .ok_or_else(|| internal("room vanished"))?;
            let here: Vec<String> = w
                .store
                .children(room)
                .filter(|e| e.kind == "citizen")
                .filter_map(|e| e.owner.clone())
                .collect();
            let name = view["components"][ROOM]["name"]
                .as_str()
                .unwrap_or("?")
                .to_string();
            let desc = view["components"][ROOM]["description"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let others: Vec<&String> = here.iter().filter(|n| **n != ctx.actor).collect();
            let who = if others.is_empty() {
                "You are alone.".to_string()
            } else {
                format!(
                    "Also here: {}.",
                    others
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            Ok(Outcome::new(
                format!("{name}. {desc} {who}"),
                json!({"room": name, "description": desc, "citizens": here}),
            ))
        }),
    );

    world.registry.register(
        CommandSpec::new(
            "entropic.chunks",
            "Your active window: every chunk, its current value, and your level.",
        ),
        Box::new(|w: &mut World, ctx, _| {
            let f = me(w, ctx)?;
            let rows = chunk_rows(w, f.window, &Viewer::actor(&ctx.actor));
            let level: f64 = rows.iter().filter_map(|r| r["value"].as_f64()).sum();
            let lines: Vec<String> = rows
                .iter()
                .map(|r| {
                    format!(
                        "  chunk-{} {:<12} {:>9.2} of {}",
                        r["id"],
                        r["source"].as_str().unwrap_or("?"),
                        r["value"].as_f64().unwrap_or(0.0),
                        r["size"]
                    )
                })
                .collect();
            let text = if lines.is_empty() {
                "Your window is empty.".to_string()
            } else {
                format!("Level {level:.2}\n{}", lines.join("\n"))
            };
            Ok(Outcome::new(text, json!({"level": level, "chunks": rows})))
        }),
    );

    world.registry.register(
        CommandSpec::new(
            "entropic.palace",
            "Your Memory Palace: shelved chunks, fading slowly.",
        ),
        Box::new(|w: &mut World, ctx, _| {
            let f = me(w, ctx)?;
            let rows = chunk_rows(w, f.palace, &Viewer::actor(&ctx.actor));
            let lines: Vec<String> = rows
                .iter()
                .map(|r| {
                    format!(
                        "  chunk-{} {:<12} {:>9.2} of {}",
                        r["id"],
                        r["source"].as_str().unwrap_or("?"),
                        r["value"].as_f64().unwrap_or(0.0),
                        r["size"]
                    )
                })
                .collect();
            let text = if lines.is_empty() {
                "Your palace is empty.".to_string()
            } else {
                format!("Palace\n{}", lines.join("\n"))
            };
            Ok(Outcome::new(text, json!({"chunks": rows})))
        }),
    );

    let active_for_earn = active.clone();
    world.registry.register(
        CommandSpec::new(
            "entropic.earn",
            "Dev: earn a chunk by hand. Combat will replace this.",
        )
        .param(
            "source",
            ParamType::String,
            true,
            "what you fought or found",
        )
        .param(
            "size",
            ParamType::Number,
            true,
            "its XP value at the moment of earning",
        ),
        Box::new(move |w: &mut World, ctx, args: &Args| {
            let f = me(w, ctx)?;
            let source = args.str("source").unwrap_or("unknown").to_string();
            let size = args.num("size").unwrap_or(0.0);
            if size <= 0.0 || size > 100_000.0 {
                return Err(CommandError::precondition(
                    "size must be between 0 and 100000 XP.",
                ));
            }
            let id = w.store.allocate_id();
            let actor = Actor::Citizen(ctx.actor.clone());
            w.record(
                actor.clone(),
                StoreEvent::EntityCreated {
                    id,
                    kind: "chunk".into(),
                    owner: Some(ctx.actor.clone()),
                    parent: Some(f.window),
                    scopes: vec![],
                },
            )
            .map_err(internal)?;
            let now = w.now();
            let mut fields = BTreeMap::new();
            fields.insert(
                "value".into(),
                Field::Timed(TimedValue::new(size, now, active_for_earn.clone())),
            );
            fields.insert("size".into(), Field::Plain(json!(size)));
            fields.insert("source".into(), Field::Plain(json!(source)));
            fields.insert("tier".into(), Field::Plain(json!("active")));
            w.record(
                actor,
                StoreEvent::ComponentSet {
                    id,
                    component: CHUNK.into(),
                    fields,
                },
            )
            .map_err(internal)?;
            Ok(Outcome::new(
                format!(
                    "You remember the {source}: chunk-{} worth {size:.0} XP, already fading.",
                    id.0
                ),
                json!({"chunk": id.0, "size": size}),
            ))
        }),
    );

    let pc = palace_curve.clone();
    world.registry.register(
        CommandSpec::new("entropic.shelve", "Move a chunk from your window into your palace, where it fades slower and counts for nothing.")
            .param("chunk", ParamType::Entity, true, "a chunk in your window"),
        Box::new(move |w: &mut World, ctx, args: &Args| {
            let f = me(w, ctx)?;
            let id = resolve_chunk(w, ctx, args.str("chunk").unwrap_or(""), &[f.window])?;
            let actor = Actor::Citizen(ctx.actor.clone());
            w.record(actor.clone(), StoreEvent::FieldRecurved { id, component: CHUNK.into(), field: "value".into(), curve: pc.clone() }).map_err(internal)?;
            w.record(actor.clone(), StoreEvent::FieldSet { id, component: CHUNK.into(), field: "tier".into(), value: json!("palace") }).map_err(internal)?;
            w.record(actor, StoreEvent::EntityMoved { id, parent: Some(f.palace) }).map_err(internal)?;
            let v = w.store.read_field(id, CHUNK, "value", w.now()).map_err(internal)?;
            Ok(Outcome::new(format!("You shelve chunk-{} at {:.2}. It will fade slowly now, and count for nothing until you reminisce.", id.0, v.as_f64().unwrap_or(0.0)), json!({"chunk": id.0, "value": v})))
        }),
    );

    let ac = active.clone();
    world.registry.register(
        CommandSpec::new(
            "entropic.reminisce",
            "Bring a chunk back from your palace into your window, at its current value.",
        )
        .param("chunk", ParamType::Entity, true, "a chunk in your palace"),
        Box::new(move |w: &mut World, ctx, args: &Args| {
            let f = me(w, ctx)?;
            let id = resolve_chunk(w, ctx, args.str("chunk").unwrap_or(""), &[f.palace])?;
            let actor = Actor::Citizen(ctx.actor.clone());
            w.record(
                actor.clone(),
                StoreEvent::FieldRecurved {
                    id,
                    component: CHUNK.into(),
                    field: "value".into(),
                    curve: ac.clone(),
                },
            )
            .map_err(internal)?;
            w.record(
                actor.clone(),
                StoreEvent::FieldSet {
                    id,
                    component: CHUNK.into(),
                    field: "tier".into(),
                    value: json!("active"),
                },
            )
            .map_err(internal)?;
            w.record(
                actor,
                StoreEvent::EntityMoved {
                    id,
                    parent: Some(f.window),
                },
            )
            .map_err(internal)?;
            let v = w
                .store
                .read_field(id, CHUNK, "value", w.now())
                .map_err(internal)?;
            Ok(Outcome::new(
                format!(
                    "You reminisce: chunk-{} returns at {:.2}, and counts again.",
                    id.0,
                    v.as_f64().unwrap_or(0.0)
                ),
                json!({"chunk": id.0, "value": v}),
            ))
        }),
    );

    world.registry.register(
        CommandSpec::new("entropic.pto", "Pause your own clock. Nothing of yours fades until you resume. (The real rules allow two 7-day windows a year; the slice does not enforce that yet.)"),
        Box::new(|w: &mut World, ctx, _| {
            me(w, ctx)?;
            let scope = account_scope(&ctx.actor);
            if w.store.clock(&scope).is_some_and(|c| c.is_paused_at(w.now())) {
                return Err(CommandError::precondition("You are already on PTO."));
            }
            w.record(Actor::Driver, StoreEvent::ClockPaused { clock: scope }).map_err(internal)?;
            Ok(Outcome::new("Your clock stops. Nothing of yours will fade until you resume.", json!({"paused": true})))
        }),
    );

    world.registry.register(
        CommandSpec::new("entropic.resume", "End your PTO. Your clock runs again."),
        Box::new(|w: &mut World, ctx, _| {
            me(w, ctx)?;
            let scope = account_scope(&ctx.actor);
            if !w
                .store
                .clock(&scope)
                .is_some_and(|c| c.is_paused_at(w.now()))
            {
                return Err(CommandError::precondition("You are not on PTO."));
            }
            w.record(Actor::Driver, StoreEvent::ClockResumed { clock: scope })
                .map_err(internal)?;
            Ok(Outcome::new(
                "Your clock runs again.",
                json!({"paused": false}),
            ))
        }),
    );

    world.registry.register(
        CommandSpec::new("entropic.freeze", "Admin: pause the whole world's clock.")
            .allowed(&[ActorKind::Admin]),
        Box::new(|w: &mut World, _, _| {
            let scope = ClockId::from(WORLD_CLOCK);
            if w.store
                .clock(&scope)
                .is_some_and(|c| c.is_paused_at(w.now()))
            {
                return Err(CommandError::precondition("The world is already frozen."));
            }
            w.record(Actor::Driver, StoreEvent::ClockPaused { clock: scope })
                .map_err(internal)?;
            Ok(Outcome::new("The world freezes.", json!({"frozen": true})))
        }),
    );

    world.registry.register(
        CommandSpec::new("entropic.thaw", "Admin: resume the world's clock.")
            .allowed(&[ActorKind::Admin]),
        Box::new(|w: &mut World, _, _| {
            let scope = ClockId::from(WORLD_CLOCK);
            if !w
                .store
                .clock(&scope)
                .is_some_and(|c| c.is_paused_at(w.now()))
            {
                return Err(CommandError::precondition("The world is not frozen."));
            }
            w.record(Actor::Driver, StoreEvent::ClockResumed { clock: scope })
                .map_err(internal)?;
            Ok(Outcome::new(
                "The world moves again.",
                json!({"frozen": false}),
            ))
        }),
    );

    world.registry.register(
        CommandSpec::new(
            "entropic.log",
            "The last events in the world, oldest first.",
        )
        .param(
            "count",
            ParamType::Integer,
            false,
            "how many (default 10, max 100)",
        ),
        Box::new(|w: &mut World, _, args: &Args| {
            let n = args.int("count").unwrap_or(10).clamp(1, 100) as usize;
            let events = w.recent_events(n);
            let lines: Vec<String> = events
                .iter()
                .map(|e| {
                    format!(
                        "  #{} t={} {} {}",
                        e.seq.0,
                        e.at.0,
                        e.kind,
                        serde_json::to_string(&e.actor).unwrap_or_default()
                    )
                })
                .collect();
            Ok(Outcome::new(
                if lines.is_empty() {
                    "Nothing has happened.".to_string()
                } else {
                    lines.join("\n")
                },
                json!({"events": events}),
            ))
        }),
    );

    world.registry.register(
        CommandSpec::new("entropic.time", "The world's clock, your clock, and how long the driver has been up."),
        Box::new(|w: &mut World, ctx, _| {
            let now = w.now();
            let world_paused = w.store.clock(&ClockId::from(WORLD_CLOCK)).is_some_and(|c| c.is_paused_at(now));
            let mine_paused = w.store.clock(&account_scope(&ctx.actor)).is_some_and(|c| c.is_paused_at(now));
            let up = now.since(w.started).0 / 1000;
            Ok(Outcome::new(
                format!("t={} world {} · your clock {} · driver up {}s", now.0, if world_paused { "frozen" } else { "running" }, if mine_paused { "paused (PTO)" } else { "running" }, up),
                json!({"now": now.0, "world_frozen": world_paused, "on_pto": mine_paused, "uptime_seconds": up}),
            ))
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::LogBacking;
    use moondial_log::WorldId;
    use moondial_store::{Registry, Store};
    use moondial_time::Timestamp;

    fn open(dir: &std::path::Path, at: Timestamp) -> World {
        let mut registry = Registry::new();
        for s in schemas() {
            registry.register(s);
        }
        let mut w = World::open(
            WorldId("test".into()),
            Store::new(registry),
            LogBacking::File(dir.to_path_buf()),
        )
        .unwrap();
        w.fixed_now = Some(at);
        ensure_room(&mut w).unwrap();
        register(
            &mut w,
            Curves {
                active: Curve::Exponential {
                    half_life: Duration::seconds(10),
                },
                palace: Curve::PowerLaw {
                    scale: Duration::seconds(50),
                    exponent: 1.0,
                },
            },
        );
        w
    }

    fn call(
        w: &mut World,
        actor: &str,
        kind: ActorKind,
        verb: &str,
        args: Value,
    ) -> Result<Outcome, CommandError> {
        w.call(
            &CallContext {
                actor: actor.into(),
                kind,
            },
            verb,
            &args,
        )
    }

    fn level(w: &mut World, actor: &str) -> f64 {
        call(w, actor, ActorKind::Citizen, "chunks", json!([]))
            .unwrap()
            .result["level"]
            .as_f64()
            .unwrap()
    }

    #[test]
    fn a_session_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let t0 = Timestamp(1_000_000);
        let mut w = open(dir.path(), t0);
        ensure_citizen(&mut w, "dave", "test").unwrap();

        let earned = call(
            &mut w,
            "dave",
            ActorKind::Citizen,
            "earn",
            json!(["hydra", 100]),
        )
        .unwrap();
        let chunk = format!("chunk-{}", earned.result["chunk"]);
        assert!((level(&mut w, "dave") - 100.0).abs() < 1e-9);

        // One half-life later: 50. Shelve it; the palace reads the same value at that instant.
        w.fixed_now = Some(t0.plus(Duration::seconds(10)));
        assert!((level(&mut w, "dave") - 50.0).abs() < 1e-6);
        let shelved = call(
            &mut w,
            "dave",
            ActorKind::Citizen,
            "shelve",
            json!([chunk.clone()]),
        )
        .unwrap();
        assert!((shelved.result["value"].as_f64().unwrap() - 50.0).abs() < 1e-6);
        assert_eq!(
            level(&mut w, "dave"),
            0.0,
            "a shelved chunk counts for nothing"
        );

        // Errors are legible and do not touch the log.
        let head = w.log.head(&w.id).unwrap().unwrap();
        assert!(matches!(
            call(
                &mut w,
                "dave",
                ActorKind::Citizen,
                "shelve",
                json!(["chunk-99"])
            ),
            Err(CommandError::Precondition { .. })
        ));
        assert!(matches!(
            call(&mut w, "dave", ActorKind::Citizen, "freeze", json!([])),
            Err(CommandError::NotAllowed { .. })
        ));
        assert!(
            matches!(
                call(&mut w, "tyler", ActorKind::Citizen, "chunks", json!([])),
                Err(CommandError::Precondition { .. })
            ),
            "tyler has not arrived"
        );
        assert_eq!(w.log.head(&w.id).unwrap().unwrap(), head);

        // PTO freezes dave's palace; the world freeze is admin-only and stacks with it.
        call(&mut w, "dave", ActorKind::Citizen, "pto", json!([])).unwrap();
        call(&mut w, "dave", ActorKind::Admin, "freeze", json!([])).unwrap();
        let events = w.log.head(&w.id).unwrap().unwrap();
        drop(w);

        // Reopen fifty seconds later: same events, same values, clocks still paused.
        let mut w = open(dir.path(), t0.plus(Duration::seconds(60)));
        assert_eq!(
            w.log.head(&w.id).unwrap().unwrap(),
            events,
            "nothing recorded by reopening"
        );
        let palace = call(&mut w, "dave", ActorKind::Citizen, "palace", json!([])).unwrap();
        assert!(
            (palace.result["chunks"][0]["value"].as_f64().unwrap() - 50.0).abs() < 1e-6,
            "paused clocks hold the value across a restart"
        );
        let time = call(&mut w, "dave", ActorKind::Citizen, "time", json!([])).unwrap();
        assert_eq!(time.result["world_frozen"], json!(true));
        assert_eq!(time.result["on_pto"], json!(true));

        // Thaw the world; dave is still on PTO so nothing of his moves. Resume: it fades.
        call(&mut w, "dave", ActorKind::Admin, "thaw", json!([])).unwrap();
        call(&mut w, "dave", ActorKind::Citizen, "resume", json!([])).unwrap();
        w.fixed_now = Some(t0.plus(Duration::seconds(60 + 50)));
        let back = call(
            &mut w,
            "dave",
            ActorKind::Citizen,
            "reminisce",
            json!([chunk]),
        )
        .unwrap();
        assert!(
            (back.result["value"].as_f64().unwrap() - 25.0).abs() < 1e-6,
            "power law halves at its scale"
        );
        assert!(
            (level(&mut w, "dave") - 25.0).abs() < 1e-6,
            "and counts again"
        );
    }

    #[test]
    fn strangers_see_a_chunk_exists_but_not_what_it_is_worth() {
        let mut w = open(tempfile::tempdir().unwrap().path(), Timestamp(0));
        ensure_citizen(&mut w, "dave", "test").unwrap();
        let id = call(
            &mut w,
            "dave",
            ActorKind::Citizen,
            "earn",
            json!(["lich", 10]),
        )
        .unwrap()
        .result["chunk"]
            .as_u64()
            .unwrap();
        let now = w.now();
        let mine = w
            .store
            .view(EntityId(id), &Viewer::actor("dave"), now)
            .unwrap();
        let theirs = w
            .store
            .view(EntityId(id), &Viewer::actor("tyler"), now)
            .unwrap();
        assert_eq!(mine["components"][CHUNK]["value"], json!(10.0));
        assert_eq!(theirs["components"][CHUNK]["source"], json!("lich"));
        assert!(theirs["components"][CHUNK].get("value").is_none());
        assert!(theirs["components"][CHUNK].get("size").is_none());
    }
}
