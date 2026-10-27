//! The Moondial observation query.
//!
//! A client names a root and a projection: which components and fields it wants, which
//! relations to follow, to what depth. The reply is exactly those fields, as this actor
//! may see them, with timed values evaluated at one moment. Three decisions keep the
//! query from becoming a security hole or a second rules engine:
//!
//! - **Visibility is applied by the store, not here.** The engine reads every entity
//!   through [`Store::view`], which applies the per-field visibility each component
//!   schema declares. The query only *narrows* that view. Filters run on the narrowed
//!   view too, so a hidden field can never be probed by filtering on it.
//! - **Cost is bounded before execution.** Depth is checked against the document before
//!   anything runs; breadth and the total entity count are checked as the walk proceeds
//!   and a query over the limit is rejected whole, never truncated.
//! - **Queries are pure reads.** Nothing here can change state or run module code.
//!
//! The query is JSON, so every client and every agent builds it the same way, and the
//! CLEAR gate's question patterns compile to it. Roots other than an entity id
//! (`self`, `here`, a palace) are the world's business: the driver resolves them through
//! a [`RootResolver`].
//!
//! ```json
//! {
//!   "root": "here",
//!   "select": { "disremembered.room": ["name", "description"], "disremembered.exits": "*" },
//!   "relations": {
//!     "contents": { "kinds": ["citizen", "creature"], "select": { "disremembered.creature": ["name", "hp"] } },
//!     "exits": { "select": { "disremembered.room": ["name"] } }
//!   }
//! }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use moondial_store::{EntityId, RelationKind, Store, TimedForm, Viewer};
use moondial_time::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// Where a query starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Root {
    /// An entity by id.
    Entity(EntityId),
    /// A name the world resolves: `"self"`, `"here"`, or `{"palace": "dave"}`.
    Named {
        /// The name.
        name: String,
        /// Its argument, for the object form.
        arg: Option<String>,
    },
}

impl Serialize for Root {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Root::Entity(id) => json!({ "entity": id.0 }).serialize(s),
            Root::Named { name, arg: None } => name.serialize(s),
            Root::Named { name, arg: Some(a) } => json!({ name: a }).serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Root {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        match v {
            Value::String(name) => Ok(Root::Named { name, arg: None }),
            Value::Number(n) => n
                .as_u64()
                .map(|id| Root::Entity(EntityId(id)))
                .ok_or_else(|| serde::de::Error::custom("root: entity id must be a non-negative integer")),
            Value::Object(m) if m.len() == 1 => {
                let (k, v) = m.into_iter().next().expect("one entry");
                if k == "entity" {
                    v.as_u64()
                        .map(|id| Root::Entity(EntityId(id)))
                        .ok_or_else(|| serde::de::Error::custom("root: entity id must be a non-negative integer"))
                } else {
                    let arg = match v {
                        Value::String(s) => Some(s),
                        Value::Number(n) => Some(n.to_string()),
                        Value::Null => None,
                        _ => return Err(serde::de::Error::custom("root: the argument must be a string")),
                    };
                    Ok(Root::Named { name: k, arg })
                }
            }
            _ => Err(serde::de::Error::custom(
                "root: a name (\"self\", \"here\"), an id ({\"entity\": 12}), or {\"<name>\": \"<arg>\"}",
            )),
        }
    }
}

/// Which fields of a component to return.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FieldSelect {
    /// Every field the viewer may see: `"*"`.
    All(String),
    /// These fields, by name.
    Some(Vec<String>),
}

/// Components to return, by name. Absent means every visible component; present means
/// exactly these.
pub type Select = BTreeMap<String, FieldSelect>;

/// A comparison or a combination of them, evaluated on the visible view of an entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Filter {
    /// `{"field": "disremembered.chunk.value", "op": "<", "value": 5}`
    Compare {
        /// `component.field`.
        field: String,
        /// One of `<`, `<=`, `==`, `!=`, `>`, `>=`.
        op: String,
        /// The value to compare with.
        value: Value,
    },
    /// All of these.
    All {
        /// The filters.
        all: Vec<Filter>,
    },
    /// Any of these.
    Any {
        /// The filters.
        any: Vec<Filter>,
    },
    /// Not this one.
    Not {
        /// The filter.
        not: Box<Filter>,
    },
}

