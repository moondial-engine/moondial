//! Disremembered's answers to the vocabulary's question patterns. Each answer is an
//! observation query as the asking citizen, rendered as FACT lines in the world's voice.
//! What the asker may not see, the answer says is not visible; it never guesses.

use super::{BODY, CHUNK, CITIZEN, ROOM};
use crate::gate::{iso_minute, observe, AskBinding, Report};
use crate::world::World;
use moondial_clear::Statement;
use moondial_store::EntityId;
use serde_json::{json, Value};

pub struct Asks;

/// A citizen by name, or the asker for `me`, `I`, `myself`, `self`.
fn citizen(world: &World, actor: &str, name: &str) -> Option<(String, EntityId)> {
    let name = name.trim().trim_start_matches("the ").trim();
    let who = if matches!(
        name.to_ascii_lowercase().as_str(),
        "me" | "i" | "myself" | "self"
    ) {
        actor
    } else {
        name
    };
    world
        .store
        .entities()
        .find(|e| e.kind == "citizen" && e.owner.as_deref() == Some(who))
        .map(|e| (who.to_string(), e.id))
}

/// A room by key or name: `fogbank`, `the fogbank`, `the fog bank`.
fn room(world: &World, name: &str) -> Option<EntityId> {
    let wanted = name
        .trim()
        .trim_start_matches("the ")
        .replace(' ', "")
        .to_ascii_lowercase();
    world
        .store
        .entities()
        .filter(|e| e.kind == "room")
        .find(|e| {
            let key = super::plain_str(world, e.id, ROOM, "key")
                .unwrap_or("")
                .to_ascii_lowercase();
            let rname = super::plain_str(world, e.id, ROOM, "name")
                .unwrap_or("")
                .trim_start_matches("The ")
                .replace(' ', "")
                .to_ascii_lowercase();
            key == wanted || rname == wanted
        })
        .map(|e| e.id)
}

fn cursor(obs: &crate::observe::Observed) -> String {
    format!("ev-{}", obs.cursor)
}

fn room_name(node: &Value) -> String {
    node["components"][ROOM]["name"]
        .as_str()
        .unwrap_or("an unnamed room")
        .to_string()
}

