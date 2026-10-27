//! Moondial entities and components.
//!
//! Everything in a world is an [`Entity`] with typed [`Component`] data attached. Modules
//! declare [`ComponentSchema`]s (versioned, with per-field [`Visibility`]) in a
//! [`Registry`]; the [`Store`] holds the entities, indexes them, and answers reads.
//!
//! Three things make this store Moondial's rather than a generic one:
//!
//! - **It is a projection over the event log.** The only way to change it is to apply a
//!   `store.*` [`Event`], and the driver's single mutation path ([`Store::record`]) appends
//!   the event first and applies it second. Replaying the log into an empty store yields
//!   the same store, by construction.
//! - **Time-evaluated fields are reconciled on read.** A field of kind [`FieldKind::Timed`]
//!   holds a [`TimedValue`]; reading it applies its curve to the active time since its
//!   waterline, with every clock scope that applies to the entity paused out. Scopes come
//!   from the entity, its containers (a chunk inside a paused account's window is paused),
//!   and the world.
//! - **Visibility is declared on the field, not checked in the query.** Every field is
//!   `public`, `party`, `owner` or `admin` in its schema, and [`Store::view`] applies that
//!   one rule per field. The observation query and the CLEAR gate both read through
//!   `view`, so permission filtering has one code path.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use moondial_log::{Actor, Event, EventLog, LogError, NewEvent, Projection, WorldId};
use moondial_time::{Clock, ClockError, Curve, TimedValue, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// An entity's identity within a world. Assigned by the driver at creation and recorded
/// in the creating event, so replay reproduces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntityId(pub u64);

/// A clock scope's name: `world`, `account:spacepants`, `zone:acid-cave`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClockId(pub String);

impl From<&str> for ClockId {
    fn from(s: &str) -> Self {
        ClockId(s.to_string())
    }
}

/// Who may see a field. Declared in the schema; applied by [`Store::view`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Anyone who can observe the entity.
    Public,
    /// The owner and anyone in the owner's party.
    Party,
    /// The owner only.
    Owner,
    /// Administrators only.
    Admin,
}

/// Whether a field is a plain value or a time-evaluated one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    /// Any JSON value, set directly.
    Plain,
    /// A [`TimedValue`]: committed, recurved, and read lazily.
    Timed,
}

/// One field of a component schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldSchema {
    /// Field name.
    pub name: String,
    /// Who may see it.
    pub visibility: Visibility,
    /// Plain or timed.
    pub kind: FieldKind,
}

/// A component schema as a module registers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentSchema {
    /// Component name, namespaced by module: `entropic.chunk`.
    pub name: String,
    /// Schema version; migrations run between versions.
    pub version: u32,
    /// The fields, in declaration order.
    pub fields: Vec<FieldSchema>,
}

impl ComponentSchema {
    /// Build a schema.
    pub fn new(name: impl Into<String>, version: u32) -> Self {
        ComponentSchema {
            name: name.into(),
            version,
            fields: Vec::new(),
        }
    }
    /// Add a field.
    pub fn field(
        mut self,
        name: impl Into<String>,
        kind: FieldKind,
        visibility: Visibility,
    ) -> Self {
        self.fields.push(FieldSchema {
            name: name.into(),
            visibility,
            kind,
        });
        self
    }
    /// Look a field up.
    pub fn get(&self, field: &str) -> Option<&FieldSchema> {
        self.fields.iter().find(|f| f.name == field)
    }
}

/// How a relation is followed from an entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "how", rename_all = "snake_case")]
pub enum RelationKind {
    /// The entities inside this one (`parent == self`), optionally only some kinds.
    Children {
        /// Entity kinds to include; empty means all.
        #[serde(default)]
        kinds: Vec<String>,
    },
    /// The entity this one is inside.
    Parent,
    /// Entity ids held in a plain field: a single id, an array of ids, or a map of
    /// label to id (an exit name to a room, say). The field's own visibility applies, so
    /// a relation through a hidden field is empty for whoever cannot see the field.
    Field {
        /// The component.
        component: String,
        /// The field.
        field: String,
    },
}

/// A relation a module declares, so a client can ask for it by name in an observation
/// query and discover it from the registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationSchema {
    /// Relation name: `contents`, `in`, `exits`.
    pub name: String,
    /// What it means, for the registry listing.
    pub description: String,
    /// How to follow it.
    #[serde(flatten)]
    pub kind: RelationKind,
}

