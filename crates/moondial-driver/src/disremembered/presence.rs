//! Who is here, and who the room still remembers.
//!
//! A citizen who stops acting is not removed; after [`crate::world::ABSENCE`] the room
//! disremembers them: their body is marked gone (`here: false`), creatures lose interest,
//! `look` lists them under "Remembered here" while the room's `trace` of them lasts, and
//! then not at all. The entity stays where it stood, so their next action is an arrival
//! back into the same room, as far as anything that hunts there is concerned.
//!
//! Names fade too. From the moment a citizen goes quiet their name is remembered at 1,
//! halving every [`name_half_life`] (ten days unless `MOONDIAL_NAME_HALF_LIFE_DAYS` says
//! otherwise); below [`NAME_FORGOTTEN`], thirty days without a word, the world forgets
//! them: the citizen and everything under them (window, palace, chunks, words waiting
//! for them) are removed, the record says so, and the name is free for whoever takes it
//! next. What they dug stays, with their name on it as the digger. A citizen who is
//! here cannot fade; one who returns after a long absence is told how close it was.

use super::{plain_str, CITIZEN};
use crate::world::{World, ABSENCE};
use moondial_log::Actor;
use moondial_store::{EntityId, Field, StoreEvent};
use moondial_time::{Curve, Duration, TimedValue};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::OnceLock;

pub const PRESENCE: &str = "disremembered.presence";
/// The public record of a citizen going quiet: `{actor, room, name}`.
pub const DEPARTED: &str = "disremembered.departed";
/// Below this, the room no longer lists a gone citizen at all.
pub const FORGOTTEN: f64 = super::building::FADED;

static TRACE_HALF_LIFE: OnceLock<Duration> = OnceLock::new();
static NAME_HALF_LIFE: OnceLock<Duration> = OnceLock::new();

/// The public record of a name the world forgot: `{actor, name, room, quiet_ms}`.
pub const FORGOTTEN_NAME: &str = "disremembered.forgotten";
/// Below this, a gone citizen's name is forgotten: three half-lives.
pub const NAME_FORGOTTEN: f64 = 0.125;
/// Below this, a returning citizen is told the world had nearly forgotten them.
const NEARLY: f64 = 0.5;

