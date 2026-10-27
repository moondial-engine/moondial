//! Chunks, the active window, the Memory Palace, and PTO.

use super::{account_scope, internal, me, plain_num, Curves, CHUNK};
use crate::world::World;
use moondial_log::Actor;
use moondial_registry::{
    ActorKind, Args, CallContext, CommandError, CommandSpec, Outcome, ParamType,
};
use moondial_store::{EntityId, Field, StoreEvent};
use moondial_time::TimedValue;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Create a chunk in a window at `size`, fading on the active curve for its vividity.
pub fn earn_chunk(
    world: &mut World,
    actor: &str,
    window: EntityId,
    source: &str,
    size: f64,
    vividity: f64,
    curves: &Curves,
) -> Result<EntityId, CommandError> {
    let id = world.store.allocate_id();
    let me = Actor::Citizen(actor.to_string());
    world
        .record(
            me.clone(),
            StoreEvent::EntityCreated {
                id,
                kind: "chunk".into(),
                owner: Some(actor.into()),
                parent: Some(window),
                scopes: vec![],
            },
        )
        .map_err(internal)?;
    let now = world.now();
    let mut fields = BTreeMap::new();
    fields.insert(
        "value".into(),
        Field::Timed(TimedValue::new(size, now, curves.active(vividity))),
    );
    fields.insert("size".into(), Field::Plain(json!(size)));
    fields.insert("vividity".into(), Field::Plain(json!(vividity)));
    fields.insert("source".into(), Field::Plain(json!(source)));
    fields.insert("tier".into(), Field::Plain(json!("active")));
    world
        .record(
            me,
            StoreEvent::ComponentSet {
                id,
                component: CHUNK.into(),
                fields,
            },
        )
        .map_err(internal)?;
    Ok(id)
}

/// The sum of a window's chunk values now.
pub fn level(world: &World, window: EntityId) -> f64 {
    let now = world.now();
    world
        .store
        .children(window)
        .filter(|e| e.kind == "chunk")
        .filter_map(|e| world.store.read_field(e.id, CHUNK, "value", now).ok())
        .filter_map(|v| v.as_f64())
        .sum()
}

/// The query behind `chunks` and `palace`: every chunk in a container, every field.
pub fn chunks_query(container: EntityId) -> Value {
    json!({
        "root": {"entity": container.0},
        "select": {},
        "relations": {"contents": {"kinds": ["chunk"], "select": {CHUNK: "*"}}}
    })
}

/// Chunk rows rendered from an observation of a window or palace.
fn chunk_rows(
    world: &World,
    ctx: &CallContext,
    container: EntityId,
) -> Result<(Vec<Value>, Value), CommandError> {
    let obs = super::observe_as(world, ctx, chunks_query(container))?;
    let rows = obs.observation.root["relations"]["contents"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|n| {
            let c = &n["components"][CHUNK];
            json!({"id": n["id"], "source": c["source"], "tier": c["tier"], "size": c["size"], "vividity": c["vividity"], "value": c["value"]})
        })
        .collect();
    Ok((rows, serde_json::to_value(obs).unwrap_or(Value::Null)))
}

