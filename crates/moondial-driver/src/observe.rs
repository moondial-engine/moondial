//! The driver's side of the observation query: named roots, the per-actor budget, the
//! legal commands at the root, and the event cursor. The engine itself is
//! `moondial-query`; everything here is what a world or a transport adds.

use crate::world::World;
use moondial_log::{Actor, Cursor, Event};
use moondial_query::{observe, Limits, Observation, Query, QueryError, RootResolver};
use moondial_registry::ActorKind;
use moondial_store::{EntityId, Viewer};
use moondial_time::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// An observation as the driver returns it: the engine's reply plus the commands this
/// actor may call and where the log stands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observed {
    /// The engine's reply.
    #[serde(flatten)]
    pub observation: Observation,
    /// Commands this actor may call now, by verb.
    pub commands: Vec<String>,
    /// The log's head; pass it back as `after` to get what happened since.
    pub cursor: u64,
    /// Events since `after` this actor may see, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub events: Vec<Event>,
}

/// A query with the driver's extras.
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    /// The query.
    #[serde(flatten)]
    pub query: Query,
    /// Include the legal commands (default true).
    #[serde(default = "yes")]
    pub commands: bool,
    /// Return events after this log position, as far as this actor may see them.
    #[serde(default)]
    pub after: Option<u64>,
}

fn yes() -> bool {
    true
}

/// Per-actor query budget: a fixed window of `per_window` queries every `window_ms`.
pub struct Budget {
    pub per_window: u32,
    pub window_ms: i64,
    used: HashMap<String, (Timestamp, u32)>,
}

impl Budget {
    pub fn new(per_window: u32, window_ms: i64) -> Self {
        Budget {
            per_window,
            window_ms,
            used: HashMap::new(),
        }
    }

    /// Spend one query for `actor` at `now`, or say when the window opens again.
    pub fn spend(&mut self, actor: &str, now: Timestamp) -> Result<(), QueryError> {
        let entry = self.used.entry(actor.to_string()).or_insert((now, 0));
        if now.since(entry.0).0 >= self.window_ms {
            *entry = (now, 0);
        }
        if entry.1 >= self.per_window {
            return Err(QueryError::OverBudget {
                retry_after_ms: self.window_ms - now.since(entry.0).0,
            });
        }
        entry.1 += 1;
        if self.used.len() > 10_000 {
            self.used
                .retain(|_, (t, _)| now.since(*t).0 < self.window_ms);
        }
        Ok(())
    }
}

/// Disremembered's named roots. `self` is the citizen, `here` the room the citizen stands
/// in, `window` and `palace` the citizen's (or, with an argument, another's: the engine
/// still shows only what the viewer may see), `room` a room by key.
pub struct Roots<'a>(pub &'a World);

impl RootResolver for Roots<'_> {
    fn resolve(&self, name: &str, arg: Option<&str>, viewer: &Viewer) -> Option<EntityId> {
        let w = self.0;
        let whose = arg.or(viewer.actor.as_deref())?;
        let citizen = || {
            w.store
                .entities()
                .find(|e| e.kind == "citizen" && e.owner.as_deref() == Some(whose))
                .map(|e| e.id)
        };
        let child = |kind: &str| {
            let c = citizen()?;
            w.store.children(c).find(|e| e.kind == kind).map(|e| e.id)
        };
        match name {
            "self" => citizen(),
            "here" => w.store.entity(citizen()?).and_then(|e| e.parent),
            "window" => child("window"),
            "palace" => child("palace"),
            "room" => crate::disremembered::rooms::room_by_key(w, arg?),
            _ => None,
        }
    }
}

