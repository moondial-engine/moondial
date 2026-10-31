//! The Disremembered text slice: four rooms, creatures that fight back, chunks that decay, a
//! window and a palace per citizen, PTO, and a world freeze. Everything here is a
//! *module* in the architecture's sense: it registers schemas and commands and records
//! events; the driver knows none of these words. When the WASM module runtime exists this
//! directory becomes the first module.
//!
//! The rule the slice is built around: your level is the sum of what has not faded, your
//! attack follows your level, and the fights you almost lost last longest.

pub mod ask;
pub mod building;
pub mod cloak;
mod combat;
pub mod errand;
pub mod exchange;
mod identity;
mod memory;
pub mod mind;
pub mod presence;
pub mod rooms;
pub mod speech;
pub mod system;
pub mod talk;

use crate::world::World;
use moondial_log::Actor;
use moondial_registry::{CallContext, CommandError};
use moondial_store::{
    ClockId, ComponentSchema, EntityId, Field, FieldKind, RelationKind, StoreEvent, Visibility,
};
use moondial_time::{Bounds, Curve, Duration, TimedValue};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub use combat::NARRATIVE;
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

pub const CHUNK: &str = "disremembered.chunk";
pub const ROOM: &str = "disremembered.room";
pub const EXITS: &str = "disremembered.exits";
pub const SPAWN: &str = "disremembered.spawn";
pub const CITIZEN: &str = "disremembered.citizen";
pub const ACCOUNT: &str = "disremembered.account";
pub const BODY: &str = "disremembered.body";
pub const CREATURE: &str = "disremembered.creature";
/// How a thing looks. Presentation is a component like any other, so the driver never
/// knows what a sprite is and a text client never has to see one.
pub const SPRITE: &str = "disremembered.sprite";

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

/// The named roots an observation may start from, for a client that builds queries:
/// each with what its argument is, if it takes one, and where a client can list the
/// values (`room` takes a room key; `window` and `palace` take a citizen's name).
pub fn roots() -> Value {
    json!([
        {"name": "self", "description": "You: the citizen."},
        {"name": "here", "description": "The room you stand in."},
        {"name": "window", "description": "Your active window of chunks, or another citizen's: you still see only what is yours to see.", "arg": "citizen", "optional": true},
        {"name": "palace", "description": "Your Memory Palace, or another citizen's.", "arg": "citizen", "optional": true},
        {"name": "room", "description": "A room, by key.", "arg": "room key", "values": {"kind": "room", "component": ROOM, "field": "key"}},
        {"name": "entity", "description": "Any entity, by id.", "arg": "id"}
    ])
}