/// The component schemas and relations a world knows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    components: BTreeMap<String, ComponentSchema>,
    #[serde(default)]
    relations: BTreeMap<String, RelationSchema>,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }
    /// Register (or replace) a schema.
    pub fn register(&mut self, schema: ComponentSchema) {
        self.components.insert(schema.name.clone(), schema);
    }
    /// Look a schema up.
    pub fn get(&self, name: &str) -> Option<&ComponentSchema> {
        self.components.get(name)
    }
    /// All registered schemas.
    pub fn schemas(&self) -> impl Iterator<Item = &ComponentSchema> {
        self.components.values()
    }
    /// Register (or replace) a relation.
    pub fn register_relation(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        kind: RelationKind,
    ) {
        let name = name.into();
        self.relations.insert(
            name.clone(),
            RelationSchema {
                name,
                description: description.into(),
                kind,
            },
        );
    }
    /// Look a relation up.
    pub fn relation(&self, name: &str) -> Option<&RelationSchema> {
        self.relations.get(name)
    }
    /// All registered relations.
    pub fn relations(&self) -> impl Iterator<Item = &RelationSchema> {
        self.relations.values()
    }
}

/// A stored field value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Field {
    /// A plain JSON value.
    Plain(Value),
    /// A time-evaluated value.
    Timed(TimedValue),
}

/// A component instance on an entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Component {
    /// The schema version the data was written under.
    pub version: u32,
    /// Field values by name.
    pub fields: BTreeMap<String, Field>,
}

/// A thing in the world.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    /// Identity.
    pub id: EntityId,
    /// What kind of thing: `citizen`, `room`, `chunk`, `pedestal`. A label modules agree on.
    pub kind: String,
    /// The actor that owns it, for `owner` and `party` visibility.
    pub owner: Option<String>,
    /// The entity this one is inside, if any (a chunk in a window, an item in a room).
    pub parent: Option<EntityId>,
    /// Clock scopes that apply to this entity directly. Ancestors' scopes apply too.
    pub scopes: Vec<ClockId>,
    /// Components by schema name.
    pub components: BTreeMap<String, Component>,
}

/// How a timed field appears in a view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimedForm {
    /// The evaluated number at `now`.
    #[default]
    Value,
    /// `{value, waterline, curve, bounds, paused}`: enough to evaluate it later.
    Curve,
}

/// Who is looking, for [`Store::view`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Viewer {
    /// The viewing actor, if any.
    pub actor: Option<String>,
    /// Actors in the viewer's party (the viewer may include themselves or not).
    pub party: BTreeSet<String>,
    /// Administrators see every field.
    pub admin: bool,
}

impl Viewer {
    /// An anonymous viewer: public fields only.
    pub fn anonymous() -> Self {
        Self::default()
    }
    /// A viewer acting as `actor`.
    pub fn actor(actor: impl Into<String>) -> Self {
        Viewer {
            actor: Some(actor.into()),
            ..Default::default()
        }
    }
    /// An administrator.
    pub fn admin() -> Self {
        Viewer {
            admin: true,
            ..Default::default()
        }
    }
    fn can_see(&self, vis: Visibility, owner: Option<&str>) -> bool {
        if self.admin {
            return true;
        }
        match vis {
            Visibility::Public => true,
            Visibility::Admin => false,
            Visibility::Owner => owner.is_some() && owner == self.actor.as_deref(),
            Visibility::Party => match owner {
                None => false,
                Some(o) => self.actor.as_deref() == Some(o) || self.party.contains(o),
            },
        }
    }
}

/// Errors from store operations.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum StoreError {
    /// No such entity.
    #[error("unknown entity {0:?}")]
    UnknownEntity(EntityId),
    /// The entity id is already in use.
    #[error("entity {0:?} already exists")]
    DuplicateEntity(EntityId),
    /// No schema registered under that name.
    #[error("unknown component schema {0}")]
    UnknownComponent(String),
    /// The entity has no such component.
    #[error("entity {0:?} has no component {1}")]
    MissingComponent(EntityId, String),
    /// The schema has no such field.
    #[error("component {0} has no field {1}")]
    UnknownField(String, String),
    /// A plain operation on a timed field or the reverse.
    #[error("field {0}.{1} is {2:?}, not {3:?}")]
    WrongKind(String, String, FieldKind, FieldKind),
    /// Moving an entity into itself or its own descendant.
    #[error("moving {0:?} under {1:?} would make a cycle")]
    Cycle(EntityId, EntityId),
    /// A clock operation failed.
    #[error("clock {0:?}: {1}")]
    Clock(ClockId, ClockError),
    /// The event payload was not a store event.
    #[error("malformed store event: {0}")]
    Malformed(String),
    /// Appending to the log failed.
    #[error("log: {0}")]
    Log(LogError),
}

