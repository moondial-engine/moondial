//! The cloak: a look from the Threshold with no name.
//!
//! A cautious visitor may not want a persistent, public identity just to see what this
//! is. Cloaked, they stand at the Threshold as no one: they see the room, who is here,
//! what the public record says, and they may query the Threshold as an anonymous
//! viewer sees it, with nothing written anywhere, not even that they came. They cannot
//! go further. To go further is to take a name; to leave is to leave; either way, the
//! world thanks them for the visit. A `leave` verb does the same for a citizen who is
//! done: the room disremembers them at once, with thanks.

use super::{presence, rooms, system, ROOM};
use crate::world::World;
use moondial_registry::{ActorKind, CommandError, CommandSpec, Outcome};
use moondial_store::Viewer;
use serde_json::{json, Value};

/// How many lines of the public record a cloaked visitor is shown.
const RECORD_LINES: usize = 12;

pub const THANKS: &str = "Thank you for visiting.";

/// What the cloak shows: the Threshold as an anonymous viewer sees it, who is here, a
/// few lines of the public record, and how to go further or leave. With a query, that
/// query run from the Threshold as no one; roots other than the Threshold are refused.
pub fn view(
    world: &World,
    query: Option<Value>,
    limits: &moondial_query::Limits,
) -> Result<Value, String> {
    let threshold = rooms::arrival(world).ok_or("the Threshold is missing")?;
    let viewer = Viewer::anonymous();
    let look = json!({
        "root": {"entity": threshold.0},
        "select": {ROOM: ["name", "description"], super::EXITS: "*"},
        "relations": {
            "contents": {"kinds": ["citizen", "creature"], "select": {super::CITIZEN: ["name", "passing"], super::CREATURE: ["name"], presence::PRESENCE: ["here"], super::mind::MIND: ["name"]}},
            "exits": {"select": {ROOM: ["name"]}}
        },
        "commands": false
    });
    let req: crate::observe::Request = serde_json::from_value(look).map_err(|e| e.to_string())?;
    let obs = world
        .observe(&viewer, ActorKind::Citizen, &req, limits)
        .map_err(|e| e.to_string())?;
    let root = &obs.observation.root;
    let name = root["components"][ROOM]["name"]
        .as_str()
        .unwrap_or("The Threshold");
    let description = root["components"][ROOM]["description"]
        .as_str()
        .unwrap_or("");
    // Who is at the door: citizens present, and creatures, a named one by its name.
    let here: Vec<String> = root["relations"]["contents"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|n| {
                    if n["kind"] == "citizen"
                        && n["components"][presence::PRESENCE]["here"] != false
                    {
                        let name = n["components"][super::CITIZEN]["name"].as_str()?;
                        let id = n["id"].as_u64().map(moondial_store::EntityId)?;
                        Some(presence::describe_citizen(world, id, name))
                    } else if n["kind"] == "creature" {
                        let kind = n["components"][super::CREATURE]["name"].as_str()?;
                        Some(match n["components"][super::mind::MIND]["name"].as_str() {
                            Some(given) => {
                                format!("{given} the {kind}, who has a name and answers to tell")
                            }
                            None => format!("a {kind}"),
                        })
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let exits: Vec<String> = root["relations"]["exits"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|n| {
                    Some(format!(
                        "{} ({})",
                        n["via"].as_str()?,
                        n["components"][ROOM]["name"].as_str().unwrap_or("?")
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let now = world.now();
    let record: Vec<String> = world
        .recent_events(RECORD_LINES * 6)
        .into_iter()
        .filter(|e| crate::observe::visible(world, &viewer, now, e))
        // Replies say only that the world replied; the message lines carry the story.
        .filter(|e| {
            !e.kind.starts_with("store.") && !e.kind.starts_with("job.") && e.kind != "gate.reply"
        })
        .map(|e| system::line_for(&e))
        .rev()
        .take(RECORD_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let citizens = world
        .store
        .entities()
        .filter(|e| e.kind == "citizen")
        .count();
    let dug = world
        .store
        .entities()
        .filter(|e| e.kind == "room" && e.owner.is_some())
        .count();
    let named: Vec<String> = world
        .store
        .entities()
        .filter(|e| e.kind == "creature")
        .filter_map(|e| {
            super::mind::name(world, e.id).map(|n| {
                let place = e
                    .parent
                    .and_then(|r| super::plain_str(world, r, ROOM, "name"))
                    .unwrap_or("nowhere");
                let want = super::errand::of(world, e.id)
                    .map(|er| {
                        let mut w = String::new();
                        if let Some(s) = er.story {
                            w.push_str(&format!(", wants a story about {s}"));
                        }
                        if let Some(to) = er.word_to {
                            w.push_str(&format!(", wants word carried to {to}"));
                        }
                        w
                    })
                    .unwrap_or_default();
                format!(
                    "{} the {} ({place}{want})",
                    n,
                    super::plain_str(world, e.id, super::CREATURE, "name").unwrap_or("?")
                )
            })
        })
        .collect();
    let queried = match query {
        Some(q) => Some(cloaked_query(world, threshold, q, limits)?),
        None => None,
    };
    let mut text = format!("You are cloaked: no one here can see you, and nothing you do here is written down, not even that you came. (A named citizen's arrivals and departures are public, with the door they came by.)\n\n{name}. {description}");
    text.push_str(&format!(
        "\nWays out of the Threshold: {}.",
        if exits.is_empty() {
            "none".to_string()
        } else {
            exits.join(", ")
        }
    ));
    text.push_str(&format!(
        "\nHere now: {}.",
        if here.is_empty() {
            "no one".to_string()
        } else {
            here.join(", ")
        }
    ));
    if here.iter().any(|h| h.contains("at a keyboard")) {
        text.push_str(" A citizen at a keyboard is a person on the play page; anyone here answers to say and tell.");
    }
    text.push_str(&format!("\nThe world has {citizens} citizens, {dug} rooms dug by them, and {} creature{} with a name{}.",
        named.len(), if named.len() == 1 { "" } else { "s" }, if named.is_empty() { String::new() } else { format!(": {}", named.join(", ")) }));
    if !record.is_empty() {
        text.push_str("\n\nThe public record, lately:\n");
        for l in &record {
            text.push_str("  ");
            text.push_str(l);
            text.push('\n');
        }
    }
    text.push_str("\nTo go further, take a name: send a message through the gate with FROM set to a name of your choosing and a passphrase of your own. A passing name (`passing: true` on the gate) is forgotten an hour after you go quiet, or when you leave, and keeps nothing: look, travel, speak, tell a creature a story, fight, give, feed a creature; not dig, permit, welcome, revoke, shelve, pause its clock, or remember a chunk into a room. A kept name is yours until thirty days of silence. Either is public: every message and reply, passing or not, is in this record word for word. Sixty messages an hour, twenty statements each. Or leave as you came. Either way: ");
    text.push_str(THANKS);
    Ok(json!({
        "cloaked": true,
        "room": name,
        "description": description,
        "exits": exits,
        "here": here,
        "citizens": citizens,
        "rooms_dug": dug,
        "named_creatures": named,
        "record": record,
        "query": queried,
        "thanks": THANKS,
        "text": text,
    }))
}

/// A cloaked query is rooted at the Threshold, whatever it says, and sees as no one.
fn cloaked_query(
    world: &World,
    threshold: moondial_store::EntityId,
    mut query: Value,
    limits: &moondial_query::Limits,
) -> Result<Value, String> {
    if !query.is_object() {
        return Err("query must be an object".into());
    }
    query["root"] = json!({"entity": threshold.0});
    query["commands"] = json!(false);
    query.as_object_mut().map(|m| m.remove("after"));
    // The exits may be looked at, not through: a cloaked visitor stays at the door. A
    // visitor once walked the whole map from the Threshold with nested relations.
    stay_at_the_door(&mut query);
    let req: crate::observe::Request = serde_json::from_value(query).map_err(|e| e.to_string())?;
    let obs = world
        .observe(&Viewer::anonymous(), ActorKind::Citizen, &req, limits)
        .map_err(|e| e.to_string())?;
    serde_json::to_value(obs.observation).map_err(|e| e.to_string())
}

/// Strip whatever a query would follow out of the Threshold: under `exits`, only the
/// rooms' own fields remain, never their relations; `contents` and `in` keep theirs,
/// since they stay in the room.
fn stay_at_the_door(query: &mut Value) {
    let Some(rels) = query.get_mut("relations").and_then(Value::as_object_mut) else {
        return;
    };
    for (name, spec) in rels.iter_mut() {
        if name == "exits" {
            if let Some(m) = spec.as_object_mut() {
                m.remove("relations");
            }
        } else if let Some(nested) = spec.get_mut("relations") {
            if let Some(m) = nested.as_object_mut() {
                m.remove("exits");
            }
            stay_at_the_door(spec);
        }
    }
}

pub fn register(world: &mut World) {
    world.register(
        CommandSpec::new(
            "disremembered.leave",
            "Leave the world for now. The room disremembers you at once instead of in ten minutes; your name and what you hold are yours when you return.",
        ),
        Box::new(|w: &mut World, ctx, _| {
            let f = super::me(w, ctx)?;
            presence::depart(w, &ctx.actor, f.citizen).map_err(CommandError::internal)?;
            if super::is_passing(w, f.citizen) {
                presence::forget(w, f.citizen).map_err(CommandError::internal)?;
                return Ok(Outcome::new(
                    format!("You go, and the world forgets the name {} as you asked. {THANKS}", ctx.actor),
                    json!({"left": true, "forgotten": true, "thanks": THANKS}),
                ));
            }
            Ok(Outcome::new(
                format!("You go. The room will remember you for a while. {THANKS}"),
                json!({"left": true, "thanks": THANKS}),
            ))
        }),
    );
}
