//! The Entropic text slice: four rooms, creatures that fight back, chunks that decay, a
//! window and a palace per citizen, PTO, and a world freeze. Everything here is a
//! *module* in the architecture's sense: it registers schemas and commands and records
//! events; the driver knows none of these words. When the WASM module runtime exists this
//! directory becomes the first module.
//!
//! The rule the slice is built around: your level is the sum of what has not faded, your
//! attack follows your level, and the fights you almost lost last longest.

mod combat;
mod identity;
mod memory;
pub mod rooms;
mod system;

use crate::world::World;
use moondial_log::Actor;
use moondial_registry::{CallContext, CommandError};
use moondial_store::{
    ClockId, ComponentSchema, EntityId, Field, FieldKind, RelationKind, StoreEvent, Visibility,
};
use moondial_time::{Bounds, Curve, Duration, TimedValue};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub use identity::LoginError;

/// Decay curves for the slice. Half-lives are short so decay is visible in a session;
/// `MOONDIAL_ACTIVE_HALF_LIFE_SECONDS` overrides the active one.
#[derive(Clone)]
pub struct Curves {
    pub active_half_life: Duration,
}

impl Curves {
    pub fn from_env() -> Self {
        let secs: i64 = std::env::var("MOONDIAL_ACTIVE_HALF_LIFE_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3600);
        Curves::with_half_life(Duration::seconds(secs))
    }

    pub fn with_half_life(active_half_life: Duration) -> Self {
        Curves { active_half_life }
    }

    /// How much longer a chunk lasts for its vividity: 1x at 0, 3x at 1.
    fn stretch(vividity: f64) -> f64 {
        1.0 + 2.0 * vividity.clamp(0.0, 1.0)
    }

    /// The active window's curve for a chunk of this vividity.
    pub fn active(&self, vividity: f64) -> Curve {
        Curve::Exponential {
            half_life: Duration((self.active_half_life.0 as f64 * Self::stretch(vividity)) as i64),
        }
    }

    /// The palace curve: strong early fade, long tail; about 5x the active half-life to
    /// halve, stretched the same way.
    pub fn palace(&self, vividity: f64) -> Curve {
        Curve::PowerLaw {
            scale: Duration(
                (self.active_half_life.0 as f64 * 5.0 * Self::stretch(vividity)) as i64,
            ),
            exponent: 1.0,
        }
    }
}

pub const CHUNK: &str = "entropic.chunk";
pub const ROOM: &str = "entropic.room";
pub const EXITS: &str = "entropic.exits";
pub const SPAWN: &str = "entropic.spawn";
pub const CITIZEN: &str = "entropic.citizen";
pub const ACCOUNT: &str = "entropic.account";
pub const BODY: &str = "entropic.body";
pub const CREATURE: &str = "entropic.creature";

/// A citizen's hit points at full health. Fixed in the slice; the level moves the attack.
pub const MAX_HP: f64 = 30.0;
/// Hit points regained per second of the citizen's active time.
pub const HP_PER_SECOND: f64 = 0.1;

/// The relations a client may follow. Declared beside the schemas so the registry
/// describes shapes, relations and commands from one place.
pub fn relations() -> Vec<(&'static str, &'static str, RelationKind)> {
    vec![
        (
            "contents",
            "What is inside: citizens and creatures in a room, chunks in a window or palace.",
            RelationKind::Children { kinds: vec![] },
        ),
        (
            "in",
            "The container: a citizen's room, a chunk's window or palace.",
            RelationKind::Parent,
        ),
        (
            "exits",
            "The rooms an exit leads to; each carries `via`, the exit name.",
            RelationKind::Field {
                component: EXITS.into(),
                field: "exits".into(),
            },
        ),
    ]
}

/// A store registry with the slice's schemas and relations.
pub fn registry() -> moondial_store::Registry {
    let mut r = moondial_store::Registry::new();
    for s in schemas() {
        r.register(s);
    }
    for (name, description, kind) in relations() {
        r.register_relation(name, description, kind);
    }
    r
}

pub fn schemas() -> Vec<ComponentSchema> {
    vec![
        ComponentSchema::new(CHUNK, 1)
            .field("value", FieldKind::Timed, Visibility::Owner)
            .field("size", FieldKind::Plain, Visibility::Owner)
            .field("vividity", FieldKind::Plain, Visibility::Owner)
            .field("source", FieldKind::Plain, Visibility::Public)
            .field("tier", FieldKind::Plain, Visibility::Public),
        ComponentSchema::new(ROOM, 1)
            .field("key", FieldKind::Plain, Visibility::Public)
            .field("name", FieldKind::Plain, Visibility::Public)
            .field("description", FieldKind::Plain, Visibility::Public),
        ComponentSchema::new(EXITS, 1).field("exits", FieldKind::Plain, Visibility::Public),
        ComponentSchema::new(SPAWN, 1)
            .field("creature", FieldKind::Plain, Visibility::Admin)
            .field("count", FieldKind::Plain, Visibility::Admin)
            .field("respawn_seconds", FieldKind::Plain, Visibility::Admin)
            .field("next_at", FieldKind::Plain, Visibility::Admin),
        ComponentSchema::new(CITIZEN, 1)
            .field("name", FieldKind::Plain, Visibility::Public)
            .field("origin", FieldKind::Plain, Visibility::Public),
        ComponentSchema::new(ACCOUNT, 1).field(
            "passphrase_hash",
            FieldKind::Plain,
            Visibility::Admin,
        ),
        ComponentSchema::new(BODY, 1)
            .field("hp", FieldKind::Timed, Visibility::Public)
            .field("max_hp", FieldKind::Plain, Visibility::Public)
            .field("engaged", FieldKind::Plain, Visibility::Owner),
        ComponentSchema::new(CREATURE, 1)
            .field("name", FieldKind::Plain, Visibility::Public)
            .field("hp", FieldKind::Plain, Visibility::Public)
            .field("max_hp", FieldKind::Plain, Visibility::Public)
            .field("attack", FieldKind::Plain, Visibility::Public)
            .field("xp", FieldKind::Plain, Visibility::Public),
    ]
}

fn account_scope(actor: &str) -> ClockId {
    ClockId(format!("account:{actor}"))
}

/// Entities the slice keeps per citizen: the citizen (in a room), a window and a palace
/// (both under the citizen).
pub struct Found {
    pub citizen: EntityId,
    pub window: EntityId,
    pub palace: EntityId,
}

fn find_citizen(world: &World, actor: &str) -> Option<Found> {
    let citizen = world
        .store
        .entities()
        .find(|e| e.kind == "citizen" && e.owner.as_deref() == Some(actor))?;
    let mut window = None;
    let mut palace = None;
    for c in world.store.children(citizen.id) {
        match c.kind.as_str() {
            "window" => window = Some(c.id),
            "palace" => palace = Some(c.id),
            _ => {}
        }
    }
    Some(Found {
        citizen: citizen.id,
        window: window?,
        palace: palace?,
    })
}

/// Build the rooms on first boot; add any room the log does not have yet.
pub fn ensure_world(world: &mut World) -> Result<(), String> {
    rooms::ensure_rooms(world)
}

/// Log a citizen in, creating them at the Gate on first login. The passphrase is set on
/// first login and checked after.
pub fn login(
    world: &mut World,
    actor: &str,
    origin: &str,
    passphrase: &str,
) -> Result<Found, LoginError> {
    let internal = |e: moondial_store::StoreError| LoginError::Internal(e.to_string());
    let me = Actor::Citizen(actor.to_string());
    let now = world.now();
    if let Some(f) = find_citizen(world, actor) {
        identity::check(world, f.citizen, passphrase)?;
        // Citizens from before passphrases and bodies existed get them on their next login.
        if plain_str(world, f.citizen, ACCOUNT, "passphrase_hash").is_none() {
            identity::acceptable(passphrase)?;
            world
                .record(
                    me.clone(),
                    StoreEvent::ComponentSet {
                        id: f.citizen,
                        component: ACCOUNT.into(),
                        fields: account_fields(passphrase)?,
                    },
                )
                .map_err(internal)?;
        }
        if world
            .store
            .entity(f.citizen)
            .is_some_and(|e| !e.components.contains_key(BODY))
        {
            world
                .record(
                    me,
                    StoreEvent::ComponentSet {
                        id: f.citizen,
                        component: BODY.into(),
                        fields: body_fields(now),
                    },
                )
                .map_err(internal)?;
        }
        return Ok(f);
    }
    identity::acceptable(passphrase)?;
    let gate =
        rooms::the_gate(world).ok_or_else(|| LoginError::Internal("the Gate is missing".into()))?;
    let citizen = world.store.allocate_id();
    world
        .record(
            me.clone(),
            StoreEvent::EntityCreated {
                id: citizen,
                kind: "citizen".into(),
                owner: Some(actor.into()),
                parent: Some(gate),
                scopes: vec![account_scope(actor)],
            },
        )
        .map_err(internal)?;
    let mut fields = BTreeMap::new();
    fields.insert("name".into(), Field::Plain(json!(actor)));
    fields.insert("origin".into(), Field::Plain(json!(origin)));
    world
        .record(
            me.clone(),
            StoreEvent::ComponentSet {
                id: citizen,
                component: CITIZEN.into(),
                fields,
            },
        )
        .map_err(internal)?;
    world
        .record(
            me.clone(),
            StoreEvent::ComponentSet {
                id: citizen,
                component: ACCOUNT.into(),
                fields: account_fields(passphrase)?,
            },
        )
        .map_err(internal)?;
    world
        .record(
            me.clone(),
            StoreEvent::ComponentSet {
                id: citizen,
                component: BODY.into(),
                fields: body_fields(now),
            },
        )
        .map_err(internal)?;
    let window = world.store.allocate_id();
    world
        .record(
            me.clone(),
            StoreEvent::EntityCreated {
                id: window,
                kind: "window".into(),
                owner: Some(actor.into()),
                parent: Some(citizen),
                scopes: vec![],
            },
        )
        .map_err(internal)?;
    let palace = world.store.allocate_id();
    world
        .record(
            me,
            StoreEvent::EntityCreated {
                id: palace,
                kind: "palace".into(),
                owner: Some(actor.into()),
                parent: Some(citizen),
                scopes: vec![],
            },
        )
        .map_err(internal)?;
    Ok(Found {
        citizen,
        window,
        palace,
    })
}

fn account_fields(passphrase: &str) -> Result<BTreeMap<String, Field>, LoginError> {
    let mut fields = BTreeMap::new();
    fields.insert(
        "passphrase_hash".into(),
        Field::Plain(json!(identity::hash(passphrase)?)),
    );
    Ok(fields)
}

/// A fresh body: full hit points, regenerating on the citizen's clock.
fn body_fields(now: moondial_time::Timestamp) -> BTreeMap<String, Field> {
    let mut fields = BTreeMap::new();
    fields.insert(
        "hp".into(),
        Field::Timed(
            TimedValue::new(
                MAX_HP,
                now,
                Curve::Linear {
                    rate_per_ms: HP_PER_SECOND / 1000.0,
                },
            )
            .with_bounds(Bounds {
                floor: 0.0,
                ceiling: Some(MAX_HP),
            }),
        ),
    );
    fields.insert("max_hp".into(), Field::Plain(json!(MAX_HP)));
    fields.insert("engaged".into(), Field::Plain(Value::Null));
    fields
}

/// Run an observation as the calling actor, for commands that render from one.
pub(crate) fn observe_as(
    world: &World,
    ctx: &CallContext,
    query: Value,
) -> Result<crate::observe::Observed, CommandError> {
    let req: crate::observe::Request = serde_json::from_value(query)
        .map_err(|e| CommandError::internal(format!("bad built-in query: {e}")))?;
    let viewer = if ctx.kind == moondial_registry::ActorKind::Admin {
        moondial_store::Viewer::admin()
    } else {
        moondial_store::Viewer::actor(&ctx.actor)
    };
    world
        .observe(&viewer, ctx.kind, &req, &moondial_query::Limits::default())
        .map_err(|e| CommandError::precondition(e.to_string()))
}

fn me(world: &World, ctx: &CallContext) -> Result<Found, CommandError> {
    find_citizen(world, &ctx.actor).ok_or_else(|| {
        CommandError::precondition(format!("{} has not arrived; log in first.", ctx.actor))
    })
}

fn internal(e: impl std::fmt::Display) -> CommandError {
    CommandError::internal(e.to_string())
}

/// A plain field as a string, or a fallback.
fn plain_str<'a>(world: &'a World, id: EntityId, component: &str, field: &str) -> Option<&'a str> {
    match world
        .store
        .entity(id)?
        .components
        .get(component)?
        .fields
        .get(field)?
    {
        Field::Plain(Value::String(s)) => Some(s.as_str()),
        _ => None,
    }
}

/// A plain field as a number, or a fallback.
fn plain_num(world: &World, id: EntityId, component: &str, field: &str) -> Option<f64> {
    match world
        .store
        .entity(id)?
        .components
        .get(component)?
        .fields
        .get(field)?
    {
        Field::Plain(v) => v.as_f64(),
        _ => None,
    }
}

/// Register the slice's commands.
pub fn register(world: &mut World, curves: Curves) {
    rooms::register(world);
    combat::register(world, curves.clone());
    memory::register(world, curves);
    system::register(world);
}

#[cfg(test)]
mod tests;