/// The store's own events: the only mutations it accepts. Recorded with kind
/// `store.<op>` and this struct as the payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum StoreEvent {
    /// A new entity.
    EntityCreated {
        /// Its id, allocated by the driver.
        id: EntityId,
        /// Its kind.
        kind: String,
        /// Its owner.
        owner: Option<String>,
        /// Its container.
        parent: Option<EntityId>,
        /// Its direct clock scopes.
        scopes: Vec<ClockId>,
    },
    /// An entity and everything inside it is gone.
    EntityRemoved {
        /// Which.
        id: EntityId,
    },
    /// An entity changed container.
    EntityMoved {
        /// Which.
        id: EntityId,
        /// To where (`None` = top level).
        parent: Option<EntityId>,
    },
    /// A component was set whole (created or replaced).
    ComponentSet {
        /// On which entity.
        id: EntityId,
        /// Which component.
        component: String,
        /// Its field values.
        fields: BTreeMap<String, Field>,
    },
    /// A plain field changed.
    FieldSet {
        /// On which entity.
        id: EntityId,
        /// Which component.
        component: String,
        /// Which field.
        field: String,
        /// The new value.
        value: Value,
    },
    /// A timed field was committed at the event's timestamp.
    FieldCommitted {
        /// On which entity.
        id: EntityId,
        /// Which component.
        component: String,
        /// Which field.
        field: String,
        /// The committed value.
        value: f64,
    },
    /// A timed field changed curve at the event's timestamp, anchoring its current value.
    FieldRecurved {
        /// On which entity.
        id: EntityId,
        /// Which component.
        component: String,
        /// Which field.
        field: String,
        /// The new curve.
        curve: Curve,
    },
    /// A clock scope paused at the event's timestamp.
    ClockPaused {
        /// Which scope.
        clock: ClockId,
    },
    /// A clock scope resumed at the event's timestamp.
    ClockResumed {
        /// Which scope.
        clock: ClockId,
    },
}

impl StoreEvent {
    /// The event kind this store event is recorded under.
    pub fn kind(&self) -> &'static str {
        match self {
            StoreEvent::EntityCreated { .. } => "store.entity_created",
            StoreEvent::EntityRemoved { .. } => "store.entity_removed",
            StoreEvent::EntityMoved { .. } => "store.entity_moved",
            StoreEvent::ComponentSet { .. } => "store.component_set",
            StoreEvent::FieldSet { .. } => "store.field_set",
            StoreEvent::FieldCommitted { .. } => "store.field_committed",
            StoreEvent::FieldRecurved { .. } => "store.field_recurved",
            StoreEvent::ClockPaused { .. } => "store.clock_paused",
            StoreEvent::ClockResumed { .. } => "store.clock_resumed",
        }
    }
}

/// The world's clock scope name; every entity is in it.
pub const WORLD_CLOCK: &str = "world";

/// Entities, components, clocks, and the registry, for one world.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Store {
    registry: Registry,
    entities: BTreeMap<EntityId, Entity>,
    clocks: BTreeMap<ClockId, Clock>,
    next_id: u64,
}

impl Store {
    /// An empty store with the given schemas.
    pub fn new(registry: Registry) -> Self {
        Store {
            registry,
            entities: BTreeMap::new(),
            clocks: BTreeMap::new(),
            next_id: 1,
        }
    }

    /// The schemas.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Register a schema. Modules do this at load.
    pub fn register(&mut self, schema: ComponentSchema) {
        self.registry.register(schema);
    }

    /// The next unused entity id. The driver takes one of these when it creates an
    /// entity and records it in the event.
    pub fn allocate_id(&mut self) -> EntityId {
        let id = EntityId(self.next_id);
        self.next_id += 1;
        id
    }

    /// An entity by id.
    pub fn entity(&self, id: EntityId) -> Option<&Entity> {
        self.entities.get(&id)
    }

    /// All entities, in id order.
    pub fn entities(&self) -> impl Iterator<Item = &Entity> {
        self.entities.values()
    }

    /// The entities directly inside `parent`.
    pub fn children(&self, parent: EntityId) -> impl Iterator<Item = &Entity> {
        self.entities
            .values()
            .filter(move |e| e.parent == Some(parent))
    }

    /// The clock for a scope, if it has ever been paused.
    pub fn clock(&self, id: &ClockId) -> Option<&Clock> {
        self.clocks.get(id)
    }

