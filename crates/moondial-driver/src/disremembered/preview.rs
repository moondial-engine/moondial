//! A dry run for effect. `check` says whether a message is well-formed; four visitors
//! asked what it would *do*: which room, which creature, what it would cost, and
//! whether the name is free. This walks the statements against the world as it is,
//! without touching it: the room moves with each `travel`, a creature is resolved as
//! the verb would resolve it, a kept name's verb is refused for a passing one, and
//! the name itself is reported as free, kept or passing. An estimate, not a promise:
//! the world may change before the message arrives.

use super::{errand, mind, plain_str, presence, rooms, ROOM};
use crate::world::World;
use moondial_store::EntityId;
use serde_json::{json, Value};

/// Verbs a passing name may not use.
const KEPT_ONLY: &[&str] = &[
    "dig",
    "permit",
    "welcome",
    "revoke",
    "shelve",
    "reminisce",
    "pto",
    "resume",
    "describe",
    "append",
    "name",
];

/// What the name is, before any statement: free, kept, or passing.
pub fn name_status(world: &World, actor: &str) -> (String, String) {
    match super::canonical_name(world, actor) {
        None => ("free".into(), format!("{actor} is no one's: the first message under it takes it, and sets its passphrase.")),
        Some(canon) => {
            let f = super::find_citizen(world, &canon);
            let passing = f.is_some_and(|f| super::is_passing(world, f.citizen));
            if passing {
                ("passing".into(), format!("{canon} is someone's passing name, forgotten an hour after it goes quiet; with its passphrase it is yours, else choose another."))
            } else {
                ("kept".into(), format!("{canon} is someone's kept name; with its passphrase it is yours, else choose another."))
            }
        }
    }
}