fn rows_text(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|r| {
            format!(
                "  chunk-{} {:<10} {:>9.2} of {:<6} vividity {:.2}",
                r["id"],
                r["source"].as_str().unwrap_or("?"),
                r["value"].as_f64().unwrap_or(0.0),
                r["size"],
                r["vividity"].as_f64().unwrap_or(0.0)
            )
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

pub fn register(world: &mut World, curves: Curves) {
    world.register(
        CommandSpec::new(
            "entropic.chunks",
            "Your active window: every chunk, its current value, and your level.",
        ),
        Box::new(|w: &mut World, ctx, _| {
            let f = me(w, ctx)?;
            let (rows, observation) = chunk_rows(w, ctx, f.window)?;
            let level: f64 = rows.iter().filter_map(|r| r["value"].as_f64()).sum();
            let attack = super::combat::attack_for_level(level);
            let text = if rows.is_empty() {
                format!("Your window is empty. Level 0, attack {attack:.0}.")
            } else {
                format!(
                    "Level {level:.2}, attack {attack:.0}\n{}",
                    rows_text(&rows).join("\n")
                )
            };
            Ok(Outcome::new(
                text,
                json!({"level": level, "attack": attack, "chunks": rows, "observation": observation}),
            ))
        }),
    );

    world.register(
        CommandSpec::new(
            "entropic.palace",
            "Your Memory Palace: shelved chunks, fading slowly, counting for nothing.",
        ),
        Box::new(|w: &mut World, ctx, _| {
            let f = me(w, ctx)?;
            let (rows, observation) = chunk_rows(w, ctx, f.palace)?;
            let text = if rows.is_empty() {
                "Your palace is empty.".to_string()
            } else {
                format!("Palace\n{}", rows_text(&rows).join("\n"))
            };
            Ok(Outcome::new(
                text,
                json!({"chunks": rows, "observation": observation}),
            ))
        }),
    );

    let c = curves.clone();
    world.register(
        CommandSpec::new("entropic.earn", "Admin: conjure a chunk by hand.")
            .param("source", ParamType::String, true, "what it is a memory of")
            .param(
                "size",
                ParamType::Number,
                true,
                "its XP value at the moment of earning",
            )
            .param("vividity", ParamType::Number, false, "0 to 1; default 0")
            .allowed(&[ActorKind::Admin]),
        Box::new(move |w: &mut World, ctx, args: &Args| {
            let f = me(w, ctx)?;
            let source = args.str("source").unwrap_or("unknown").to_string();
            let size = args.num("size").unwrap_or(0.0);
            let vividity = args.num("vividity").unwrap_or(0.0).clamp(0.0, 1.0);
            if size <= 0.0 || size > 100_000.0 {
                return Err(CommandError::precondition(
                    "size must be between 0 and 100000 XP.",
                ));
            }
            let id = earn_chunk(w, &ctx.actor, f.window, &source, size, vividity, &c)?;
            Ok(Outcome::new(
                format!(
                    "You remember the {source}: chunk-{} worth {size:.0} XP, already fading.",
                    id.0
                ),
                json!({"chunk": id.0, "size": size, "vividity": vividity}),
            ))
        }),
    );

    let c = curves.clone();
    world.register(
        CommandSpec::new("entropic.shelve", "Move a chunk from your window into your palace, where it fades slower and counts for nothing.")
            .param("chunk", ParamType::Entity, true, "a chunk in your window"),
        Box::new(move |w: &mut World, ctx, args: &Args| {
            let f = me(w, ctx)?;
            let id = resolve_chunk(w, ctx, args.str("chunk").unwrap_or(""), &[f.window])?;
            let vividity = plain_num(w, id, CHUNK, "vividity").unwrap_or(0.0);
            let actor = Actor::Citizen(ctx.actor.clone());
            w.record(actor.clone(), StoreEvent::FieldRecurved { id, component: CHUNK.into(), field: "value".into(), curve: c.palace(vividity) }).map_err(internal)?;
            w.record(actor.clone(), StoreEvent::FieldSet { id, component: CHUNK.into(), field: "tier".into(), value: json!("palace") }).map_err(internal)?;
            w.record(actor, StoreEvent::EntityMoved { id, parent: Some(f.palace) }).map_err(internal)?;
            let v = w.store.read_field(id, CHUNK, "value", w.now()).map_err(internal)?;
            Ok(Outcome::new(
                format!("You shelve chunk-{} at {:.2}. It will fade slowly now, and count for nothing until you reminisce.", id.0, v.as_f64().unwrap_or(0.0)),
                json!({"chunk": id.0, "value": v}),
            ))
        }),
    );

    let c = curves;
    world.register(
        CommandSpec::new(
            "entropic.reminisce",
            "Bring a chunk back from your palace into your window, at its current value.",
        )
        .param("chunk", ParamType::Entity, true, "a chunk in your palace"),
        Box::new(move |w: &mut World, ctx, args: &Args| {
            let f = me(w, ctx)?;
            let id = resolve_chunk(w, ctx, args.str("chunk").unwrap_or(""), &[f.palace])?;
            let vividity = plain_num(w, id, CHUNK, "vividity").unwrap_or(0.0);
            let actor = Actor::Citizen(ctx.actor.clone());
            w.record(
                actor.clone(),
                StoreEvent::FieldRecurved {
                    id,
                    component: CHUNK.into(),
                    field: "value".into(),
                    curve: c.active(vividity),
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

    world.register(
        CommandSpec::new("entropic.pto", "Pause your own clock. Nothing of yours fades or heals until you resume. (The real rules allow two 7-day windows a year; the slice does not enforce that yet.)"),
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

    world.register(
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
}