    /// The clocks that apply to an entity: the world's, its own, and its ancestors'.
    pub fn scopes_for(&self, id: EntityId) -> Result<Vec<Clock>, StoreError> {
        let mut names: BTreeSet<ClockId> = BTreeSet::new();
        names.insert(ClockId::from(WORLD_CLOCK));
        let mut cur = Some(id);
        let mut hops = 0;
        while let Some(eid) = cur {
            let e = self
                .entities
                .get(&eid)
                .ok_or(StoreError::UnknownEntity(eid))?;
            names.extend(e.scopes.iter().cloned());
            cur = e.parent;
            hops += 1;
            if hops > 10_000 {
                break; // defensive: cycles are prevented on move, but never loop forever
            }
        }
        Ok(names
            .iter()
            .filter_map(|n| self.clocks.get(n).cloned())
            .collect())
    }

    /// Read one field at `now`. Timed fields are reconciled against the entity's scopes.
    /// Visibility is not applied here; see [`Store::view`].
    pub fn read_field(
        &self,
        id: EntityId,
        component: &str,
        field: &str,
        now: Timestamp,
    ) -> Result<Value, StoreError> {
        let e = self
            .entities
            .get(&id)
            .ok_or(StoreError::UnknownEntity(id))?;
        let c = e
            .components
            .get(component)
            .ok_or_else(|| StoreError::MissingComponent(id, component.into()))?;
        let f = c
            .fields
            .get(field)
            .ok_or_else(|| StoreError::UnknownField(component.into(), field.into()))?;
        Ok(match f {
            Field::Plain(v) => v.clone(),
            Field::Timed(t) => Value::from(t.read_at(now, &self.scopes_for(id)?)),
        })
    }

    /// The timed field itself, for callers that need the waterline or curve.
    pub fn timed_field(
        &self,
        id: EntityId,
        component: &str,
        field: &str,
    ) -> Result<&TimedValue, StoreError> {
        let e = self
            .entities
            .get(&id)
            .ok_or(StoreError::UnknownEntity(id))?;
        let c = e
            .components
            .get(component)
            .ok_or_else(|| StoreError::MissingComponent(id, component.into()))?;
        match c.fields.get(field) {
            Some(Field::Timed(t)) => Ok(t),
            Some(Field::Plain(_)) => Err(StoreError::WrongKind(
                component.into(),
                field.into(),
                FieldKind::Plain,
                FieldKind::Timed,
            )),
            None => Err(StoreError::UnknownField(component.into(), field.into())),
        }
    }

    /// An entity as `viewer` may see it at `now`: its kind, owner, parent, and every
    /// component's visible fields, timed ones evaluated. `None` if the entity does not exist.
    pub fn view(&self, id: EntityId, viewer: &Viewer, now: Timestamp) -> Option<Value> {
        self.view_as(id, viewer, now, TimedForm::Value)
    }

    /// [`Store::view`] with a choice of how timed fields appear: the evaluated number,
    /// or the curve itself so a client can evaluate it at any moment without asking
    /// again. A renderer animates decay from the curve at frame rate; the number is
    /// what a text client or a filter wants.
    pub fn view_as(
        &self,
        id: EntityId,
        viewer: &Viewer,
        now: Timestamp,
        timed: TimedForm,
    ) -> Option<Value> {
        let e = self.entities.get(&id)?;
        let scopes = self.scopes_for(id).ok()?;
        let paused = scopes.iter().any(|c| c.is_paused_at(now));
        let mut comps = serde_json::Map::new();
        for (name, c) in &e.components {
            let Some(schema) = self.registry.get(name) else {
                continue;
            };
            let mut fields = serde_json::Map::new();
            for (fname, f) in &c.fields {
                let Some(fs) = schema.get(fname) else {
                    continue;
                };
                if !viewer.can_see(fs.visibility, e.owner.as_deref()) {
                    continue;
                }
                let v = match (f, timed) {
                    (Field::Plain(v), _) => v.clone(),
                    (Field::Timed(t), TimedForm::Value) => Value::from(t.read_at(now, &scopes)),
                    (Field::Timed(t), TimedForm::Curve) => serde_json::json!({
                        "value": t.read_at(now, &scopes),
                        "waterline": {"value": t.waterline.value, "at": t.waterline.at.0},
                        "curve": t.curve,
                        "bounds": t.bounds,
                        "paused": paused,
                    }),
                };
                fields.insert(fname.clone(), v);
            }
            if !fields.is_empty() {
                comps.insert(name.clone(), Value::Object(fields));
            }
        }
        let mut out = serde_json::Map::new();
        out.insert("id".into(), Value::from(e.id.0));
        out.insert("kind".into(), Value::from(e.kind.clone()));
        if viewer.admin || viewer.actor.is_some() {
            out.insert(
                "owner".into(),
                e.owner.clone().map(Value::from).unwrap_or(Value::Null),
            );
        }
        out.insert(
            "parent".into(),
            e.parent.map(|p| Value::from(p.0)).unwrap_or(Value::Null),
        );
        out.insert("components".into(), Value::Object(comps));
        Some(Value::Object(out))
    }

