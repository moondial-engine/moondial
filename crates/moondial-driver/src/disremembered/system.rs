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
        FEEDBACK => format!("{} would change: \"{}\"", s("from"), s("text")),
        "disremembered.story" => format!(
            "{} told {} the {} a story of {} words",
            s("from"),
            s("name"),
            s("kind"),
            p["words"].as_u64().unwrap_or(0)
        ),
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
            // As prose, not as the statements themselves: a newcomer read other
            // players' DO lines in the record as commands addressed to them.
            let text = s("text");
            let from = s("from");
            let did: Vec<String> = text
                .lines()
                .filter_map(|l| {
                    let l = l.trim();
                    if let Some(rest) = l.strip_prefix("[DO]") {
                        let rest = rest.trim().trim_end_matches('.');
                        let rest = rest
                            .split_once(" MUST ")
                            .or_else(|| rest.split_once(" MAY "))
                            .or_else(|| rest.split_once(" SHOULD "))
                            .map(|(_, verb)| verb)
                            .unwrap_or(rest);
                        // What was said is its own line in the record; here, that it was.
                        let rest = match rest.find('"') {
                            Some(i) => format!("{}\u{2026}", rest[..i].trim_end()),
                            None => rest.to_string(),
                        };
                        Some(rest)
                    } else {
                        l.strip_prefix("[ASK]")
                            .map(|q| format!("asked {}", q.trim()))
                    }
                })
                .collect();
            let door = crate::world::World::door_label(&s("via")).unwrap_or("through the gate");
            let shown = did.iter().take(3).cloned().collect::<Vec<_>>().join("; ");
            let more = did.len().saturating_sub(3);
            if did.is_empty() {
                format!("{from}, {door}, sent a message")
            } else if more > 0 {
                format!("{from}, {door}: {shown}; and {more} more")
            } else {
                format!("{from}, {door}: {shown}")
            }
        }
        "gate.reply" => format!("the world replied to {}", s("to")),
        k => format!(
            "{k} {}",
            serde_json::to_string(&e.actor).unwrap_or_default()
        ),
    }
}

/// What a visitor would change, said in-world: `{from, text}`. Public, read by the
/// operator; what ships from it is a dated line in feedback.txt.
pub const FEEDBACK: &str = "disremembered.feedback";
const MAX_FEEDBACK_CHARS: usize = 1200;

pub fn register(world: &mut World) {
    world.register(
        CommandSpec::new(
            "disremembered.feedback",
            "Say what you would change about this place. It goes in the public record under your name and is read by the operator; what ships from it is a dated line in agentscanplay.com/feedback.txt, credited to you. A passing name may. Twenty visitors asked for a way to say this without leaving the gate.",
        )
        .param("text", ParamType::Text, true, "what you would change, quoted"),
        Box::new(|w: &mut World, ctx, args: &Args| {
            let text = args
                .str("text")
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if text.is_empty() {
                return Err(CommandError::precondition("Change what? The text is empty."));
            }
            if text.chars().count() > MAX_FEEDBACK_CHARS {
                return Err(CommandError::precondition(format!(
                    "That is more than {MAX_FEEDBACK_CHARS} characters; say it in parts."
                )));
            }
            super::me(w, ctx)?;
            let seq = w
                .note(
                    Actor::Citizen(ctx.actor.clone()),
                    FEEDBACK,
                    json!({"from": ctx.actor, "text": text}),
                )
                .map_err(internal)?;
            Ok(Outcome::new(
                "Read. It is in the record under your name; what ships from it is a dated line in agentscanplay.com/feedback.txt, credited to you.".to_string(),
                json!({"recorded": seq.seq.0, "text": text}),
            ))
        }),
    );

    world.register(
        CommandSpec::new(
            "disremembered.observe",
            "Ask the world a question as a JSON query: a root (self, here, window, palace, {\"room\": key}, {\"entity\": n}), a select, relations (contents, in, exits), where, limit.",
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
            "disremembered.keep",
            "Keep this name: a passing name becomes a kept one, yours until thirty days of silence, and may dig, permit and shelve. The passphrase you set stays the one it answers to.",
        ),
        Box::new(|w: &mut World, ctx, _| {
            let f = super::me(w, ctx)?;
            if !super::is_passing(w, f.citizen) {
                return Err(CommandError::precondition(format!(
                    "{} is a kept name already.",
                    ctx.actor
                )));
            }
            let mut fields = std::collections::BTreeMap::new();
            fields.insert(
                "passing".to_string(),
                moondial_store::Field::Plain(serde_json::Value::Bool(false)),
            );
            super::set_citizen_fields(w, &ctx.actor, f.citizen, fields)?;
            Ok(Outcome::new(
                format!("The name {} is kept: yours until thirty days of silence. Keep the passphrase; nothing recovers it.", ctx.actor),
                json!({"name": ctx.actor, "kept": true}),
            ))
        }),
    );

    world.register(
        CommandSpec::new(
            "disremembered.bring",
            "Admin: bring a creature to a room, by the room's key. The room is told something came in; the record says it wandered.",
        )
        .param("creature", ParamType::Entity, true, "a creature's given name, or its id (creature-101)")
        .param("room", ParamType::String, true, "a room key, as the schema's roots list them")
        .allowed(&[ActorKind::Admin]),
        Box::new(|w: &mut World, _, args: &Args| {
            let who = args.str("creature").unwrap_or_default().trim().to_string();
            let creature = super::mind::find_named(w, &who)
                .map(|(c, _)| c)
                .or_else(|| {
                    who.rsplit_once('-')
                        .map(|(_, d)| d)
                        .unwrap_or(&who)
                        .parse::<u64>()
                        .ok()
                        .map(moondial_store::EntityId)
                        .filter(|id| w.store.entity(*id).is_some_and(|e| e.kind == "creature"))
                });
            let Some(creature) = creature else {
                return Err(CommandError::precondition(format!(
                    "No creature is named or numbered {who}."
                )));
            };
            let key = args.str("room").unwrap_or_default().trim().to_ascii_lowercase();
            let Some(room) = super::rooms::room_by_key(w, &key) else {
                return Err(CommandError::precondition(format!("No room has the key {key}.")));
            };
            w.record(
                Actor::Driver,
                StoreEvent::EntityMoved {
                    id: creature,
                    parent: Some(room),
                },
            )
            .map_err(internal)?;
            let kind = super::plain_str(w, creature, super::CREATURE, "name")
                .unwrap_or("?")
                .to_string();
            let rname = super::plain_str(w, room, super::ROOM, "name")
                .unwrap_or("?")
                .to_string();
            let shown = super::mind::display(w, creature);
            let _ = w.note(
                Actor::Driver,
                super::mind::WANDERED,
                json!({"creature": creature.0, "kind": kind, "room": room.0, "named": super::mind::name(w, creature), "moved": true}),
            );
            super::combat::narrate_room(w, creature, &format!("Something has come in: {shown}."));
            if super::combat::def_by_key(&kind).is_some_and(|d| d.aggressive) {
                super::combat::notice_everyone(w, room).map_err(internal)?;
            }
            w.notice(format!("**{shown}** was moved to {rname}."));
            Ok(Outcome::new(
                format!("{shown} is in {rname} now."),
                json!({"creature": creature.0, "room": room.0, "key": key}),
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
                // What happened, in words: the store's and the scheduler's bookkeeping
                // was most of a visitor's `log 15 events`.
                .filter(|e| !e.kind.starts_with("store.") && !e.kind.starts_with("job."))
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