/// Which events in a slice of the log this viewer may see: their own, the driver's
/// clock events, and events on entities they can currently observe. Removed entities
/// cannot be observed, so their removal is not shown; a later pass will carry visibility
/// on the event itself.
pub(crate) fn visible(world: &World, viewer: &Viewer, now: Timestamp, e: &Event) -> bool {
    if viewer.admin {
        return true;
    }
    // Public by nature: what crossed the Gate (the invitation promises it), who arrived,
    // what was said in a room, and the scheduler's bookkeeping.
    if e.kind.starts_with("gate.")
        || e.kind.starts_with("job.")
        || e.kind == crate::world::ARRIVED
        || e.kind == crate::disremembered::speech::SAY
    {
        return true;
    }
    if let (Actor::Citizen(a), Some(me)) = (&e.actor, viewer.actor.as_deref()) {
        if a == me {
            return true;
        }
    }
    if e.kind.starts_with("store.clock_") {
        return true;
    }
    // Addressed to the viewer: narrative, a reply through the Gate.
    if let (Some(to), Some(me)) = (
        e.payload.get("to").and_then(Value::as_str),
        viewer.actor.as_deref(),
    ) {
        if to == me {
            return true;
        }
    }
    match e.payload.get("id").and_then(Value::as_u64) {
        Some(id) => world.store.view(EntityId(id), viewer, now).is_some(),
        None => false,
    }
}

impl World {
    /// Run an observation for `viewer`, who acts as `kind`.
    pub fn observe(
        &self,
        viewer: &Viewer,
        kind: ActorKind,
        req: &Request,
        limits: &Limits,
    ) -> Result<Observed, QueryError> {
        let now = self.now();
        let observation = observe(&self.store, viewer, now, &req.query, &Roots(self), limits)?;
        let commands = if req.commands {
            self.commands_for(kind)
                .iter()
                .map(|s| s.verb().to_string())
                .collect()
        } else {
            Vec::new()
        };
        let cursor = self
            .log
            .head(&self.id)
            .ok()
            .flatten()
            .map(|s| s.0)
            .unwrap_or(0);
        let events = match req.after {
            Some(after) if after < cursor => self
                .log
                .read(&self.id, Cursor { after }, 200)
                .unwrap_or_default()
                .into_iter()
                .filter(|e| visible(self, viewer, now, e))
                .map(|e| redacted(self, viewer, e))
                .collect(),
            _ => Vec::new(),
        };
        Ok(Observed {
            observation,
            commands,
            cursor,
            events,
        })
    }
}

/// An event with the fields this viewer may not see removed. A `store.component_set`
/// carries every field it set, and some are the owner's or the operator's (a
/// passphrase hash is the operator's: no one else, the owner included, needs it). The
/// schema says who sees what; the entity says who owns it; what is left is what the
/// viewer would get from `view`. Other event kinds carry no component fields and pass
/// through.
pub(crate) fn redacted(world: &World, viewer: &Viewer, mut e: Event) -> Event {
    if e.kind != "store.component_set" {
        return e;
    }
    let component = e.payload["component"].as_str().unwrap_or("").to_string();
    let owner = e.payload["id"]
        .as_u64()
        .and_then(|id| world.store.entity(EntityId(id)))
        .and_then(|en| en.owner.clone());
    let is_owner = owner.is_some() && owner.as_deref() == viewer.actor.as_deref();
    let in_party = owner.as_deref().is_some_and(|o| viewer.party.contains(o));
    let schema = world.store.registry().get(&component);
    let keep = |field: &str| -> bool {
        if viewer.admin {
            return true;
        }
        let Some(f) = schema.and_then(|c| c.get(field)) else {
            return false;
        };
        match f.visibility {
            moondial_store::Visibility::Public => true,
            moondial_store::Visibility::Party => is_owner || in_party,
            moondial_store::Visibility::Owner => is_owner,
            moondial_store::Visibility::Admin => false,
        }
    };
    if let Some(fields) = e.payload.get_mut("fields").and_then(Value::as_object_mut) {
        fields.retain(|k, _| keep(k));
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_is_a_window_per_actor() {
        let mut b = Budget::new(3, 1000);
        let t = Timestamp(0);
        for _ in 0..3 {
            b.spend("dave", t).unwrap();
        }
        assert_eq!(
            b.spend("dave", Timestamp(400)),
            Err(QueryError::OverBudget {
                retry_after_ms: 600
            })
        );
        b.spend("tyler", Timestamp(400)).unwrap();
        b.spend("dave", Timestamp(1000)).unwrap();
    }
}