/// A relation to follow from a node, and what to return for what it reaches.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RelationQuery {
    /// Entity kinds to keep; empty means all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    /// Components to return for each reached entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<Select>,
    /// Relations to follow from each reached entity.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub relations: BTreeMap<String, RelationQuery>,
    /// Keep only entities matching this.
    #[serde(default, rename = "where", skip_serializing_if = "Option::is_none")]
    pub filter: Option<Filter>,
    /// At most this many; more is an error, never a truncation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// An observation query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Query {
    /// Where to start.
    pub root: Root,
    /// Components to return for the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<Select>,
    /// Relations to follow from the root.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub relations: BTreeMap<String, RelationQuery>,
    /// How timed fields appear: `"value"` (the number, the default) or `"curve"` (the
    /// waterline, curve and bounds, so a client can evaluate the value at any moment
    /// without asking again).
    #[serde(default)]
    pub timed: TimedForm,
}

/// Limits a query is checked against. The driver sets them; a world may tighten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Deepest relation nesting allowed (the root is depth 0).
    pub max_depth: usize,
    /// Most entities one relation may reach when the query sets no `limit`, and the most
    /// a `limit` may ask for.
    pub max_breadth: usize,
    /// Most entities one query may touch in total.
    pub max_entities: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_depth: 4,
            max_breadth: 100,
            max_entities: 200,
        }
    }
}

/// Why a query was refused. Every variant names the limit or the nearest known name, so
/// a client or agent can repair the query without guessing.
#[derive(Debug, Clone, PartialEq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum QueryError {
    /// The root does not resolve for this viewer.
    #[error("no such root: {root}")]
    UnknownRoot {
        /// What was asked for.
        root: String,
    },
    /// The root entity does not exist or is not visible.
    #[error("the root entity is not visible to you")]
    RootNotVisible,
    /// A component name the registry does not know.
    #[error("unknown component {name}{}", nearest.as_ref().map(|n| format!("; did you mean {n}?")).unwrap_or_default())]
    UnknownComponent {
        /// What was asked for.
        name: String,
        /// The closest registered name, if any is close.
        nearest: Option<String>,
    },
    /// A field the component's schema does not have.
    #[error("unknown field {component}.{field}{}", nearest.as_ref().map(|n| format!("; did you mean {n}?")).unwrap_or_default())]
    UnknownField {
        /// The component.
        component: String,
        /// The field.
        field: String,
        /// The closest field name, if any is close.
        nearest: Option<String>,
    },
    /// A relation name the registry does not know.
    #[error("unknown relation {name}{}", nearest.as_ref().map(|n| format!("; did you mean {n}?")).unwrap_or_default())]
    UnknownRelation {
        /// What was asked for.
        name: String,
        /// The closest registered name, if any is close.
        nearest: Option<String>,
    },
    /// A filter that cannot be evaluated.
    #[error("bad filter: {message}")]
    BadFilter {
        /// What is wrong with it.
        message: String,
    },
    /// Relations nested deeper than the limit.
    #[error("query nests relations {depth} deep; the limit is {max}")]
    TooDeep {
        /// The query's depth.
        depth: usize,
        /// The limit.
        max: usize,
    },
    /// A relation reached more entities than its limit.
    #[error("relation {relation} reaches {found} entities; the limit is {limit}. Narrow it with kinds, where, or a smaller root.")]
    TooMany {
        /// The relation.
        relation: String,
        /// How many it found before stopping.
        found: usize,
        /// The limit in force.
        limit: usize,
    },
    /// The whole query touched more entities than allowed.
    #[error("query touches more than {max} entities")]
    TooManyEntities {
        /// The limit.
        max: usize,
    },
    /// This actor has used its query budget for now. The engine never raises this; the
    /// driver does, before calling the engine, so it shares the error type.
    #[error("query budget spent; try again in {retry_after_ms} ms")]
    OverBudget {
        /// When to try again.
        retry_after_ms: i64,
    },
}

/// Resolves named roots for a world. The driver implements it; `self` and `here` mean
/// nothing to the engine.
pub trait RootResolver {
    /// The entity a name refers to for this viewer, if any.
    fn resolve(&self, name: &str, arg: Option<&str>, viewer: &Viewer) -> Option<EntityId>;
}

/// A resolver that knows no names: only entity ids work.
pub struct NoNames;

impl RootResolver for NoNames {
    fn resolve(&self, _: &str, _: Option<&str>, _: &Viewer) -> Option<EntityId> {
        None
    }
}

/// What a query returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    /// The moment every timed value was evaluated at.
    pub as_of: Timestamp,
    /// The root node: `id`, `kind`, `parent`, `owner` (for signed-in viewers),
    /// `components` as selected, and `relations` as followed.
    pub root: Value,
    /// How many entities the query touched.
    pub entities: usize,
}