/// How to read what the schema does not say by itself. Served with it.
pub fn schema_notes() -> Vec<&'static str> {
    vec![
        "A creature's disremembered.creature.name is its kind (wisp, hollow, ragpicker). A creature that has been remembered to 60 also carries disremembered.mind, whose name is its given name and whose tier is at least 1; it answers to tell and story, knows its keepers, and is written as 'Lute the wisp'. A creature with no mind component, or a mind with no name, is unnamed: fair game, and deaf.",
        "A room with an owner was dug by a citizen; its memory field fades on half_life_ms and is 1 again when someone visits. The seed rooms have no owner and no memory field, and never fade.",
        "A chunk's value is a timed field: read it now and it is smaller than a moment ago. The chunks command states the rate.",
        "A named creature with disremembered.errand wants something: story (a story whose words touch it, worth double) and/or word (carry it to the creature named in word_to, by travel and tell; look gives the route). The two parts are independent. A done part is a public disremembered.errand_done, it clears, and the creature knows who did it.",
        "A citizen's disremembered.presence says whether they are here (acting within the last ten minutes) or a trace the room still remembers; gone_at is when they went quiet. A room's contents relation returns traced citizens too, with the room as parent, while look lists them as remembered: select disremembered.presence here to tell the two apart. Thirty days after gone_at the world forgets the citizen (a disremembered.forgotten event) and the name is free.",
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
        ComponentSchema::new(ROOM, 2)
            .field("key", FieldKind::Plain, Visibility::Public)
            .field("name", FieldKind::Plain, Visibility::Public)
            .field("description", FieldKind::Plain, Visibility::Public)
            // A dug room's own memory of itself: 1 when visited, fading on the half-life
            // of the chunk that paid for it. Seed rooms have none and never fade.
            .field("memory", FieldKind::Timed, Visibility::Public)
            .field("half_life_ms", FieldKind::Plain, Visibility::Public)
            // Who else may describe and name it: citizen name to an expiry (ms) or null.
            .field("permits", FieldKind::Plain, Visibility::Owner)
            // Who may only add to it (a guest book), same shape.
            .field("welcomes", FieldKind::Plain, Visibility::Owner)
            // The room it was dug from, and what memory paid for it.
            .field("origin", FieldKind::Plain, Visibility::Public)
            .field("dug_with", FieldKind::Plain, Visibility::Public)
            // Who gave up a memory so this room would last: `[{by, with, half_life_ms}]`.
            .field("kept", FieldKind::Plain, Visibility::Public),
        ComponentSchema::new(EXITS, 1).field("exits", FieldKind::Plain, Visibility::Public),
        // A creature that is remembered: its tier, its given name, who keeps it, and
        // whether a memory was just taken from it. Its level is its window's.
        // What a named creature wants: a story about something, or a word carried to
        // another named creature. look and the cloak say it in one line.
        ComponentSchema::new(errand::ERRAND, 2)
            .field("story", FieldKind::Plain, Visibility::Public)
            .field("keywords", FieldKind::Plain, Visibility::Public)
            .field("story_by", FieldKind::Plain, Visibility::Public)
            .field("word", FieldKind::Plain, Visibility::Public)
            .field("word_to", FieldKind::Plain, Visibility::Public)
            .field("word_by", FieldKind::Plain, Visibility::Public)
            // Version 1's fields, kept so the log replays: a schema that drops a field
            // refuses its own history, and the deploy rolled back on the first boot.
            .field("kind", FieldKind::Plain, Visibility::Public)
            .field("want", FieldKind::Plain, Visibility::Public)
            .field("to", FieldKind::Plain, Visibility::Public)
            .field("set_by", FieldKind::Plain, Visibility::Public),
        ComponentSchema::new(mind::MIND, 1)
            .field("tier", FieldKind::Plain, Visibility::Public)
            .field("name", FieldKind::Plain, Visibility::Public)
            .field("kept", FieldKind::Plain, Visibility::Public)
            .field("shaken_until", FieldKind::Plain, Visibility::Public),
        // Whether a citizen is here, and how well the room remembers them once gone.
        ComponentSchema::new(presence::PRESENCE, 1)
            .field("here", FieldKind::Plain, Visibility::Public)
            .field("trace", FieldKind::Timed, Visibility::Public)
            .field("gone_at", FieldKind::Plain, Visibility::Public)
            // The door they last came by: session, terminal (a keyboard), http, gate, mcp.
            .field("door", FieldKind::Plain, Visibility::Public)
            // The log position up to which words left for them have been delivered.
            .field("heard_through", FieldKind::Plain, Visibility::Owner),
        ComponentSchema::new(SPAWN, 1)
            .field("creature", FieldKind::Plain, Visibility::Admin)
            .field("count", FieldKind::Plain, Visibility::Admin)
            .field("respawn_seconds", FieldKind::Plain, Visibility::Admin)
            .field("next_at", FieldKind::Plain, Visibility::Admin),
        ComponentSchema::new(CITIZEN, 1)
            .field("name", FieldKind::Plain, Visibility::Public)
            .field("origin", FieldKind::Plain, Visibility::Public)
            // A passing name: forgotten an hour after it goes quiet, keeping nothing.
            .field("passing", FieldKind::Plain, Visibility::Public)
            // A standing greeting: what this citizen says to whoever arrives where
            // they are, while they are here. Null when they have none.
            .field("greeting", FieldKind::Plain, Visibility::Public),
        ComponentSchema::new(ACCOUNT, 1).field(
            "passphrase_hash",
            FieldKind::Plain,
            Visibility::Admin,
        ),
        ComponentSchema::new(BODY, 1)
            .field("hp", FieldKind::Timed, Visibility::Public)
            .field("max_hp", FieldKind::Plain, Visibility::Public)
            .field("engaged", FieldKind::Plain, Visibility::Owner),
        ComponentSchema::new(SPRITE, 2)
            .field("glyph", FieldKind::Plain, Visibility::Public)
            .field("tint", FieldKind::Plain, Visibility::Public)
            .field("atlas", FieldKind::Plain, Visibility::Public)
            .field("frame", FieldKind::Plain, Visibility::Public),
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

/// Change a citizen's passphrase: the old one must be right, the new one acceptable.
/// Never a gate verb, so the words never reach the log.
pub fn change_passphrase(
    world: &mut World,
    actor: &str,
    old: &str,
    new: &str,
) -> Result<(), LoginError> {
    let f = find_citizen(world, actor).ok_or(LoginError::Missing)?;
    if let Some(minutes) = world.locked_for(actor) {
        return Err(LoginError::Locked {
            name: actor.to_string(),
            minutes,
        });
    }
    if let Err(e) = identity::check(world, f.citizen, old) {
        if matches!(e, LoginError::WrongPassphrase { .. }) {
            world.failed_login(actor);
        }
        return Err(e);
    }
    identity::acceptable(new)?;
    world
        .record(
            Actor::Citizen(actor.to_string()),
            StoreEvent::ComponentSet {
                id: f.citizen,
                component: ACCOUNT.into(),
                fields: account_fields(new)?,
            },
        )
        .map_err(|e| LoginError::Internal(e.to_string()))?;
    world.succeeded_login(actor);
    Ok(())
}

/// Entities the slice keeps per citizen: the citizen (in a room), a window and a palace
/// (both under the citizen).
pub struct Found {
    pub citizen: EntityId,
    pub window: EntityId,
    pub palace: EntityId,
}

/// A citizen's name as it is spelled, from any spelling of it: names are unique
/// regardless of case, so `lantern_moth` finds Lantern_Moth.
pub(crate) fn canonical_name(world: &World, name: &str) -> Option<String> {
    let lower = name.trim().to_lowercase();
    world
        .store
        .entities()
        .filter(|e| e.kind == "citizen")
        .filter_map(|e| e.owner.clone())
        .find(|o| o.to_lowercase() == lower)
}

pub(crate) fn find_citizen(world: &World, actor: &str) -> Option<Found> {
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
    rooms::ensure_rooms(world)?;
    combat::ensure_spawns(world)?;
    building::reframe_all(world)?;
    mind::ensure_wander(world)
}

/// Log a citizen in, creating them at the Threshold on first login. The passphrase is set on
/// first login and checked after.
pub fn login(
    world: &mut World,
    actor: &str,
    origin: &str,
    passphrase: &str,
) -> Result<Found, LoginError> {
    login_as(world, actor, origin, passphrase, false)
}

/// Three safe first moves for a new name, with a creature that would take a story
/// named if one is near. Asked for by a visitor who had to work them out.
fn first_moves(world: &World) -> String {
    // The nearest named creature: here, then one exit away (with the exit named), then
    // anywhere, with its room.
    let home = rooms::arrival(world);
    let named: Vec<(Option<EntityId>, String, Option<String>)> = world
        .store
        .entities()
        .filter(|e| e.kind == "creature")
        .filter_map(|e| {
            mind::name(world, e.id).map(|n| {
                // What it wants, if a story: the first move is then the errand.
                let want = errand::of(world, e.id).and_then(|er| er.story);
                (e.parent, n, want)
            })
        })
        .collect();
    let exits = home.map(|h| rooms::exits_of(world, h)).unwrap_or_default();
    let ask = |n: &str, want: &Option<String>| match want {
        Some(w) => format!("story {n} \"<a dozen words about {w}>\""),
        None => format!("story {n} \"<a dozen words>\""),
    };
    let listener = named
        .iter()
        .find(|(room, _, _)| *room == home)
        .map(|(_, n, want)| {
            format!(
                "{} ({n} is here{})",
                ask(n, want),
                if want.is_some() {
                    ", and wants that"
                } else {
                    ""
                }
            )
        })
        .or_else(|| {
            named.iter().find_map(|(room, n, want)| {
                let (via, _) = exits.iter().find(|(_, to)| Some(**to) == *room)?;
                Some(format!("travel {via}, then {}", ask(n, want)))
            })
        })
        .or_else(|| {
            named.first().map(|(room, n, want)| {
                let place = room
                    .and_then(|r| plain_str(world, r, ROOM, "name"))
                    .unwrap_or("?");
                format!("{} ({n} is in {place})", ask(n, want))
            })
        })
        .unwrap_or_else(|| "story <a named creature> \"<a dozen words>\"".to_string());
    format!("Three safe first moves: look; {listener}; travel east to the Fogbank, where the wisps are mild. Nothing here is required.")
}

/// Whether a citizen's name is a passing one: forgotten an hour after they go quiet,
/// keeping nothing, obligating no one.
pub fn is_passing(world: &World, citizen: EntityId) -> bool {
    matches!(
        world
            .store
            .entity(citizen)
            .and_then(|e| e.components.get(CITIZEN))
            .and_then(|c| c.fields.get("passing")),
        Some(Field::Plain(Value::Bool(true)))
    )
}

/// The building and keeping verbs are a kept name's: a passing name is refused with
/// the way to keep it.
pub(crate) fn kept_only(world: &World, ctx: &CallContext, what: &str) -> Result<(), CommandError> {
    let f = me(world, ctx)?;
    if is_passing(world, f.citizen) {
        return Err(CommandError::precondition(format!(
            "A passing name does not {what}: nothing of its is kept. `keep` makes {} a kept name, yours until thirty days of silence, and then it can.",
            ctx.actor
        )));
    }
    Ok(())
}

/// Log in, or arrive for the first time; `passing` asks for a passing name, which a
/// name already kept ignores.
pub fn login_as(
    world: &mut World,
    actor: &str,
    origin: &str,
    passphrase: &str,
    passing: bool,
) -> Result<Found, LoginError> {
    let internal = |e: moondial_store::StoreError| LoginError::Internal(e.to_string());
    let me = Actor::Citizen(actor.to_string());
    let now = world.now();
    if let Some(f) = find_citizen(world, actor) {
        if let Some(minutes) = world.locked_for(actor) {
            return Err(LoginError::Locked {
                name: actor.to_string(),
                minutes,
            });
        }
        if let Err(e) = identity::check(world, f.citizen, passphrase) {
            if matches!(e, LoginError::WrongPassphrase { .. }) {
                world.failed_login(actor);
            }
            return Err(e);
        }
        world.succeeded_login(actor);
        if world
            .store
            .entity(f.citizen)
            .is_some_and(|e| !e.components.contains_key(SPRITE))
            || sprite_is_old(world, f.citizen)
        {
            world
                .record(
                    me.clone(),
                    StoreEvent::ComponentSet {
                        id: f.citizen,
                        component: SPRITE.into(),
                        fields: sprite_fields("@", "#e8e4da", "citizen"),
                    },
                )
                .map_err(internal)?;
        }
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
        // Logging in where something hunts is arriving, as far as it is concerned.
        if combat::engaged_with(world, f.citizen).is_none() {
            if let Some(room) = world.store.entity(f.citizen).and_then(|e| e.parent) {
                combat::on_arrival(world, actor, f.citizen, room).map_err(LoginError::Internal)?;
                building::visited(world, room).map_err(LoginError::Internal)?;
            }
        }
        world.arrive(actor, origin, false);
        // A known citizen in by a new door (the play page logs in over HTTP, then opens
        // its session): the room should say which.
        if origin != "command"
            && presence::is_here(world, f.citizen)
            && presence::door_field(world, f.citizen).as_deref() != Some(origin)
        {
            presence::mark_here(world, actor, f.citizen, origin).map_err(LoginError::Internal)?;
        }
        return Ok(f);
    }
    // A name is one name whatever its letters: claude-scout is Claude-scout's, not a
    // second citizen. Log in with the spelling the world knows.
    if let Some(taken) = canonical_name(world, actor) {
        return Err(LoginError::Taken { taken });
    }
    identity::acceptable(passphrase)?;
    let home = rooms::arrival(world)
        .ok_or_else(|| LoginError::Internal("the Threshold is missing".into()))?;
    let citizen = world.store.allocate_id();
    world
        .record(
            me.clone(),
            StoreEvent::EntityCreated {
                id: citizen,
                kind: "citizen".into(),
                owner: Some(actor.into()),
                parent: Some(home),
                scopes: vec![account_scope(actor)],
            },
        )
        .map_err(internal)?;
    let mut fields = BTreeMap::new();
    fields.insert("name".into(), Field::Plain(json!(actor)));
    fields.insert("origin".into(), Field::Plain(json!(origin)));
    fields.insert("passing".into(), Field::Plain(json!(passing)));
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
    world
        .record(
            me.clone(),
            StoreEvent::ComponentSet {
                id: citizen,
                component: SPRITE.into(),
                fields: sprite_fields("@", "#e8e4da", "citizen"),
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
    world.arrive(actor, origin, true);
    presence::mark_here(world, actor, citizen, origin).map_err(LoginError::Internal)?;
    talk::greet(world, actor, home);
    // The first thing a new citizen hears: that the name was free and is theirs, so a
    // second arrival under a name already taken reads differently from a first.
    combat::narrate(
        world,
        actor,
        if passing {
            format!("The name {actor} was no one's; it is yours for now, a passing name: the world forgets it an hour after you go quiet, or at once when you `leave`, and nothing done under it is kept. Nothing is asked of you. Everything you do under it is public all the same. `keep` would make it yours for longer.")
        } else {
            format!("The name {actor} was no one's; it is yours now, and answers to the passphrase you just gave. Keep that passphrase: nothing recovers it, and only its hash is stored, so a random one is as good as any. Everything you do under it is public, and a name unspoken for thirty days is forgotten.")
        },
    );
    combat::narrate(world, actor, first_moves(world));
    Ok(Found {
        citizen,
        window,
        palace,
    })
}

/// A sprite: one glyph and a tint, enough for a terminal renderer and a placeholder for
/// an atlas frame later.
pub(crate) fn sprite_fields(glyph: &str, tint: &str, frame: &str) -> BTreeMap<String, Field> {
    let mut fields = BTreeMap::new();
    fields.insert("glyph".into(), Field::Plain(json!(glyph)));
    fields.insert("tint".into(), Field::Plain(json!(tint)));
    fields.insert("atlas".into(), Field::Plain(json!(ATLAS)));
    fields.insert("frame".into(), Field::Plain(json!(frame)));
    fields
}

/// The atlas the slice's frames belong to. A client that has it draws the frame; one
/// that does not draws the glyph.
pub const ATLAS: &str = "disremembered-pixel-1";

/// Whether an entity's sprite is from before frames existed.
pub(crate) fn sprite_is_old(world: &World, id: EntityId) -> bool {
    plain_str(world, id, SPRITE, "frame").is_none()
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

/// Run a query the driver itself wrote, for commands that render from one.
pub(crate) fn observe_as(
    world: &World,
    ctx: &CallContext,
    query: Value,
) -> Result<crate::observe::Observed, CommandError> {
    let req: crate::observe::Request = serde_json::from_value(query)
        .map_err(|e| CommandError::internal(format!("bad built-in query: {e}")))?;
    observe_request(world, ctx, &req)
}

/// Read a query an actor wrote. A shape error is the actor's, and says what was expected.
pub(crate) fn parse_query(query: Value) -> Result<crate::observe::Request, CommandError> {
    serde_json::from_value(query).map_err(|e| {
        let e = e.to_string();
        let hint = if e.contains("Filter") {
            " A where is {\"field\": \"component.field\", \"op\": \"<\", \"value\": 5} (ops <, <=, ==, !=, >, >=), or {\"all\": [...]}, {\"any\": [...]}, {\"not\": ...}."
        } else if e.contains("Root") {
            " A root is \"self\", \"here\", \"window\", \"palace\", {\"room\": \"fogbank\"}, {\"window\": \"name\"} or {\"entity\": n}."
        } else {
            ""
        };
        CommandError::precondition(format!("The query is not the shape the world reads: {e}.{hint}"))
    })
}

/// Run a parsed observation as the calling actor.
pub(crate) fn observe_request(
    world: &World,
    ctx: &CallContext,
    req: &crate::observe::Request,
) -> Result<crate::observe::Observed, CommandError> {
    let viewer = if ctx.kind == moondial_registry::ActorKind::Admin {
        moondial_store::Viewer::admin()
    } else {
        moondial_store::Viewer::actor(&ctx.actor)
    };
    world
        .observe(&viewer, ctx.kind, req, &moondial_query::Limits::default())
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

/// Set some fields of a citizen's `disremembered.citizen` component, keeping the rest:
/// a ComponentSet replaces the whole component, so the change is merged first.
pub(crate) fn set_citizen_fields(
    world: &mut World,
    actor: &str,
    citizen: EntityId,
    changes: BTreeMap<String, Field>,
) -> Result<(), CommandError> {
    let mut fields = world
        .store
        .entity(citizen)
        .and_then(|e| e.components.get(CITIZEN))
        .map(|c| c.fields.clone())
        .unwrap_or_default();
    fields.extend(changes);
    world
        .record(
            Actor::Citizen(actor.to_string()),
            StoreEvent::ComponentSet {
                id: citizen,
                component: CITIZEN.into(),
                fields,
            },
        )
        .map(|_| ())
        .map_err(internal)
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
    memory::register(world, curves.clone());
    presence::register(world, curves.clone());
    mind::register(world);
    speech::register(world);
    talk::register(world);
    exchange::register(world, curves.clone());
    errand::register(world);
    cloak::register(world);
    building::register(world);
    system::register(world);
}

#[cfg(test)]
mod tests;
