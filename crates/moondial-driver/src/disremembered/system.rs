//! World-level commands: the freeze, the log, the clocks.

use super::{account_scope, internal};
use crate::world::World;
use moondial_log::Actor;
use moondial_registry::{ActorKind, Args, CommandError, CommandSpec, Outcome, ParamType};
use moondial_store::{ClockId, StoreEvent, WORLD_CLOCK};
use serde_json::json;

/// One event as a line a reader can follow: who said what, who came and went, what was
/// kept and taken; the rest by kind.
pub(crate) fn line_for(e: &moondial_log::Event) -> String {
    let p = &e.payload;
    let s = |k: &str| p[k].as_str().unwrap_or("?").to_string();
    match e.kind.as_str() {
        "disremembered.say" => match p["to"].as_str() {
            Some(to) => format!("{} says to {to}, \"{}\"", s("from"), s("text")),
            None => format!("{} says, \"{}\"", s("from"), s("text")),
        },
        "disremembered.tell" => format!("{} left word for {}", s("from"), s("to")),
        "citizen.arrived" => format!("{} arrived via {}", s("actor"), s("origin")),
        "disremembered.departed" => format!("{} went quiet", s("actor")),
        "disremembered.forgotten" => format!("the world forgot {}; the name is free", s("name")),
        "disremembered.remembered" => match p["creature"].as_u64() {
            Some(_) => format!(
                "{} gave the memory of {} to the {}",
                s("by"),
                s("with"),
                s("kind")
            ),
            None => format!(
                "{} kept {} with the memory of {}",
                s("by"),
                s("name"),
                s("with")
            ),
        },
        "disremembered.tier" => match p["name"].as_str() {
            Some(n) => format!("the {} is {n} now", s("kind")),
            None => format!("the {} has forgotten its name", s("kind")),
        },
        "disremembered.taken" => format!(
            "{} took the memory of {} from {}",
            s("by"),
            s("with"),
            p["name"].as_str().unwrap_or(&s("kind"))
        ),
        "disremembered.wandered" => format!("a {} wandered in", s("kind")),
        "gate.message" => {
            let text = s("text");
            let first_do = text
                .lines()
                .find(|l| l.starts_with("[DO]") || l.starts_with("[ASK]"))
                .unwrap_or("");
            format!("{} ({}): {first_do}", s("from"), s("via"))
        }
        "gate.reply" => format!("the world replied to {}", s("to")),
        k => format!(
            "{k} {}",
            serde_json::to_string(&e.actor).unwrap_or_default()
        ),
    }
}

pub fn register(world: &mut World) {
    world.register(
        CommandSpec::new(
            "disremembered.observe",
            "Ask the world a question as a JSON query: a root (self, here, window, palace, {\"entity\": n}), a select, relations (contents, in, exits), where, limit.",
        )
        .param("query", ParamType::String, true, "the query document, a JSON object, written here as one quoted string; the MCP observe tool takes the same object unquoted"),
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
            "disremembered.forget",
            "Admin: forget a citizen's passphrase, so the next login sets a new one.",
        )
        .param("name", ParamType::String, true, "the citizen")
        .allowed(&[ActorKind::Admin]),
        Box::new(|w: &mut World, _, args: &Args| {
            let name = args.str("name").unwrap_or_default().to_string();
            let Some(found) = super::find_citizen(w, &name) else {
                return Err(CommandError::precondition(format!(
                    "No citizen named {name}."
                )));
            };
            let mut fields = std::collections::BTreeMap::new();
            fields.insert(
                "passphrase_hash".to_string(),
                moondial_store::Field::Plain(serde_json::Value::Null),
            );
            w.record(
                Actor::Driver,
                StoreEvent::ComponentSet {
                    id: found.citizen,
                    component: super::ACCOUNT.into(),
                    fields,
                },
            )
            .map_err(internal)?;
            Ok(Outcome::new(
                format!("{name}'s passphrase is forgotten; their next login sets a new one."),
                json!({"name": name, "reset": true}),
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
        Box::new(|w: &mut World, ctx, args: &Args| {
            let n = args.int("count").unwrap_or(10).clamp(1, 100) as usize;
            // What this viewer may see, as they may see it: the same rule as /v0/log.
            let viewer = if ctx.kind == ActorKind::Admin {
                moondial_store::Viewer::admin()
            } else {
                moondial_store::Viewer::actor(&ctx.actor)
            };
            let now = w.now();
            let events: Vec<moondial_log::Event> = w
                .recent_events(n * 4)
                .into_iter()
                .filter(|e| crate::observe::visible(w, &viewer, now, e))
                .map(|e| crate::observe::redacted(w, &viewer, e))
                .collect();
            let events: Vec<moondial_log::Event> = events
                .into_iter()
                .rev()
                .take(n)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let lines: Vec<String> = events
                .iter()
                .map(|e| format!("  #{} {}", e.seq.0, line_for(e)))
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