/// Run a query as `viewer` at `now`.
pub fn observe(
    store: &Store,
    viewer: &Viewer,
    now: Timestamp,
    query: &Query,
    roots: &dyn RootResolver,
    limits: &Limits,
) -> Result<Observation, QueryError> {
    // Static checks first: depth, names. Nothing is read until the document is sound.
    let depth = depth_of(&query.relations);
    if depth > limits.max_depth {
        return Err(QueryError::TooDeep {
            depth,
            max: limits.max_depth,
        });
    }
    check_select(store, query.select.as_ref())?;
    check_relations(store, &query.relations, limits)?;

    let root = match &query.root {
        Root::Entity(id) => *id,
        Root::Named { name, arg } => {
            roots
                .resolve(name, arg.as_deref(), viewer)
                .ok_or_else(|| QueryError::UnknownRoot {
                    root: match arg {
                        Some(a) => format!("{name}:{a}"),
                        None => name.clone(),
                    },
                })?
        }
    };
    let mut walk = Walk {
        store,
        viewer,
        now,
        limits,
        timed: query.timed,
        touched: 0,
    };
    let node = walk
        .node(root, query.select.as_ref(), &query.relations)?
        .ok_or(QueryError::RootNotVisible)?;
    Ok(Observation {
        as_of: now,
        root: node,
        entities: walk.touched,
    })
}

/// How deep the relations nest: none is 0, `contents` is 1, `contents.in` is 2.
fn depth_of(relations: &BTreeMap<String, RelationQuery>) -> usize {
    relations
        .values()
        .map(|r| 1 + depth_of(&r.relations))
        .max()
        .unwrap_or(0)
}

fn check_select(store: &Store, select: Option<&Select>) -> Result<(), QueryError> {
    let Some(select) = select else { return Ok(()) };
    let registry = store.registry();
    for (component, fields) in select {
        let Some(schema) = registry.get(component) else {
            return Err(QueryError::UnknownComponent {
                name: component.clone(),
                nearest: nearest(component, registry.schemas().map(|s| s.name.as_str())),
            });
        };
        if let FieldSelect::Some(names) = fields {
            for f in names {
                if schema.get(f).is_none() {
                    return Err(QueryError::UnknownField {
                        component: component.clone(),
                        field: f.clone(),
                        nearest: nearest(f, schema.fields.iter().map(|x| x.name.as_str())),
                    });
                }
            }
        } else if let FieldSelect::All(s) = fields {
            if s != "*" {
                return Err(QueryError::UnknownField {
                    component: component.clone(),
                    field: s.clone(),
                    nearest: Some("\"*\" or a list of field names".into()),
                });
            }
        }
    }
    Ok(())
}

fn check_relations(
    store: &Store,
    relations: &BTreeMap<String, RelationQuery>,
    limits: &Limits,
) -> Result<(), QueryError> {
    let registry = store.registry();
    for (name, rq) in relations {
        if registry.relation(name).is_none() {
            return Err(QueryError::UnknownRelation {
                name: name.clone(),
                nearest: nearest(name, registry.relations().map(|r| r.name.as_str())),
            });
        }
        if let Some(l) = rq.limit {
            if l > limits.max_breadth {
                return Err(QueryError::TooMany {
                    relation: name.clone(),
                    found: l,
                    limit: limits.max_breadth,
                });
            }
        }
        check_select(store, rq.select.as_ref())?;
        if let Some(f) = &rq.filter {
            check_filter(store, f)?;
        }
        check_relations(store, &rq.relations, limits)?;
    }
    Ok(())
}

fn check_filter(store: &Store, f: &Filter) -> Result<(), QueryError> {
    match f {
        Filter::Compare { field, op, .. } => {
            if !matches!(op.as_str(), "<" | "<=" | "==" | "!=" | ">" | ">=") {
                return Err(QueryError::BadFilter {
                    message: format!("op {op} is not one of <, <=, ==, !=, >, >="),
                });
            }
            let Some((component, fname)) = field.rsplit_once('.') else {
                return Err(QueryError::BadFilter {
                    message: format!("field {field} must be component.field"),
                });
            };
            let registry = store.registry();
            let Some(schema) = registry.get(component) else {
                return Err(QueryError::UnknownComponent {
                    name: component.to_string(),
                    nearest: nearest(component, registry.schemas().map(|s| s.name.as_str())),
                });
            };
            if schema.get(fname).is_none() {
                return Err(QueryError::UnknownField {
                    component: component.to_string(),
                    field: fname.to_string(),
                    nearest: nearest(fname, schema.fields.iter().map(|x| x.name.as_str())),
                });
            }
            Ok(())
        }
        Filter::All { all } => all.iter().try_for_each(|f| check_filter(store, f)),
        Filter::Any { any } => any.iter().try_for_each(|f| check_filter(store, f)),
        Filter::Not { not } => check_filter(store, not),
    }
}