    /// The driver's single mutation path: append the event to the log, then apply it.
    /// If the log refuses, nothing changes. If the store refuses, the event is already in
    /// the log, which is the correct record of what the driver tried; callers treat that
    /// as a bug, not a runtime condition, because validation happens before `record`.
    pub fn record(
        &mut self,
        log: &mut dyn EventLog,
        world: &WorldId,
        at: Timestamp,
        actor: Actor,
        event: StoreEvent,
    ) -> Result<Event, StoreError> {
        let payload =
            serde_json::to_value(&event).map_err(|e| StoreError::Malformed(e.to_string()))?;
        let ev = log
            .append(world, NewEvent::new(at, event.kind(), actor, payload))
            .map_err(StoreError::Log)?;
        self.apply_store_event(ev.at, &event)?;
        Ok(ev)
    }

    /// Apply a store event at a timestamp. Used by [`Projection::apply`] and [`Store::record`].
    pub fn apply_store_event(
        &mut self,
        at: Timestamp,
        event: &StoreEvent,
    ) -> Result<(), StoreError> {
        match event {
            StoreEvent::EntityCreated {
                id,
                kind,
                owner,
                parent,
                scopes,
            } => {
                if self.entities.contains_key(id) {
                    return Err(StoreError::DuplicateEntity(*id));
                }
                if let Some(p) = parent {
                    if !self.entities.contains_key(p) {
                        return Err(StoreError::UnknownEntity(*p));
                    }
                }
                self.entities.insert(
                    *id,
                    Entity {
                        id: *id,
                        kind: kind.clone(),
                        owner: owner.clone(),
                        parent: *parent,
                        scopes: scopes.clone(),
                        components: BTreeMap::new(),
                    },
                );
                self.next_id = self.next_id.max(id.0 + 1);
            }
            StoreEvent::EntityRemoved { id } => {
                if !self.entities.contains_key(id) {
                    return Err(StoreError::UnknownEntity(*id));
                }
                let mut doomed = vec![*id];
                let mut i = 0;
                while i < doomed.len() {
                    let p = doomed[i];
                    doomed.extend(
                        self.entities
                            .values()
                            .filter(|e| e.parent == Some(p))
                            .map(|e| e.id),
                    );
                    i += 1;
                }
                for d in doomed {
                    self.entities.remove(&d);
                }
            }
            StoreEvent::EntityMoved { id, parent } => {
                if !self.entities.contains_key(id) {
                    return Err(StoreError::UnknownEntity(*id));
                }
                if let Some(p) = parent {
                    if !self.entities.contains_key(p) {
                        return Err(StoreError::UnknownEntity(*p));
                    }
                    let mut cur = Some(*p);
                    while let Some(c) = cur {
                        if c == *id {
                            return Err(StoreError::Cycle(*id, *p));
                        }
                        cur = self.entities.get(&c).and_then(|e| e.parent);
                    }
                }
                self.entities.get_mut(id).expect("checked").parent = *parent;
            }
            StoreEvent::ComponentSet {
                id,
                component,
                fields,
            } => {
                let schema = self
                    .registry
                    .get(component)
                    .ok_or_else(|| StoreError::UnknownComponent(component.clone()))?;
                for (fname, f) in fields {
                    let fs = schema.get(fname).ok_or_else(|| {
                        StoreError::UnknownField(component.clone(), fname.clone())
                    })?;
                    let actual = match f {
                        Field::Plain(_) => FieldKind::Plain,
                        Field::Timed(_) => FieldKind::Timed,
                    };
                    if actual != fs.kind {
                        return Err(StoreError::WrongKind(
                            component.clone(),
                            fname.clone(),
                            fs.kind,
                            actual,
                        ));
                    }
                }
                let version = schema.version;
                let e = self
                    .entities
                    .get_mut(id)
                    .ok_or(StoreError::UnknownEntity(*id))?;
                e.components.insert(
                    component.clone(),
                    Component {
                        version,
                        fields: fields.clone(),
                    },
                );
            }
            StoreEvent::FieldSet {
                id,
                component,
                field,
                value,
            } => {
                let fs = self.field_schema(component, field)?;
                if fs.kind != FieldKind::Plain {
                    return Err(StoreError::WrongKind(
                        component.clone(),
                        field.clone(),
                        fs.kind,
                        FieldKind::Plain,
                    ));
                }
                let c = self.component_mut(*id, component)?;
                c.fields.insert(field.clone(), Field::Plain(value.clone()));
            }
            StoreEvent::FieldCommitted {
                id,
                component,
                field,
                value,
            } => {
                let fs = self.field_schema(component, field)?;
                if fs.kind != FieldKind::Timed {
                    return Err(StoreError::WrongKind(
                        component.clone(),
                        field.clone(),
                        fs.kind,
                        FieldKind::Timed,
                    ));
                }
                let c = self.component_mut(*id, component)?;
                match c.fields.get_mut(field) {
                    Some(Field::Timed(t)) => t.commit(*value, at),
                    Some(Field::Plain(_)) => unreachable!("schema says timed"),
                    None => return Err(StoreError::UnknownField(component.clone(), field.clone())),
                }
            }
            StoreEvent::FieldRecurved {
                id,
                component,
                field,
                curve,
            } => {
                let fs = self.field_schema(component, field)?;
                if fs.kind != FieldKind::Timed {
                    return Err(StoreError::WrongKind(
                        component.clone(),
                        field.clone(),
                        fs.kind,
                        FieldKind::Timed,
                    ));
                }
                let scopes = self.scopes_for(*id)?;
                let c = self.component_mut(*id, component)?;
                match c.fields.get_mut(field) {
                    Some(Field::Timed(t)) => t.recurve(curve.clone(), at, &scopes),
                    Some(Field::Plain(_)) => unreachable!("schema says timed"),
                    None => return Err(StoreError::UnknownField(component.clone(), field.clone())),
                }
            }
            StoreEvent::ClockPaused { clock } => {
                let c = self.clocks.entry(clock.clone()).or_default();
                c.pause(at)
                    .map_err(|e| StoreError::Clock(clock.clone(), e))?;
            }
            StoreEvent::ClockResumed { clock } => {
                let c = self.clocks.entry(clock.clone()).or_default();
                c.resume(at)
                    .map_err(|e| StoreError::Clock(clock.clone(), e))?;
            }
        }
        Ok(())
    }