impl AskBinding for Asks {
    fn answer(&self, world: &World, actor: &str, st: &Statement, report: &mut Report) -> bool {
        let n = st.index;
        let at = iso_minute(world.now());
        let pattern = st.pattern.as_deref().unwrap_or("");
        let slot = |k: &str| {
            st.slots
                .get(k)
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };

        if pattern.starts_with("^(what|who) is here") {
            let Some((_, me)) = citizen(world, actor, "me") else {
                report.fact(
                    format!("The world knows no citizen named {actor} as of {at}"),
                    "gate",
                );
                return true;
            };
            let Some(here) = world.store.entity(me).and_then(|e| e.parent) else {
                report.fact(format!("{actor} is nowhere as of {at}"), "gate");
                return true;
            };
            let q = json!({"root": {"entity": here.0}, "select": {ROOM: ["name"]}, "relations": {"contents": {"kinds": ["citizen", "creature"], "select": {CITIZEN: ["name"], super::CREATURE: ["name"], super::mind::MIND: ["name"], super::presence::PRESENCE: ["here"]}}, "exits": {"select": {ROOM: ["name"]}}}, "commands": false});
            match observe(world, actor, q) {
                Ok(obs) => {
                    let root = &obs.observation.root;
                    let name = root["components"][ROOM]["name"]
                        .as_str()
                        .unwrap_or("here")
                        .to_string();
                    let mut citizens = Vec::new();
                    let mut creatures = Vec::new();
                    for n in root["relations"]["contents"]
                        .as_array()
                        .into_iter()
                        .flatten()
                    {
                        if n["kind"] == "citizen"
                            && n["components"][super::presence::PRESENCE]["here"] != false
                        {
                            if let Some(c) = n["components"][CITIZEN]["name"].as_str() {
                                if c != actor {
                                    citizens.push(c.to_string());
                                }
                            }
                        } else if n["kind"] == "creature" {
                            let kind = n["components"][super::CREATURE]["name"]
                                .as_str()
                                .unwrap_or("?");
                            creatures.push(
                                match n["components"][super::mind::MIND]["name"].as_str() {
                                    Some(g) => format!("{g} the {kind}"),
                                    None => kind.to_string(),
                                },
                            );
                        }
                    }
                    let exits: Vec<String> = root["relations"]["exits"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|n| n["via"].as_str().map(str::to_string))
                        .collect();
                    let c = cursor(&obs);
                    report.fact(format!("{actor} is in {name} as of {at}"), c.clone());
                    report.fact(
                        format!(
                            "The ways out are {}",
                            if exits.is_empty() {
                                "none".into()
                            } else {
                                exits.join(", ")
                            }
                        ),
                        c.clone(),
                    );
                    report.fact(
                        format!(
                            "The creatures here are {}",
                            if creatures.is_empty() {
                                "none".into()
                            } else {
                                creatures.join(", ")
                            }
                        ),
                        c.clone(),
                    );
                    report.fact(
                        format!(
                            "The other citizens here are {}",
                            if citizens.is_empty() {
                                "none".into()
                            } else {
                                citizens.join(", ")
                            }
                        ),
                        c,
                    );
                }
                Err(e) => report.fact(format!("Question {n} could not be answered: {e}"), "query"),
            }
            return true;
        }

        if pattern.starts_with("^where is ") {
            let entity = slot("entity");
            let Some((who, id)) = citizen(world, actor, &entity) else {
                // Not a citizen: a named creature, perhaps.
                if let Some((creature, room)) = super::mind::find_named(world, &entity) {
                    let place = room
                        .and_then(|r| super::plain_str(world, r, ROOM, "name").map(str::to_string))
                        .unwrap_or_else(|| "nowhere".to_string());
                    report.fact(
                        format!(
                            "{} is in {place} as of {at}",
                            super::mind::display(world, creature)
                        ),
                        "gate",
                    );
                    return true;
                }
                report.fact(
                    format!(
                        "The world knows no citizen or named creature called {entity} as of {at}"
                    ),
                    "gate",
                );
                return true;
            };
            let q = json!({"root": {"entity": id.0}, "select": {}, "relations": {"in": {"select": {ROOM: ["name"]}}}, "commands": false});
            match observe(world, actor, q) {
                Ok(obs) => {
                    let name = room_name(&obs.observation.root["relations"]["in"]);
                    report.fact(format!("{who} is in {name} as of {at}"), cursor(&obs));
                }
                Err(e) => report.fact(format!("Question {n} could not be answered: {e}"), "query"),
            }
            return true;
        }

        if pattern.starts_with("^what is the ") {
            let prop = slot("prop").to_ascii_lowercase();
            let entity = slot("entity");
            let Some((who, id)) = citizen(world, actor, &entity) else {
                report.fact(
                    format!("The world knows no citizen named {entity} as of {at}"),
                    "gate",
                );
                return true;
            };
            match prop.as_str() {
                "level" | "attack" => {
                    let window = world
                        .store
                        .children(id)
                        .find(|e| e.kind == "window")
                        .map(|e| e.id);
                    let Some(window) = window else {
                        report.fact(format!("{who} has no active window"), "gate");
                        return true;
                    };
                    let q = json!({"root": {"entity": window.0}, "select": {}, "relations": {"contents": {"kinds": ["chunk"], "select": {CHUNK: ["value"]}}}, "commands": false});
                    match observe(world, actor, q) {
                        Ok(obs) => {
                            let chunks = obs.observation.root["relations"]["contents"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default();
                            let visible: Vec<f64> = chunks
                                .iter()
                                .filter_map(|c| c["components"][CHUNK]["value"].as_f64())
                                .collect();
                            let level: f64 = visible.iter().fold(0.0, |a, b| a + b);
                            if who != actor && visible.len() < chunks.len() {
                                report.fact(
                                    format!("The {prop} of {who} is not visible to {actor}"),
                                    cursor(&obs),
                                );
                            } else {
                                if prop == "level" {
                                    report.fact(
                                        format!("The level of {who} is {level:.2} XP as of {at}"),
                                        cursor(&obs),
                                    );
                                } else {
                                    report.fact(format!("The attack of {who} is {:.0} damage per round as of {at}", super::combat::attack_for_level(level)), cursor(&obs));
                                }
                            }
                        }
                        Err(e) => {
                            report.fact(format!("Question {n} could not be answered: {e}"), "query")
                        }
                    }
                }
                "hp" | "hit points" | "health" => {
                    let q = json!({"root": {"entity": id.0}, "select": {BODY: ["hp", "max_hp"]}, "commands": false});
                    match observe(world, actor, q) {
                        Ok(obs) => {
                            let b = &obs.observation.root["components"][BODY];
                            match (b["hp"].as_f64(), b["max_hp"].as_f64()) {
                                (Some(hp), Some(max)) => report.fact(
                                    format!("{who} has {hp:.0} HP of {max:.0} HP as of {at}"),
                                    cursor(&obs),
                                ),
                                _ => report.fact(
                                    format!("The hit points of {who} are not visible to {actor} as of {at}"),
                                    cursor(&obs),
                                ),
                            }
                        }
                        Err(e) => {
                            report.fact(format!("Question {n} could not be answered: {e}"), "query")
                        }
                    }
                }
                "room" | "location" | "position" => {
                    let q = json!({"root": {"entity": id.0}, "select": {}, "relations": {"in": {"select": {ROOM: ["name"]}}}, "commands": false});
                    match observe(world, actor, q) {
                        Ok(obs) => report.fact(
                            format!(
                                "{who} is in {} as of {at}",
                                room_name(&obs.observation.root["relations"]["in"])
                            ),
                            cursor(&obs),
                        ),
                        Err(e) => {
                            report.fact(format!("Question {n} could not be answered: {e}"), "query")
                        }
                    }
                }
                _ => {
                    report.fact(format!("The world has no property named {prop}; it knows level, attack, hit points and room"), "gate");
                }
            }
            return true;
        }

        if pattern.starts_with("^how many ") {
            let noun = slot("noun").to_ascii_lowercase();
            let rest = slot("rest").to_ascii_lowercase();
            match noun.as_str() {
                "chunks" | "chunk" => {
                    // "does X hold", "are in X's window", "do I hold"
                    let who = rest
                        .trim_start_matches("does ")
                        .trim_start_matches("do ")
                        .split(' ')
                        .next()
                        .unwrap_or("me")
                        .trim_end_matches("'s")
                        .to_string();
                    let Some((who, id)) = citizen(world, actor, &who) else {
                        report.fact(
                            format!("The world knows no citizen named {who} as of {at}"),
                            "gate",
                        );
                        return true;
                    };
                    let window = world
                        .store
                        .children(id)
                        .find(|e| e.kind == "window")
                        .map(|e| e.id);
                    let Some(window) = window else {
                        report.fact(format!("{who} has no active window"), "gate");
                        return true;
                    };
                    let q = json!({"root": {"entity": window.0}, "select": {}, "relations": {"contents": {"kinds": ["chunk"], "select": {}}}, "commands": false});
                    match observe(world, actor, q) {
                        Ok(obs) => {
                            let count = obs.observation.root["relations"]["contents"]
                                .as_array()
                                .map(Vec::len)
                                .unwrap_or(0);
                            report.fact(
                                format!(
                                    "{who} holds {count} chunks in the active window as of {at}"
                                ),
                                cursor(&obs),
                            );
                        }
                        Err(e) => {
                            report.fact(format!("Question {n} could not be answered: {e}"), "query")
                        }
                    }
                }
                "creatures" | "creature" | "citizens" | "citizen" => {
                    // "are here", "are in the fogbank"
                    let place = rest
                        .trim_start_matches("are ")
                        .trim_start_matches("is ")
                        .trim();
                    let room_id = if place == "here" || place.is_empty() {
                        citizen(world, actor, "me")
                            .and_then(|(_, id)| world.store.entity(id).and_then(|e| e.parent))
                    } else {
                        room(world, place.trim_start_matches("in "))
                    };
                    let Some(room_id) = room_id else {
                        report.fact(
                            format!("The world knows no room named {place} as of {at}"),
                            "gate",
                        );
                        return true;
                    };
                    let kind = if noun.starts_with("creature") {
                        "creature"
                    } else {
                        "citizen"
                    };
                    let q = json!({"root": {"entity": room_id.0}, "select": {ROOM: ["name"]}, "relations": {"contents": {"kinds": [kind], "select": {CITIZEN: ["name"], super::CREATURE: ["name"]}}}, "commands": false});
                    match observe(world, actor, q) {
                        Ok(obs) => {
                            let count = obs.observation.root["relations"]["contents"]
                                .as_array()
                                .map(Vec::len)
                                .unwrap_or(0);
                            let name = room_name(&obs.observation.root);
                            report.fact(
                                format!(
                                    "{count} {} {} in {name} as of {at}",
                                    if count == 1 {
                                        noun.trim_end_matches('s')
                                    } else {
                                        &noun
                                    },
                                    if count == 1 { "is" } else { "are" }
                                ),
                                cursor(&obs),
                            );
                        }
                        Err(e) => {
                            report.fact(format!("Question {n} could not be answered: {e}"), "query")
                        }
                    }
                }
                _ => {
                    report.fact(format!("The world does not count {noun}; it counts chunks, creatures and citizens"), "gate");
                }
            }
            return true;
        }

        false
    }
}