/// The nearest of `candidates` to `name` by edit distance, if it is close enough to be
/// a typo rather than a different word.
pub fn nearest<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        let d = edit_distance(name, c);
        if best.map_or(true, |(bd, _)| d < bd) {
            best = Some((d, c));
        }
    }
    best.filter(|(d, c)| *d <= 3.max(c.len() / 3))
        .map(|(_, c)| c.to_string())
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

struct Walk<'a> {
    store: &'a Store,
    viewer: &'a Viewer,
    now: Timestamp,
    limits: &'a Limits,
    timed: TimedForm,
    touched: usize,
}

impl Walk<'_> {
    /// The node for an entity, or `None` if the viewer cannot see it at all.
    fn node(
        &mut self,
        id: EntityId,
        select: Option<&Select>,
        relations: &BTreeMap<String, RelationQuery>,
    ) -> Result<Option<Value>, QueryError> {
        let Some(view) = self.store.view_as(id, self.viewer, self.now, self.timed) else {
            return Ok(None);
        };
        self.touched += 1;
        if self.touched > self.limits.max_entities {
            return Err(QueryError::TooManyEntities {
                max: self.limits.max_entities,
            });
        }
        let mut node = project(view, select);
        if !relations.is_empty() {
            let mut rels = Map::new();
            for (name, rq) in relations {
                rels.insert(name.clone(), self.relation(id, name, rq)?);
            }
            node["relations"] = Value::Object(rels);
        }
        Ok(Some(node))
    }

    fn relation(
        &mut self,
        from: EntityId,
        name: &str,
        rq: &RelationQuery,
    ) -> Result<Value, QueryError> {
        let schema = self
            .store
            .registry()
            .relation(name)
            .cloned()
            .ok_or_else(|| QueryError::UnknownRelation {
                name: name.to_string(),
                nearest: None,
            })?;
        let limit = rq.limit.unwrap_or(self.limits.max_breadth);
        // Candidates: (entity, label) where the label is the map key for field relations.
        let candidates: Vec<(EntityId, Option<String>)> = match &schema.kind {
            RelationKind::Children { kinds } => self
                .store
                .children(from)
                .filter(|e| kinds.is_empty() || kinds.contains(&e.kind))
                .map(|e| (e.id, None))
                .collect(),
            RelationKind::Parent => self
                .store
                .entity(from)
                .and_then(|e| e.parent)
                .map(|p| vec![(p, None)])
                .unwrap_or_default(),
            RelationKind::Field { component, field } => {
                // Through the *visible* view, so a hidden field yields nothing.
                let view = self
                    .store
                    .view(from, self.viewer, self.now)
                    .unwrap_or(Value::Null);
                match &view["components"][component][field] {
                    Value::Number(n) => n
                        .as_u64()
                        .map(|id| vec![(EntityId(id), None)])
                        .unwrap_or_default(),
                    Value::Array(items) => items
                        .iter()
                        .filter_map(|v| v.as_u64().map(|id| (EntityId(id), None)))
                        .collect(),
                    Value::Object(m) => m
                        .iter()
                        .filter_map(|(k, v)| v.as_u64().map(|id| (EntityId(id), Some(k.clone()))))
                        .collect(),
                    _ => Vec::new(),
                }
            }
        };
        // Keep the matches first, so breadth is judged before any child is walked and
        // a too-broad relation fails with its own reason rather than the total.
        let mut kept: Vec<(EntityId, Option<String>)> = Vec::new();
        for (id, label) in candidates {
            let Some(e) = self.store.entity(id) else {
                continue;
            };
            if !rq.kinds.is_empty() && !rq.kinds.contains(&e.kind) {
                continue;
            }
            // Filters see the visible view only.
            if let Some(f) = &rq.filter {
                let Some(view) = self.store.view(id, self.viewer, self.now) else {
                    continue;
                };
                if !matches(f, &view) {
                    continue;
                }
            }
            kept.push((id, label));
            if kept.len() > limit {
                return Err(QueryError::TooMany {
                    relation: name.to_string(),
                    found: kept.len(),
                    limit,
                });
            }
        }
        let mut out = Vec::new();
        for (id, label) in kept {
            let Some(mut node) = self.node(id, rq.select.as_ref(), &rq.relations)? else {
                continue;
            };
            if let Some(l) = label {
                node["via"] = Value::String(l);
            }
            out.push(node);
        }
        Ok(if matches!(schema.kind, RelationKind::Parent) {
            out.into_iter().next().unwrap_or(Value::Null)
        } else {
            Value::Array(out)
        })
    }
}