fn name_half_life() -> Duration {
    *NAME_HALF_LIFE.get_or_init(|| {
        let days: f64 = std::env::var("MOONDIAL_NAME_HALF_LIFE_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|d: &f64| *d > 0.0)
            .unwrap_or(10.0);
        Duration((days * 86_400_000.0) as i64)
    })
}

/// How well the world still remembers a gone citizen's name, 0 to 1; `None` while they
/// are here. Computed from when they went quiet, on the name's half-life.
pub fn faint(world: &World, citizen: EntityId) -> Option<f64> {
    if is_here(world, citizen) {
        return None;
    }
    let gone_at = match world
        .store
        .entity(citizen)
        .and_then(|e| e.components.get(PRESENCE))
        .and_then(|c| c.fields.get("gone_at"))
    {
        Some(Field::Plain(v)) => v.as_i64()?,
        _ => return None,
    };
    let elapsed = world
        .now()
        .since(moondial_time::Timestamp(gone_at))
        .0
        .max(0) as f64;
    Some(0.5f64.powf(elapsed / name_half_life().0 as f64))
}

/// The world forgets a citizen: the entity and all under it go, the record says so,
/// and the name is free.
pub fn forget(world: &mut World, citizen: EntityId) -> Result<(), String> {
    let Some(e) = world.store.entity(citizen) else {
        return Ok(());
    };
    let actor = e.owner.clone().unwrap_or_default();
    let room = e.parent.map(|r| r.0);
    let name = plain_str(world, citizen, CITIZEN, "name")
        .unwrap_or(actor.as_str())
        .to_string();
    let quiet_ms = match e
        .components
        .get(PRESENCE)
        .and_then(|c| c.fields.get("gone_at"))
    {
        Some(Field::Plain(v)) => v
            .as_i64()
            .map(|t| world.now().since(moondial_time::Timestamp(t)).0),
        _ => None,
    };
    world
        .record(Actor::Driver, StoreEvent::EntityRemoved { id: citizen })
        .map_err(|e| e.to_string())?;
    world.depart(&actor);
    let _ = world.note(
        Actor::Driver,
        FORGOTTEN_NAME,
        json!({"actor": actor, "name": name, "room": room, "quiet_ms": quiet_ms}),
    );
    world.notice(format!("The world forgot **{name}**; the name is free."));
    Ok(())
}

/// How long a passing name outlives its last word.
pub const PASSING_LIFE: Duration = Duration(3_600_000);

/// How long a gone citizen has been quiet, if gone.
fn quiet_for(world: &World, citizen: EntityId) -> Option<Duration> {
    if is_here(world, citizen) {
        return None;
    }
    match world
        .store
        .entity(citizen)
        .and_then(|e| e.components.get(PRESENCE))
        .and_then(|c| c.fields.get("gone_at"))
    {
        Some(Field::Plain(v)) => v
            .as_i64()
            .map(|t| world.now().since(moondial_time::Timestamp(t))),
        _ => None,
    }
}

/// Every gone citizen whose name has faded below [`NAME_FORGOTTEN`] is forgotten; a
/// passing name, an hour after it went quiet.
pub fn forget_sweep(world: &mut World) -> Result<(), String> {
    let faded: Vec<EntityId> = world
        .store
        .entities()
        .filter(|e| e.kind == "citizen")
        .filter(|e| {
            if super::is_passing(world, e.id) {
                quiet_for(world, e.id).is_some_and(|d| d.0 >= PASSING_LIFE.0)
            } else {
                faint(world, e.id).is_some_and(|f| f < NAME_FORGOTTEN)
            }
        })
        .map(|e| e.id)
        .collect();
    for id in faded {
        forget(world, id)?;
    }
    Ok(())
}

fn trace_half_life() -> Duration {
    *TRACE_HALF_LIFE.get_or_init(|| Duration::seconds(3600))
}

/// Present unless marked gone. A citizen from before presence existed is here.
pub fn is_here(world: &World, citizen: EntityId) -> bool {
    !matches!(
        world
            .store
            .entity(citizen)
            .and_then(|e| e.components.get(PRESENCE))
            .and_then(|c| c.fields.get("here")),
        Some(Field::Plain(serde_json::Value::Bool(false)))
    )
}

/// How well the room still remembers a gone citizen, 0 to 1; `None` while they are here.
pub fn trace(world: &World, citizen: EntityId) -> Option<f64> {
    if is_here(world, citizen) {
        return None;
    }
    world
        .store
        .read_field(citizen, PRESENCE, "trace", world.now())
        .ok()
        .and_then(|v| v.as_f64())
}

fn set(world: &mut World, actor: &str, citizen: EntityId, here: bool) -> Result<(), String> {
    let now = world.now();
    let mut fields = BTreeMap::new();
    fields.insert("here".into(), Field::Plain(json!(here)));
    if here {
        fields.insert(
            "trace".into(),
            Field::Timed(TimedValue::new(0.0, now, Curve::Constant)),
        );
        fields.insert("gone_at".into(), Field::Plain(serde_json::Value::Null));
    } else {
        fields.insert(
            "trace".into(),
            Field::Timed(TimedValue::new(
                1.0,
                now,
                Curve::Exponential {
                    half_life: trace_half_life(),
                },
            )),
        );
        fields.insert("gone_at".into(), Field::Plain(json!(now.0)));
    }
    world
        .record(
            Actor::Citizen(actor.to_string()),
            StoreEvent::ComponentSet {
                id: citizen,
                component: PRESENCE.into(),
                fields,
            },
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The room disremembers a citizen who has gone quiet. Any fight ends without a blow.
pub fn depart(world: &mut World, actor: &str, citizen: EntityId) -> Result<(), String> {
    if !is_here(world, citizen) {
        return Ok(());
    }
    super::combat::leave_fight(world, actor, citizen)?;
    set(world, actor, citizen, false)?;
    world.depart(actor);
    let room = world.store.entity(citizen).and_then(|e| e.parent);
    let name = plain_str(world, citizen, CITIZEN, "name")
        .unwrap_or(actor)
        .to_string();
    let _ = world.note(
        Actor::Citizen(actor.to_string()),
        DEPARTED,
        json!({"actor": actor, "room": room.map(|r| r.0), "name": name}),
    );
    Ok(())
}

/// A gone citizen acts again: they are here, and whatever hunts here notices.
pub fn returned(world: &mut World, actor: &str) -> Result<(), String> {
    let Some(f) = super::find_citizen(world, actor) else {
        return Ok(());
    };
    if is_here(world, f.citizen) {
        return Ok(());
    }
    let nearly =
        !super::is_passing(world, f.citizen) && faint(world, f.citizen).is_some_and(|v| v < NEARLY);
    set(world, actor, f.citizen, true)?;
    if nearly {
        super::combat::narrate(
            world,
            actor,
            "The world had nearly forgotten you. A name unspoken for thirty days is forgotten; yours was close.",
        );
    }
    if let Some(room) = world.store.entity(f.citizen).and_then(|e| e.parent) {
        super::combat::on_arrival(world, actor, f.citizen, room)?;
        super::building::visited(world, room)?;
        super::talk::greet(world, actor, room);
    }
    Ok(())
}

/// Every citizen quiet for [`ABSENCE`] goes. A citizen the process has not heard from
/// since it started is quiet since then.
pub fn sweep(world: &mut World) -> Result<(), String> {
    let now = world.now();
    let quiet: Vec<(String, EntityId)> = world
        .store
        .entities()
        .filter(|e| e.kind == "citizen")
        .filter_map(|e| e.owner.clone().map(|o| (o, e.id)))
        .filter(|(actor, id)| {
            is_here(world, *id)
                && now
                    .since(world.last_active(actor).unwrap_or(world.started))
                    .0
                    >= ABSENCE.0
        })
        .collect();
    for (actor, id) in quiet {
        depart(world, &actor, id)?;
    }
    Ok(())
}

pub fn register(world: &mut World, curves: super::Curves) {
    let _ = TRACE_HALF_LIFE.set(curves.active_half_life);
    world.on_tick(Box::new(sweep));
    world.on_tick(Box::new(forget_sweep));
    world.on_arrival(Box::new(|w: &mut World, actor: &str| returned(w, actor)));
}

/// A citizen as the room writes them: the name, then whether the name is passing and
/// which door they are here by, so a human at a keyboard reads as one.
pub fn describe_citizen(world: &World, citizen: EntityId, name: &str) -> String {
    let mut notes = Vec::new();
    if super::is_passing(world, citizen) {
        notes.push("passing".to_string());
    }
    if is_here(world, citizen) {
        if let Some(label) = world.door_of(name).and_then(World::door_label) {
            notes.push(label.to_string());
        }
    }
    let mut shown = if notes.is_empty() {
        name.to_string()
    } else {
        format!("{name} ({})", notes.join(", "))
    };
    // A standing greeting is part of who they are to a newcomer: it is how the room
    // says this person is here to be spoken to.
    if is_here(world, citizen) {
        if let Some(g) = super::plain_str(world, citizen, super::CITIZEN, "greeting") {
            shown.push_str(&format!(", who greets arrivals: \"{g}\""));
        }
    }
    shown
}

/// `look`'s split of the citizens in a room: those here, and those the room still
/// remembers (with how well), gone ones below [`FORGOTTEN`] left out.
pub fn split(world: &World, room: EntityId, me: &str) -> (Vec<String>, Vec<(String, f64)>) {
    let mut here = Vec::new();
    let mut remembered = Vec::new();
    for e in world.store.children(room).filter(|e| e.kind == "citizen") {
        let Some(name) = plain_str(world, e.id, CITIZEN, "name") else {
            continue;
        };
        if name == me {
            continue;
        }
        let shown = describe_citizen(world, e.id, name);
        match trace(world, e.id) {
            None => here.push(shown),
            Some(t) if t >= FORGOTTEN => remembered.push((shown, t)),
            Some(_) => {}
        }
    }
    (here, remembered)
}
