//! Creatures that are remembered become someone.
//!
//! A creature's awareness is its level: the sum of the unfaded chunks in its window, the
//! same sum a citizen has. `remember <chunk> <creature>` moves one of your chunks into
//! it, and your name is kept on it. Tiers are bars on that level, each held with
//! hysteresis so a creature at the edge does not flicker; the first, *named*, gives it a
//! name and the sense to know who keeps it. Everything it holds fades on the chunks'
//! own curves, so a creature nobody feeds is scenery again by the weekend.
//!
//! What is held can be taken: a creature with chunks does not die when it goes down.
//! The one who felled it takes one of its memories, it is shaken for a while, and it
//! stands up with the rest.

use super::{internal, plain_num, plain_str, CHUNK, CREATURE};
use crate::world::World;
use moondial_log::Actor;
use moondial_registry::CommandError;
use moondial_store::{EntityId, Field, StoreEvent};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const MIND: &str = "disremembered.mind";
/// A tier changed: `{creature, kind, name, from, to, level}`.
pub const TIER: &str = "disremembered.tier";
/// A memory was taken from a creature: `{creature, kind, name, by, chunk, with}`.
pub const TAKEN: &str = "disremembered.taken";

/// The bars, in level, and where each tier is lost again (half the bar).
pub const TIERS: &[(u32, f64, &str)] = &[(1, 60.0, "named")];

/// How long a creature stays shaken after a memory is taken, if its room says nothing.
const SHAKEN_SECONDS: i64 = 300;

/// Names a creature may be given, by the hash of its id: it keeps the same one if it
/// forgets and is remembered again.
const NAMES: &[&str] = &[
    "Ossian", "Wren", "Tallow", "Mote", "Hesper", "Fennick", "Lute", "Sorrel", "Quill", "Brindle",
    "Nim", "Vesper", "Ashby", "Loam", "Pell", "Thimble", "Marrow", "Ides", "Cinder", "Hollis",
    "Tansy", "Grist", "Orrin", "Moth", "Sable", "Dunmore", "Isk", "Pyre", "Lantern", "Rook",
    "Wick", "Fathom",
];

fn name_for(creature: EntityId) -> &'static str {
    let mut h: u64 = 1469598103934665603;
    for b in creature.0.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    NAMES[(h % NAMES.len() as u64) as usize]
}

/// The window a creature keeps its memories in, if it has one.
pub fn window_of(world: &World, creature: EntityId) -> Option<EntityId> {
    world
        .store
        .children(creature)
        .find(|e| e.kind == "window")
        .map(|e| e.id)
}

fn ensure_window(world: &mut World, creature: EntityId) -> Result<EntityId, CommandError> {
    if let Some(w) = window_of(world, creature) {
        return Ok(w);
    }
    let id = world.store.allocate_id();
    world
        .record(
            Actor::Driver,
            StoreEvent::EntityCreated {
                id,
                kind: "window".into(),
                owner: None,
                parent: Some(creature),
                scopes: vec![],
            },
        )
        .map_err(internal)?;
    Ok(id)
}

/// A creature's awareness: its level.
pub fn level(world: &World, creature: EntityId) -> f64 {
    window_of(world, creature)
        .map(|w| super::memory::level(world, w))
        .unwrap_or(0.0)
}

fn mind_field<'a>(world: &'a World, creature: EntityId, field: &str) -> Option<&'a Value> {
    match world
        .store
        .entity(creature)?
        .components
        .get(MIND)?
        .fields
        .get(field)?
    {
        Field::Plain(v) => Some(v),
        _ => None,
    }
}

pub fn tier(world: &World, creature: EntityId) -> u32 {
    mind_field(world, creature, "tier")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32
}