/// Narrow a visible view to the selected components and fields.
fn project(mut view: Value, select: Option<&Select>) -> Value {
    let Some(select) = select else { return view };
    let all = view["components"].take();
    let mut kept = Map::new();
    for (component, fields) in select {
        let Some(comp) = all.get(component) else {
            continue;
        };
        match fields {
            FieldSelect::All(_) => {
                kept.insert(component.clone(), comp.clone());
            }
            FieldSelect::Some(names) => {
                let mut sub = Map::new();
                for n in names {
                    if let Some(v) = comp.get(n) {
                        sub.insert(n.clone(), v.clone());
                    }
                }
                if !sub.is_empty() {
                    kept.insert(component.clone(), Value::Object(sub));
                }
            }
        }
    }
    view["components"] = Value::Object(kept);
    view
}

/// Evaluate a filter against a visible view. A field the viewer cannot see compares as
/// absent, which is false for every operator except `!=`.
fn matches(f: &Filter, view: &Value) -> bool {
    match f {
        Filter::Compare { field, op, value } => {
            let Some((component, fname)) = field.rsplit_once('.') else {
                return false;
            };
            let actual = &view["components"][component][fname];
            if actual.is_null() {
                return op == "!=";
            }
            match (actual.as_f64(), value.as_f64()) {
                (Some(a), Some(b)) => match op.as_str() {
                    "<" => a < b,
                    "<=" => a <= b,
                    "==" => a == b,
                    "!=" => a != b,
                    ">" => a > b,
                    ">=" => a >= b,
                    _ => false,
                },
                _ => match op.as_str() {
                    "==" => actual == value,
                    "!=" => actual != value,
                    _ => false,
                },
            }
        }
        Filter::All { all } => all.iter().all(|f| matches(f, view)),
        Filter::Any { any } => any.iter().any(|f| matches(f, view)),
        Filter::Not { not } => !matches(not, view),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_parse_from_every_form() {
        let q: Query = serde_json::from_value(json!({"root": "here"})).unwrap();
        assert_eq!(
            q.root,
            Root::Named {
                name: "here".into(),
                arg: None
            }
        );
        let q: Query = serde_json::from_value(json!({"root": {"entity": 12}})).unwrap();
        assert_eq!(q.root, Root::Entity(EntityId(12)));
        let q: Query = serde_json::from_value(json!({"root": {"palace": "dave"}})).unwrap();
        assert_eq!(
            q.root,
            Root::Named {
                name: "palace".into(),
                arg: Some("dave".into())
            }
        );
        assert!(serde_json::from_value::<Query>(json!({"root": [1]})).is_err());
        let back = serde_json::to_value(&q).unwrap();
        assert_eq!(back["root"], json!({"palace": "dave"}));
    }

    #[test]
    fn depth_counts_nesting() {
        let q: Query = serde_json::from_value(json!({"root": "here"})).unwrap();
        assert_eq!(depth_of(&q.relations), 0);
        let q: Query =
            serde_json::from_value(json!({"root": "here", "relations": {"contents": {}}})).unwrap();
        assert_eq!(depth_of(&q.relations), 1);
        let q: Query = serde_json::from_value(
            json!({"root": "here", "relations": {"contents": {"relations": {"contents": {"relations": {"in": {}}}}}, "in": {}}}),
        )
        .unwrap();
        assert_eq!(depth_of(&q.relations), 3);
    }

    #[test]
    fn nearest_suggests_typos_not_strangers() {
        assert_eq!(
            nearest(
                "disremembered.chunk",
                ["disremembered.chunks", "disremembered.room"].into_iter()
            ),
            Some("disremembered.chunks".into())
        );
        assert_eq!(
            nearest("contents", ["content", "in", "exits"].into_iter()),
            Some("content".into())
        );
        assert_eq!(nearest("zzzzzzzzzz", ["in", "exits"].into_iter()), None);
    }
}
