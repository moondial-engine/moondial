//! Who is here, and who the room still remembers.
//!
//! A citizen who stops acting is not removed; after [`crate::world::ABSENCE`] the room
//! disremembers them: their body is marked gone (`here: false`), creatures lose interest,
//! `look` lists them under "Remembered here" while the room's `trace` of them lasts, and
//! then not at all. The entity stays where it stood, so their next action is an arrival
//! back into the same room, as far as anything that hunts there is concerned.

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
    set(world, actor, f.citizen, true)?;
    if let Some(room) = world.store.entity(f.citizen).and_then(|e| e.parent) {
        super::combat::on_arrival(world, actor, f.citizen, room)?;
        super::building::visited(world, room)?;
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
    world.on_arrival(Box::new(|w: &mut World, actor: &str| returned(w, actor)));
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
        match trace(world, e.id) {
            None => here.push(name.to_string()),
            Some(t) if t >= FORGOTTEN => remembered.push((name.to_string(), t)),
            Some(_) => {}
        }
    }
    (here, remembered)
}