/// One line per DO: what it would do, or why it would be refused; and the header
/// faults the gate would refuse the whole message for (FROM not the actor, TO not this
/// world), which the checker alone does not see.
pub fn run(world: &World, actor: &str, passing: bool, msg: &moondial_clear::Message) -> Value {
    let statements = &msg.statements;
    let mut header: Vec<String> = Vec::new();
    let from = msg.field("FROM").unwrap_or("");
    if from != actor {
        header.push(format!(
            "FROM is {from:?} and the actor is {actor:?}: the gate acts only on messages whose FROM is the authenticated sender"
        ));
    }
    let to = msg.field("TO").unwrap_or("");
    if to != world.id.0 {
        header.push(format!(
            "TO is {to:?} and this world is {:?}: the gate refuses a message addressed elsewhere",
            world.id.0
        ));
    }
    let (status, status_line) = name_status(world, actor);
    let existing = super::canonical_name(world, actor).and_then(|c| super::find_citizen(world, &c));
    // A passing name that exists stays passing; a kept one ignores the flag.
    let passing = match &existing {
        Some(f) => super::is_passing(world, f.citizen),
        None => passing,
    };
    let mut room = existing
        .as_ref()
        .and_then(|f| rooms::room_of(world, f.citizen))
        .or_else(|| rooms::arrival(world));
    let mut lines: Vec<Value> = Vec::new();
    let mut refused = 0usize;
    for st in statements
        .iter()
        .filter(|s| s.r#type.as_deref() == Some("DO"))
    {
        let verb = st.verb.clone().unwrap_or_default();
        let args: Vec<String> = st
            .args
            .iter()
            .map(|a| match a.get("value") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            })
            .collect();
        let arg = |i: usize| args.get(i).cloned().unwrap_or_default();
        let room_name = |r: Option<EntityId>| {
            r.and_then(|r| plain_str(world, r, ROOM, "name"))
                .unwrap_or("nowhere")
                .to_string()
        };
        let (ok, what) = if passing && KEPT_ONLY.contains(&verb.as_str()) {
            (
                false,
                format!("refused: {verb} is a kept name's; keep first"),
            )
        } else {
            match verb.as_str() {
                "look" => (true, format!("shows {}", room_name(room))),
                "travel" => match room.map(|r| rooms::exits_of(world, r)) {
                    Some(exits) => match exits.get(&arg(0)).or_else(|| {
                        (arg(0) == "back")
                            .then(|| world.came_from(actor))
                            .flatten()
                            .and_then(|from| exits.values().find(|&&to| to == from))
                    }) {
                        Some(&to) => {
                            room = Some(to);
                            (true, format!("goes {} to {}", arg(0), room_name(Some(to))))
                        }
                        None => (
                            false,
                            format!(
                                "refused: no exit {} from {}; exits: {}",
                                arg(0),
                                room_name(room),
                                exits.keys().cloned().collect::<Vec<_>>().join(", ")
                            ),
                        ),
                    },
                    None => (false, "refused: no room to leave".into()),
                },
                "story" | "tell" | "attack" | "remember"
                    if !arg(if verb == "remember" { 1 } else { 0 }).is_empty()
                        || verb == "attack" =>
                {
                    let who = if verb == "remember" { arg(1) } else { arg(0) };
                    let found = room.and_then(|r| mind::find_here(world, r, &who));
                    match (verb.as_str(), found) {
                        ("story", Some(c)) => match mind::name(world, c) {
                            Some(_) if world.recent_events(2000).iter().any(|e| {
                                e.kind == super::exchange::STORY
                                    && e.payload["from"] == actor
                                    && e.payload["creature"] == c.0
                                    && world.now().0 - e.at.0 < 3_600_000
                            }) => (false, format!("refused: one story an hour per creature, and {} had one from you within the hour", mind::display(world, c))),
                            Some(_) => {
                                let wanted = errand::wants_story(world, c, &arg(1));
                                // Not the one wanted: say what it listens for, so the
                                // next draft can be.
                                let listens = errand::of(world, c)
                                    .filter(|e| e.story.is_some())
                                    .map(|e| e.keywords.join(", "));
                                (
                                    true,
                                    format!(
                                        "tells {} a story{}",
                                        mind::display(world, c),
                                        if wanted {
                                            ": the one it wants, worth double".to_string()
                                        } else if let Some(k) = listens {
                                            format!(
                                                ", not the one it wants (it listens for {k})"
                                            )
                                        } else {
                                            String::new()
                                        }
                                    ),
                                )
                            }
                            None => (
                                false,
                                format!(
                                    "refused: the {} has no name to listen with",
                                    mind::display(world, c)
                                ),
                            ),
                        },
                        ("tell", Some(c)) => {
                            let word = world
                                .store
                                .entities()
                                .filter(|e| e.kind == "creature")
                                .filter_map(|e| errand::of(world, e.id))
                                .any(|e| {
                                    e.word_to.as_deref().is_some_and(|t| {
                                        Some(t.to_lowercase())
                                            == mind::name(world, c).map(|n| n.to_lowercase())
                                    }) && e.word.as_deref().is_some_and(|w| {
                                        arg(1).to_lowercase().contains(&w.to_lowercase())
                                    })
                                });
                            (
                                true,
                                format!(
                                    "reaches {}{}",
                                    mind::display(world, c),
                                    if word {
                                        ": a word it wants carried, worth a chunk"
                                    } else if mind::name(world, c).is_none() {
                                        ", which has no name and will not understand"
                                    } else {
                                        ""
                                    }
                                ),
                            )
                        }
                        ("attack", Some(c)) => {
                            let kind = plain_str(world, c, super::CREATURE, "name").unwrap_or("?");
                            let asked = who.trim().trim_start_matches("the ").to_lowercase();
                            if mind::name(world, c).is_some() && (asked.is_empty() || asked == kind)
                            {
                                (
                                    false,
                                    format!(
                                        "refused: {} has a name and is struck only by it",
                                        mind::display(world, c)
                                    ),
                                )
                            } else {
                                (true, format!("strikes {}", mind::display(world, c)))
                            }
                        }
                        ("remember", Some(c)) => (
                            true,
                            format!("gives {} to {}", arg(0), mind::display(world, c)),
                        ),
                        ("tell", None) => match super::canonical_name(world, &who) {
                            Some(cz) => {
                                let here = super::find_citizen(world, &cz).is_some_and(|f| {
                                    rooms::room_of(world, f.citizen) == room
                                        && presence::is_here(world, f.citizen)
                                });
                                (
                                    true,
                                    format!(
                                        "reaches {cz}{}",
                                        if here {
                                            ", who is here"
                                        } else {
                                            " when they next arrive"
                                        }
                                    ),
                                )
                            }
                            None => (
                                false,
                                format!(
                                    "refused: no one called {who}, and no such creature in {}",
                                    room_name(room)
                                ),
                            ),
                        },
                        ("attack", None) if who.is_empty() => {
                            let unnamed = room
                                .map(|r| super::combat::creatures_in(world, r))
                                .unwrap_or_default()
                                .into_iter()
                                .find(|c| mind::name(world, c.0).is_none());
                            match unnamed {
                                Some(c) => (true, format!("strikes the {}", c.1)),
                                None => (
                                    false,
                                    format!(
                                        "refused: nothing unnamed to fight in {}",
                                        room_name(room)
                                    ),
                                ),
                            }
                        }
                        (_, None) => (false, format!("refused: no {who} in {}", room_name(room))),
                        _ => (true, format!("runs {verb}")),
                    }
                }
                "leave" => (
                    true,
                    if passing {
                        "leaves; the name is forgotten at once".into()
                    } else {
                        "leaves; the room forgets you now".into()
                    },
                ),
                "keep" => (
                    true,
                    if passing {
                        "keeps the name: thirty days of silence from then".into()
                    } else {
                        "runs keep (already a kept name)".into()
                    },
                ),
                "feedback" | "say" | "greet" | "chunks" | "palace" | "time" | "log" | "rest"
                | "retreat" | "give" | "observe" => (true, format!("runs {verb}")),
                other => (true, format!("runs {other}, if the world knows it")),
            }
        };
        if !ok {
            refused += 1;
        }
        lines.push(json!({"statement": st.index, "verb": verb, "ok": ok, "what": what}));
    }
    let dos = lines.len();
    let cost = if dos == 0 {
        "no statements; nothing to send"
    } else if !header.is_empty() {
        "a quarter unit: the gate would refuse the whole message on its header"
    } else {
        "one unit of sixty an hour if the checker passes it, a quarter if it refuses"
    };
    json!({
        "actor": actor,
        "name": status,
        "name_note": status_line,
        "passing": passing,
        "from": room.and_then(|r| plain_str(world, r, ROOM, "name")),
        "header": header,
        "statements": lines,
        "refusals": refused,
        "cost": cost,
        "note": "an estimate against the world as it is now, not a promise; nothing was written"
    })
}

/// The preview as prose, for the text half of the tool reply.
pub fn text(p: &Value) -> String {
    let mut out = format!(
        "Dry run for {}: {}\n",
        p["actor"].as_str().unwrap_or("?"),
        p["name_note"].as_str().unwrap_or("")
    );
    for h in p["header"].as_array().into_iter().flatten() {
        out.push_str(&format!("  NO  header: {}\n", h.as_str().unwrap_or("")));
    }
    for l in p["statements"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "  {}. {} {}\n",
            l["statement"],
            if l["ok"].as_bool().unwrap_or(false) {
                "ok "
            } else {
                "NO "
            },
            l["what"].as_str().unwrap_or("")
        ));
    }
    out.push_str(&format!(
        "Cost: {}. {}",
        p["cost"].as_str().unwrap_or(""),
        p["note"].as_str().unwrap_or("")
    ));
    out
}