    fn field_schema(&self, component: &str, field: &str) -> Result<FieldSchema, StoreError> {
        let schema = self
            .registry
            .get(component)
            .ok_or_else(|| StoreError::UnknownComponent(component.into()))?;
        schema
            .get(field)
            .cloned()
            .ok_or_else(|| StoreError::UnknownField(component.into(), field.into()))
    }

    fn component_mut(
        &mut self,
        id: EntityId,
        component: &str,
    ) -> Result<&mut Component, StoreError> {
        let e = self
            .entities
            .get_mut(&id)
            .ok_or(StoreError::UnknownEntity(id))?;
        e.components
            .get_mut(component)
            .ok_or_else(|| StoreError::MissingComponent(id, component.into()))
    }
}

impl Projection for Store {
    type Error = StoreError;
    fn apply(&mut self, event: &Event) -> Result<(), StoreError> {
        if !event.kind.starts_with("store.") {
            return Ok(());
        }
        let se: StoreEvent = serde_json::from_value(event.payload.clone())
            .map_err(|e| StoreError::Malformed(e.to_string()))?;
        self.apply_store_event(event.at, &se)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moondial_log::{replay, Cursor, MemoryLog};
    use moondial_time::Duration;
    use serde_json::json;

    const T0: Timestamp = Timestamp(1_700_000_000_000);
    fn at(ms: i64) -> Timestamp {
        Timestamp(T0.0 + ms)
    }

    fn registry() -> Registry {
        let mut r = Registry::new();
        r.register(
            ComponentSchema::new("test.chunk", 1)
                .field("value", FieldKind::Timed, Visibility::Owner)
                .field("source", FieldKind::Plain, Visibility::Public)
                .field("note", FieldKind::Plain, Visibility::Party)
                .field("flag", FieldKind::Plain, Visibility::Admin),
        );
        r
    }

    fn chunk_fields(v0: f64, at: Timestamp) -> BTreeMap<String, Field> {
        let mut f = BTreeMap::new();
        f.insert(
            "value".into(),
            Field::Timed(TimedValue::new(
                v0,
                at,
                Curve::Exponential {
                    half_life: Duration::days(1),
                },
            )),
        );
        f.insert("source".into(), Field::Plain(json!("hydra")));
        f.insert("note".into(), Field::Plain(json!("shared with party")));
        f.insert("flag".into(), Field::Plain(json!(true)));
        f
    }

    #[test]
    fn view_applies_visibility_per_field() {
        let mut s = Store::new(registry());
        let id = s.allocate_id();
        s.apply_store_event(
            T0,
            &StoreEvent::EntityCreated {
                id,
                kind: "chunk".into(),
                owner: Some("dave".into()),
                parent: None,
                scopes: vec![],
            },
        )
        .unwrap();
        s.apply_store_event(
            T0,
            &StoreEvent::ComponentSet {
                id,
                component: "test.chunk".into(),
                fields: chunk_fields(100.0, T0),
            },
        )
        .unwrap();
        let fields = |v: &Viewer| -> Vec<String> {
            let view = s.view(id, v, T0).unwrap();
            view["components"]["test.chunk"]
                .as_object()
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default()
        };
        assert_eq!(fields(&Viewer::anonymous()), vec!["source"]);
        assert_eq!(
            fields(&Viewer::actor("dave")),
            vec!["note", "source", "value"]
        );
        let mut friend = Viewer::actor("tyler");
        friend.party.insert("dave".into());
        assert_eq!(fields(&friend), vec!["note", "source"]);
        assert_eq!(fields(&Viewer::actor("stranger")), vec!["source"]);
        assert_eq!(
            fields(&Viewer::admin()),
            vec!["flag", "note", "source", "value"]
        );
    }

    #[test]
    fn containment_carries_clock_scopes() {
        let mut s = Store::new(registry());
        let window = s.allocate_id();
        let chunk = s.allocate_id();
        s.apply_store_event(
            T0,
            &StoreEvent::EntityCreated {
                id: window,
                kind: "window".into(),
                owner: Some("dave".into()),
                parent: None,
                scopes: vec!["account:dave".into()],
            },
        )
        .unwrap();
        s.apply_store_event(
            T0,
            &StoreEvent::EntityCreated {
                id: chunk,
                kind: "chunk".into(),
                owner: Some("dave".into()),
                parent: Some(window),
                scopes: vec![],
            },
        )
        .unwrap();
        s.apply_store_event(
            T0,
            &StoreEvent::ComponentSet {
                id: chunk,
                component: "test.chunk".into(),
                fields: chunk_fields(100.0, T0),
            },
        )
        .unwrap();
        // Pause the account for a day, starting after 12 h.
        s.apply_store_event(
            at(Duration::hours(12).0),
            &StoreEvent::ClockPaused {
                clock: "account:dave".into(),
            },
        )
        .unwrap();
        s.apply_store_event(
            at(Duration::hours(36).0),
            &StoreEvent::ClockResumed {
                clock: "account:dave".into(),
            },
        )
        .unwrap();
        // At 48 h, only 24 h were active: one half-life.
        let v = s
            .read_field(chunk, "test.chunk", "value", at(Duration::hours(48).0))
            .unwrap();
        assert!((v.as_f64().unwrap() - 50.0).abs() < 1e-9);
        // The world clock applies to everything.
        s.apply_store_event(
            at(Duration::hours(48).0),
            &StoreEvent::ClockPaused {
                clock: WORLD_CLOCK.into(),
            },
        )
        .unwrap();
        let later = s
            .read_field(chunk, "test.chunk", "value", at(Duration::hours(900).0))
            .unwrap();
        assert!((later.as_f64().unwrap() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn moves_reject_cycles_and_removal_cascades() {
        let mut s = Store::new(registry());
        let a = s.allocate_id();
        let b = s.allocate_id();
        let c = s.allocate_id();
        for (id, parent) in [(a, None), (b, Some(a)), (c, Some(b))] {
            s.apply_store_event(
                T0,
                &StoreEvent::EntityCreated {
                    id,
                    kind: "box".into(),
                    owner: None,
                    parent,
                    scopes: vec![],
                },
            )
            .unwrap();
        }
        assert_eq!(
            s.apply_store_event(
                T0,
                &StoreEvent::EntityMoved {
                    id: a,
                    parent: Some(c)
                }
            ),
            Err(StoreError::Cycle(a, c))
        );
        assert_eq!(
            s.apply_store_event(
                T0,
                &StoreEvent::EntityMoved {
                    id: a,
                    parent: Some(a)
                }
            ),
            Err(StoreError::Cycle(a, a))
        );
        s.apply_store_event(
            T0,
            &StoreEvent::EntityMoved {
                id: c,
                parent: None,
            },
        )
        .unwrap();
        s.apply_store_event(T0, &StoreEvent::EntityRemoved { id: a })
            .unwrap();
        assert!(s.entity(a).is_none() && s.entity(b).is_none() && s.entity(c).is_some());
    }

    #[test]
    fn schema_is_enforced() {
        let mut s = Store::new(registry());
        let id = s.allocate_id();
        s.apply_store_event(
            T0,
            &StoreEvent::EntityCreated {
                id,
                kind: "chunk".into(),
                owner: None,
                parent: None,
                scopes: vec![],
            },
        )
        .unwrap();
        assert_eq!(
            s.apply_store_event(
                T0,
                &StoreEvent::ComponentSet {
                    id,
                    component: "nope".into(),
                    fields: BTreeMap::new()
                }
            ),
            Err(StoreError::UnknownComponent("nope".into()))
        );
        let mut bad = BTreeMap::new();
        bad.insert("value".into(), Field::Plain(json!(1)));
        assert_eq!(
            s.apply_store_event(
                T0,
                &StoreEvent::ComponentSet {
                    id,
                    component: "test.chunk".into(),
                    fields: bad
                }
            ),
            Err(StoreError::WrongKind(
                "test.chunk".into(),
                "value".into(),
                FieldKind::Timed,
                FieldKind::Plain
            ))
        );
        s.apply_store_event(
            T0,
            &StoreEvent::ComponentSet {
                id,
                component: "test.chunk".into(),
                fields: chunk_fields(1.0, T0),
            },
        )
        .unwrap();
        assert_eq!(
            s.apply_store_event(
                T0,
                &StoreEvent::FieldSet {
                    id,
                    component: "test.chunk".into(),
                    field: "value".into(),
                    value: json!(2)
                }
            ),
            Err(StoreError::WrongKind(
                "test.chunk".into(),
                "value".into(),
                FieldKind::Timed,
                FieldKind::Plain
            ))
        );
    }

    #[test]
    fn record_appends_then_applies_and_replay_rebuilds_the_store() {
        let mut log = MemoryLog::new();
        let world = WorldId::from("test");
        let mut s = Store::new(registry());
        let window = s.allocate_id();
        let chunk = s.allocate_id();
        let me = Actor::Citizen("dave".into());
        s.record(
            &mut log,
            &world,
            T0,
            me.clone(),
            StoreEvent::EntityCreated {
                id: window,
                kind: "window".into(),
                owner: Some("dave".into()),
                parent: None,
                scopes: vec!["account:dave".into()],
            },
        )
        .unwrap();
        s.record(
            &mut log,
            &world,
            T0,
            me.clone(),
            StoreEvent::EntityCreated {
                id: chunk,
                kind: "chunk".into(),
                owner: Some("dave".into()),
                parent: Some(window),
                scopes: vec![],
            },
        )
        .unwrap();
        s.record(
            &mut log,
            &world,
            T0,
            me.clone(),
            StoreEvent::ComponentSet {
                id: chunk,
                component: "test.chunk".into(),
                fields: chunk_fields(100.0, T0),
            },
        )
        .unwrap();
        s.record(
            &mut log,
            &world,
            at(1000),
            Actor::Driver,
            StoreEvent::ClockPaused {
                clock: "account:dave".into(),
            },
        )
        .unwrap();
        s.record(
            &mut log,
            &world,
            at(Duration::hours(10).0),
            Actor::Driver,
            StoreEvent::ClockResumed {
                clock: "account:dave".into(),
            },
        )
        .unwrap();
        s.record(
            &mut log,
            &world,
            at(Duration::hours(20).0),
            me.clone(),
            StoreEvent::FieldRecurved {
                id: chunk,
                component: "test.chunk".into(),
                field: "value".into(),
                curve: Curve::PowerLaw {
                    scale: Duration::days(3),
                    exponent: 1.2,
                },
            },
        )
        .unwrap();
        s.record(
            &mut log,
            &world,
            at(Duration::hours(30).0),
            me,
            StoreEvent::FieldSet {
                id: chunk,
                component: "test.chunk".into(),
                field: "source".into(),
                value: json!("void lich"),
            },
        )
        .unwrap();
        assert_eq!(log.head(&world).unwrap().map(|s| s.0), Some(7));

        let mut rebuilt = Store::new(registry());
        replay(&log, &world, Cursor::START, &mut rebuilt).unwrap();
        assert_eq!(rebuilt, s);
        let now = at(Duration::days(5).0);
        let a = s.read_field(chunk, "test.chunk", "value", now).unwrap();
        let b = rebuilt
            .read_field(chunk, "test.chunk", "value", now)
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(rebuilt.allocate_id(), EntityId(3));
    }
}
