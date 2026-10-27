//! World-level commands: the freeze, the log, the clocks.

use super::{account_scope, internal};
use crate::world::World;
use moondial_log::Actor;
use moondial_registry::{ActorKind, Args, CommandError, CommandSpec, Outcome, ParamType};
use moondial_store::{ClockId, StoreEvent, WORLD_CLOCK};
use serde_json::json;

pub fn register(world: &mut World) {
    world.register(
        CommandSpec::new(
            "disremembered.observe",
            "Ask the world a question as a JSON query: a root (self, here, window, palace, {\"entity\": n}), a select, relations (contents, in, exits), where, limit.",
        )
        .param("query", ParamType::String, true, "the query as JSON"),
        Box::new(|w: &mut World, ctx, args: &Args| {
            let raw = args.str("query").unwrap_or("");
            let query: serde_json::Value = serde_json::from_str(raw)
                .map_err(|e| CommandError::precondition(format!("The query is not JSON: {e}")))?;
            let req = super::parse_query(query)?;
            let obs = super::observe_request(w, ctx, &req)?;
            let text = serde_json::to_string_pretty(&obs).unwrap_or_default();
            Ok(Outcome::new(text, serde_json::to_value(obs).unwrap_or_default()))
        }),
    );

    world.register(
        CommandSpec::new(
            "disremembered.freeze",
            "Admin: pause the whole world's clock.",
        )
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

    world.register(
        CommandSpec::new("disremembered.thaw", "Admin: resume the world's clock.")
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

    world.register(
        CommandSpec::new(
            "disremembered.log",
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

    world.register(
        CommandSpec::new("disremembered.time", "The world's clock, your clock, and how long the driver has been up."),
        Box::new(|w: &mut World, ctx, _| {
            let now = w.now();
            let world_paused = w.store.clock(&ClockId::from(WORLD_CLOCK)).is_some_and(|c| c.is_paused_at(now));
            let mine_paused = w.store.clock(&account_scope(&ctx.actor)).is_some_and(|c| c.is_paused_at(now));
            let up = now.since(w.started).0 / 1000;
            Ok(Outcome::new(
                format!(
                    "t={} world {} · your clock {} · driver up {}s",
                    now.0,
                    if world_paused { "frozen" } else { "running" },
                    if mine_paused { "paused (PTO)" } else { "running" },
                    up
                ),
                json!({"now": now.0, "world_frozen": world_paused, "on_pto": mine_paused, "uptime_seconds": up}),
            ))
        }),
    );
}