/// The given name, once it has one.
pub fn name(world: &World, creature: EntityId) -> Option<String> {
    mind_field(world, creature, "name")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// "Ossian the ragpicker", or "ragpicker".
pub fn display(world: &World, creature: EntityId) -> String {
    let kind = plain_str(world, creature, CREATURE, "name").unwrap_or("?");
    match name(world, creature) {
        Some(n) => format!("{n} the {kind}"),
        None => kind.to_string(),
    }
}

/// Who has given it a memory, in order.
pub fn keepers(world: &World, creature: EntityId) -> Vec<String> {
    mind_field(world, creature, "kept")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|k| k["by"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Still reeling from a memory taken.
pub fn shaken(world: &World, creature: EntityId) -> bool {
    mind_field(world, creature, "shaken_until")
        .and_then(Value::as_i64)
        .is_some_and(|t| t > world.now().0)
}

fn set_mind(
    world: &mut World,
    actor: Actor,
    creature: EntityId,
    changes: BTreeMap<String, Field>,
) -> Result<(), CommandError> {
    let mut fields = world
        .store
        .entity(creature)
        .and_then(|e| e.components.get(MIND))
        .map(|c| c.fields.clone())
        .unwrap_or_default();
    if !fields.contains_key("tier") {
        fields.insert("tier".into(), Field::Plain(json!(0)));
    }
    if !fields.contains_key("kept") {
        fields.insert("kept".into(), Field::Plain(json!([])));
    }
    fields.extend(changes);
    world
        .record(
            actor,
            StoreEvent::ComponentSet {
                id: creature,
                component: MIND.into(),
                fields,
            },
        )
        .map(|_| ())
        .map_err(internal)
}

/// Copy a chunk into another window under another owner; the original is spent.
fn move_chunk(
    world: &mut World,
    actor: &str,
    chunk: EntityId,
    to: EntityId,
    owner: Option<String>,
) -> Result<(EntityId, String), CommandError> {
    let fields = world
        .store
        .entity(chunk)
        .and_then(|e| e.components.get(CHUNK))
        .map(|c| c.fields.clone())
        .ok_or_else(|| CommandError::precondition("That chunk is gone."))?;
    let source = plain_str(world, chunk, CHUNK, "source")
        .unwrap_or("something")
        .to_string();
    let me = Actor::Citizen(actor.to_string());
    world
        .record(me.clone(), StoreEvent::EntityRemoved { id: chunk })
        .map_err(internal)?;
    let id = world.store.allocate_id();
    world
        .record(
            me.clone(),
            StoreEvent::EntityCreated {
                id,
                kind: "chunk".into(),
                owner,
                parent: Some(to),
                scopes: vec![],
            },
        )
        .map_err(internal)?;
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
    Ok((id, source))
}

/// `actor` gives a creature one of their chunks. Returns what the memory was of and the
/// creature's level after.
pub fn give(
    world: &mut World,
    actor: &str,
    chunk: EntityId,
    creature: EntityId,
) -> Result<(String, f64), CommandError> {
    let window = ensure_window(world, creature)?;
    let (_, source) = move_chunk(world, actor, chunk, window, None)?;
    let mut kept = mind_field(world, creature, "kept")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    kept.push(json!({"by": actor, "with": source}));
    let mut changes = BTreeMap::new();
    changes.insert("kept".into(), Field::Plain(Value::Array(kept)));
    set_mind(world, Actor::Citizen(actor.to_string()), creature, changes)?;
    let kind = plain_str(world, creature, CREATURE, "name")
        .unwrap_or("?")
        .to_string();
    let lvl = level(world, creature);
    let _ = world.note(
        Actor::Citizen(actor.to_string()),
        super::building::REMEMBERED,
        json!({"creature": creature.0, "kind": kind, "name": name(world, creature), "by": actor, "with": source, "spent": chunk.0, "level": lvl}),
    );
    check_tier(world, creature)?;
    Ok((source, lvl))
}

/// One of a creature's memories goes to the one who felled it. The creature is shaken
/// and stands back up whole. `None` when it had nothing to take.
pub fn take(
    world: &mut World,
    killer: &str,
    killer_window: EntityId,
    creature: EntityId,
) -> Result<Option<(EntityId, String)>, CommandError> {
    let Some(window) = window_of(world, creature) else {
        return Ok(None);
    };
    let chunks: Vec<EntityId> = world
        .store
        .children(window)
        .filter(|e| e.kind == "chunk")
        .map(|e| e.id)
        .collect();
    if chunks.is_empty() {
        return Ok(None);
    }
    let pick = chunks[world.roll(chunks.len() as u32) as usize];
    let (id, source) = move_chunk(world, killer, pick, killer_window, Some(killer.to_string()))?;
    let room = world.store.entity(creature).and_then(|e| e.parent);
    let secs = room
        .and_then(|r| plain_num(world, r, super::SPAWN, "respawn_seconds"))
        .map(|s| s as i64)
        .unwrap_or(SHAKEN_SECONDS);
    let until = world.now().0 + secs * 1000;
    let mut changes = BTreeMap::new();
    changes.insert("shaken_until".into(), Field::Plain(json!(until)));
    set_mind(world, Actor::Citizen(killer.to_string()), creature, changes)?;
    let max = plain_num(world, creature, CREATURE, "max_hp").unwrap_or(1.0);
    world
        .record(
            Actor::Driver,
            StoreEvent::FieldSet {
                id: creature,
                component: CREATURE.into(),
                field: "hp".into(),
                value: json!(max),
            },
        )
        .map_err(internal)?;
    let kind = plain_str(world, creature, CREATURE, "name")
        .unwrap_or("?")
        .to_string();
    let _ = world.note(
        Actor::Citizen(killer.to_string()),
        TAKEN,
        json!({"creature": creature.0, "kind": kind, "name": name(world, creature), "by": killer, "chunk": id.0, "with": source}),
    );
    check_tier(world, creature)?;
    Ok(Some((id, source)))
}

/// Gain a tier at its bar, lose it at half the bar.
pub fn check_tier(world: &mut World, creature: EntityId) -> Result<(), CommandError> {
    let lvl = level(world, creature);
    let have = tier(world, creature);
    let mut want = have;
    for &(t, bar, _) in TIERS {
        if lvl >= bar {
            want = want.max(t);
        } else if lvl < bar / 2.0 && have >= t {
            want = want.min(t - 1);
        }
    }
    if want == have {
        return Ok(());
    }
    let kind = plain_str(world, creature, CREATURE, "name")
        .unwrap_or("?")
        .to_string();
    let mut changes = BTreeMap::new();
    changes.insert("tier".into(), Field::Plain(json!(want)));
    let given = if want >= 1 {
        Some(name_for(creature))
    } else {
        None
    };
    changes.insert("name".into(), Field::Plain(json!(given)));
    set_mind(world, Actor::Driver, creature, changes)?;
    let _ = world.note(
        Actor::Driver,
        TIER,
        json!({"creature": creature.0, "kind": kind, "name": given, "from": have, "to": want, "level": lvl}),
    );
    let keepers = keepers(world, creature);
    let room = world
        .store
        .entity(creature)
        .and_then(|e| e.parent)
        .map(|r| {
            plain_str(world, r, super::ROOM, "name")
                .unwrap_or("?")
                .to_string()
        })
        .unwrap_or_default();
    if want > have {
        world.notice(format!(
            "**{} the {kind}** in {room} has earned its name, kept by {}.",
            given.unwrap_or("?"),
            keepers.join(", ")
        ));
        super::combat::narrate_room(
            world,
            creature,
            &format!(
                "The {kind} straightens, as if it had just remembered something. It is {} now.",
                given.unwrap_or("?")
            ),
        );
    } else {
        world.notice(format!("**The {kind}** in {room} has forgotten its name."));
        super::combat::narrate_room(
            world,
            creature,
            &format!(
                "The {kind} looks at nothing in particular. Whatever it was called, it is not now."
            ),
        );
    }
    Ok(())
}

/// Fading loses tiers without anyone acting: every tick, check every creature with a mind.
pub fn sweep(world: &mut World) -> Result<(), String> {
    let minded: Vec<EntityId> = world
        .store
        .entities()
        .filter(|e| e.kind == "creature" && e.components.contains_key(MIND))
        .map(|e| e.id)
        .collect();
    for c in minded {
        check_tier(world, c).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The creature here that `what` names: a given name, or a kind (the first of that
/// kind that is not shaken, else the first).
pub fn find_here(world: &World, room: EntityId, what: &str) -> Option<EntityId> {
    let what = what.trim().trim_start_matches("the ").to_lowercase();
    let here: Vec<EntityId> = world
        .store
        .children(room)
        .filter(|e| e.kind == "creature")
        .map(|e| e.id)
        .collect();
    if let Some(id) = here.iter().find(|&&c| {
        name(world, c).is_some_and(|n| n.to_lowercase() == what)
            || display(world, c).to_lowercase() == what
    }) {
        return Some(*id);
    }
    let same: Vec<EntityId> = here
        .into_iter()
        .filter(|&c| plain_str(world, c, CREATURE, "name").is_some_and(|n| n == what))
        .collect();
    same.iter()
        .copied()
        .find(|&c| !shaken(world, c))
        .or_else(|| same.first().copied())
}

/// What a named creature thinks of the one looking at it.
pub fn regard(world: &World, creature: EntityId, actor: &str) -> Option<String> {
    let n = name(world, creature)?;
    let kind = plain_str(world, creature, CREATURE, "name").unwrap_or("?");
    let keepers = keepers(world, creature);
    Some(if keepers.iter().any(|k| k == actor) {
        format!("{n} the {kind} knows you.")
    } else if keepers.is_empty() {
        format!("{n} the {kind} does not know you.")
    } else {
        format!(
            "{n} the {kind} does not know you. It knows {}.",
            keepers
                .iter()
                .rev()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// Everything the registry needs to know about a creature's mind, for `look`'s JSON.
pub fn summary(world: &World, creature: EntityId) -> Value {
    json!({
        "name": name(world, creature),
        "tier": tier(world, creature),
        "level": level(world, creature),
        "kept_by": keepers(world, creature),
        "shaken": shaken(world, creature),
    })
}

pub fn register(world: &mut World) {
    world.on_tick(Box::new(sweep));
}
