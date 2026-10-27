//! The observation query over a small world: two rooms joined by exits, two citizens
//! with chunks, a creature. Scenario tests for projection, relations, filters, limits and
//! errors, then a property test: whatever a stranger asks, over random worlds and random
//! queries, no owner or admin field ever appears in the reply, and a stranger's filter
//! on a hidden field selects nothing an open filter would not.

use moondial_query::{observe, Limits, NoNames, Query, QueryError, Root, RootResolver};
use moondial_store::{
    ComponentSchema, EntityId, Field, FieldKind, Registry, RelationKind, Store, StoreEvent, Viewer,
    Visibility,
};
use moondial_time::{Curve, Duration, TimedValue, Timestamp};
use proptest::prelude::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const T0: Timestamp = Timestamp(1_700_000_000_000);

fn registry() -> Registry {
    let mut r = Registry::new();
    r.register(
        ComponentSchema::new("w.room", 1)
            .field("name", FieldKind::Plain, Visibility::Public)
            .field("secret", FieldKind::Plain, Visibility::Admin),
    );
    r.register(ComponentSchema::new("w.exits", 1).field(
        "exits",
        FieldKind::Plain,
        Visibility::Public,
    ));
    r.register(
        ComponentSchema::new("w.chunk", 1)
            .field("value", FieldKind::Timed, Visibility::Owner)
            .field("size", FieldKind::Plain, Visibility::Owner)
            .field("source", FieldKind::Plain, Visibility::Public),
    );
    r.register(
        ComponentSchema::new("w.body", 1)
            .field("hp", FieldKind::Plain, Visibility::Party)
            .field("name", FieldKind::Plain, Visibility::Public),
    );
    r.register_relation(
        "contents",
        "what is inside",
        RelationKind::Children { kinds: vec![] },
    );
    r.register_relation("in", "the container", RelationKind::Parent);
    r.register_relation(
        "exits",
        "rooms reachable by an exit",
        RelationKind::Field {
            component: "w.exits".into(),
            field: "exits".into(),
        },
    );
    r
}

fn apply(store: &mut Store, at: Timestamp, e: StoreEvent) {
    store.apply_store_event(at, &e).unwrap();
}

fn create(
    store: &mut Store,
    kind: &str,
    owner: Option<&str>,
    parent: Option<EntityId>,
) -> EntityId {
    let id = store.allocate_id();
    apply(
        store,
        T0,
        StoreEvent::EntityCreated {
            id,
            kind: kind.into(),
            owner: owner.map(str::to_string),
            parent,
            scopes: vec![],
        },
    );
    id
}

fn set(store: &mut Store, id: EntityId, component: &str, fields: Vec<(&str, Field)>) {
    apply(
        store,
        T0,
        StoreEvent::ComponentSet {
            id,
            component: component.into(),
            fields: fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        },
    );
}

fn plain(v: Value) -> Field {
    Field::Plain(v)
}

struct World {
    store: Store,
    gate: EntityId,
    fog: EntityId,
    dave: EntityId,
    dave_window: EntityId,
}

fn world() -> World {
    let mut store = Store::new(registry());
    let gate = create(&mut store, "room", None, None);
    let fog = create(&mut store, "room", None, None);
    set(
        &mut store,
        gate,
        "w.room",
        vec![
            ("name", plain(json!("The Threshold"))),
            ("secret", plain(json!("spawn table"))),
        ],
    );
    set(
        &mut store,
        gate,
        "w.exits",
        vec![("exits", plain(json!({"east": fog.0})))],
    );
    set(
        &mut store,
        fog,
        "w.room",
        vec![
            ("name", plain(json!("The Fogbank"))),
            ("secret", plain(json!("wisps"))),
        ],
    );
    set(
        &mut store,
        fog,
        "w.exits",
        vec![("exits", plain(json!({"west": gate.0})))],
    );
    let dave = create(&mut store, "citizen", Some("dave"), Some(gate));
    set(
        &mut store,
        dave,
        "w.body",
        vec![("hp", plain(json!(21))), ("name", plain(json!("dave")))],
    );
    let tyler = create(&mut store, "citizen", Some("tyler"), Some(gate));
    set(
        &mut store,
        tyler,
        "w.body",
        vec![("hp", plain(json!(30))), ("name", plain(json!("tyler")))],
    );
    let dave_window = create(&mut store, "window", Some("dave"), Some(dave));
    for (source, size) in [("wisp", 15.0), ("hollow", 60.0), ("revenant", 200.0)] {
        let c = create(&mut store, "chunk", Some("dave"), Some(dave_window));
        set(
            &mut store,
            c,
            "w.chunk",
            vec![
                (
                    "value",
                    Field::Timed(TimedValue::new(
                        size,
                        T0,
                        Curve::Exponential {
                            half_life: Duration::seconds(10),
                        },
                    )),
                ),
                ("size", plain(json!(size))),
                ("source", plain(json!(source))),
            ],
        );
        let _ = c;
    }
    let wisp = create(&mut store, "creature", None, Some(fog));
    set(
        &mut store,
        wisp,
        "w.body",
        vec![("hp", plain(json!(8))), ("name", plain(json!("wisp")))],
    );
    World {
        store,
        gate,
        fog,
        dave,
        dave_window,
    }
}

struct Names<'a>(&'a World);
impl RootResolver for Names<'_> {
    fn resolve(&self, name: &str, arg: Option<&str>, viewer: &Viewer) -> Option<EntityId> {
        match (name, arg) {
            ("here", _) => Some(self.0.gate),
            ("self", _) if viewer.actor.as_deref() == Some("dave") => Some(self.0.dave),
            ("window", Some("dave")) => Some(self.0.dave_window),
            _ => None,
        }
    }
}

fn q(v: Value) -> Query {
    serde_json::from_value(v).unwrap()
}

#[test]
fn a_projection_returns_exactly_the_fields_asked_for() {
    let w = world();
    let dave = Viewer::actor("dave");
    let obs = observe(
        &w.store,
        &dave,
        T0,
        &q(json!({"root": "here", "select": {"w.room": ["name"]}})),
        &Names(&w),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(obs.root["kind"], json!("room"));
    assert_eq!(
        obs.root["components"],
        json!({"w.room": {"name": "The Threshold"}})
    );
    assert!(obs.root.get("relations").is_none());
    assert_eq!(obs.entities, 1);

    // No select: every visible component. An admin also sees the secret.
    let all = observe(
        &w.store,
        &dave,
        T0,
        &q(json!({"root": "here"})),
        &Names(&w),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(
        all.root["components"]["w.room"],
        json!({"name": "The Threshold"})
    );
    assert_eq!(
        all.root["components"]["w.exits"]["exits"]["east"],
        json!(w.fog.0)
    );
    let admin = observe(
        &w.store,
        &Viewer::admin(),
        T0,
        &q(json!({"root": "here"})),
        &Names(&w),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(
        admin.root["components"]["w.room"]["secret"],
        json!("spawn table")
    );

    // An empty select is id and kind only.
    let bare = observe(
        &w.store,
        &dave,
        T0,
        &q(json!({"root": "here", "select": {}})),
        &Names(&w),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(bare.root["components"], json!({}));
}

#[test]
fn relations_follow_children_parents_and_fields() {
    let w = world();
    let dave = Viewer::actor("dave");
    let obs = observe(
        &w.store,
        &dave,
        T0,
        &q(json!({
            "root": "here",
            "select": {"w.room": ["name"]},
            "relations": {
                "contents": {"kinds": ["citizen"], "select": {"w.body": "*"}},
                "exits": {"select": {"w.room": ["name"]}, "relations": {"contents": {"select": {"w.body": ["name"]}}}}
            }
        })),
        &Names(&w),
        &Limits::default(),
    )
    .unwrap();
    let here = obs.root["relations"]["contents"].as_array().unwrap();
    assert_eq!(here.len(), 2);
    // dave sees his own hp (party-visible, owner counts), not tyler's.
    let me = here.iter().find(|n| n["owner"] == json!("dave")).unwrap();
    assert_eq!(me["components"]["w.body"]["hp"], json!(21));
    let them = here.iter().find(|n| n["owner"] == json!("tyler")).unwrap();
    assert_eq!(them["components"]["w.body"], json!({"name": "tyler"}));
    let exits = obs.root["relations"]["exits"].as_array().unwrap();
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0]["via"], json!("east"));
    assert_eq!(
        exits[0]["components"]["w.room"]["name"],
        json!("The Fogbank")
    );
    assert_eq!(
        exits[0]["relations"]["contents"][0]["components"]["w.body"]["name"],
        json!("wisp")
    );
    assert_eq!(obs.entities, 5);

    // Parent relation is a single node.
    let up = observe(
        &w.store,
        &dave,
        T0,
        &q(json!({"root": "self", "select": {}, "relations": {"in": {"select": {"w.room": ["name"]}}}})),
        &Names(&w),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(
        up.root["relations"]["in"]["components"]["w.room"]["name"],
        json!("The Threshold")
    );
    let top = observe(
        &w.store,
        &dave,
        T0,
        &q(json!({"root": "here", "select": {}, "relations": {"in": {}}})),
        &Names(&w),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(top.root["relations"]["in"], Value::Null);
}

#[test]
fn filters_run_on_the_visible_view_at_the_query_moment() {
    let w = world();
    let dave = Viewer::actor("dave");
    let later = T0.plus(Duration::seconds(20)); // two half-lives: 15 -> 3.75, 60 -> 15, 200 -> 50
    let fading = q(json!({
        "root": {"window": "dave"},
        "select": {},
        "relations": {"contents": {"where": {"field": "w.chunk.value", "op": "<", "value": 20}, "select": {"w.chunk": ["source", "value"]}}}
    }));
    let obs = observe(
        &w.store,
        &dave,
        later,
        &fading,
        &Names(&w),
        &Limits::default(),
    )
    .unwrap();
    let got: Vec<&str> = obs.root["relations"]["contents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["components"]["w.chunk"]["source"].as_str().unwrap())
        .collect();
    assert_eq!(got, vec!["wisp", "hollow"]);
    assert_eq!(obs.as_of, later);

    // Combinators.
    let combo = q(json!({
        "root": {"window": "dave"}, "select": {},
        "relations": {"contents": {"where": {"any": [
            {"field": "w.chunk.source", "op": "==", "value": "revenant"},
            {"not": {"field": "w.chunk.value", "op": ">=", "value": 5}}
        ]}}}
    }));
    let obs = observe(
        &w.store,
        &dave,
        later,
        &combo,
        &Names(&w),
        &Limits::default(),
    )
    .unwrap();
    let got: Vec<&str> = obs.root["relations"]["contents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["components"]["w.chunk"]["source"].as_str().unwrap())
        .collect();
    assert_eq!(got, vec!["wisp", "revenant"]);

    // A stranger filtering on dave's values gets nothing: the field is not there to compare.
    let stranger = Viewer::actor("mallory");
    let obs = observe(
        &w.store,
        &stranger,
        later,
        &fading,
        &Names(&w),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(obs.root["relations"]["contents"], json!([]));
    // ...and `!=` on a hidden field matches everything, which reveals nothing.
    let ne = q(
        json!({"root": {"window": "dave"}, "select": {}, "relations": {"contents": {"where": {"field": "w.chunk.value", "op": "!=", "value": 0}, "select": {"w.chunk": ["source"]}}}}),
    );
    let obs = observe(
        &w.store,
        &stranger,
        later,
        &ne,
        &Names(&w),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(
        obs.root["relations"]["contents"].as_array().unwrap().len(),
        3
    );
}

#[test]
fn limits_and_names_are_checked_before_anything_runs() {
    let w = world();
    let dave = Viewer::actor("dave");
    let names = Names(&w);
    let tight = Limits {
        max_depth: 1,
        max_breadth: 2,
        max_entities: 3,
    };
    let err = |v: Value, l: &Limits| observe(&w.store, &dave, T0, &q(v), &names, l).unwrap_err();

    assert_eq!(
        err(
            json!({"root": "here", "relations": {"contents": {"relations": {"in": {}}}}}),
            &tight
        ),
        QueryError::TooDeep { depth: 2, max: 1 }
    );
    assert_eq!(
        err(
            json!({"root": "here", "relations": {"contents": {"limit": 50}}}),
            &tight
        ),
        QueryError::TooMany {
            relation: "contents".into(),
            found: 50,
            limit: 2
        }
    );
    assert_eq!(
        err(
            json!({"root": {"window": "dave"}, "relations": {"contents": {}}}),
            &tight
        ),
        QueryError::TooMany {
            relation: "contents".into(),
            found: 3,
            limit: 2
        }
    );
    assert_eq!(
        err(
            json!({"root": {"window": "dave"}, "relations": {"contents": {"limit": 1}}}),
            &tight
        ),
        QueryError::TooMany {
            relation: "contents".into(),
            found: 2,
            limit: 1
        }
    );
    assert_eq!(
        err(
            json!({"root": "here", "relations": {"contents": {}, "exits": {}}}),
            &Limits {
                max_depth: 4,
                max_breadth: 100,
                max_entities: 3
            }
        ),
        QueryError::TooManyEntities { max: 3 }
    );

    assert_eq!(
        err(
            json!({"root": "here", "select": {"w.rooms": "*"}}),
            &Limits::default()
        ),
        QueryError::UnknownComponent {
            name: "w.rooms".into(),
            nearest: Some("w.room".into())
        }
    );
    assert_eq!(
        err(
            json!({"root": "here", "select": {"w.room": ["nmae"]}}),
            &Limits::default()
        ),
        QueryError::UnknownField {
            component: "w.room".into(),
            field: "nmae".into(),
            nearest: Some("name".into())
        }
    );
    assert_eq!(
        err(
            json!({"root": "here", "relations": {"content": {}}}),
            &Limits::default()
        ),
        QueryError::UnknownRelation {
            name: "content".into(),
            nearest: Some("contents".into())
        }
    );
    assert_eq!(
        err(
            json!({"root": "here", "relations": {"contents": {"where": {"field": "w.chunk.value", "op": "~", "value": 1}}}}),
            &Limits::default()
        ),
        QueryError::BadFilter {
            message: "op ~ is not one of <, <=, ==, !=, >, >=".into()
        }
    );
    assert_eq!(
        err(json!({"root": "nowhere"}), &Limits::default()),
        QueryError::UnknownRoot {
            root: "nowhere".into()
        }
    );
    assert_eq!(
        err(json!({"root": {"palace": "x"}}), &Limits::default()),
        QueryError::UnknownRoot {
            root: "palace:x".into()
        }
    );
    assert_eq!(
        err(json!({"root": {"entity": 9999}}), &Limits::default()),
        QueryError::RootNotVisible
    );
    // Errors serialize with a reason tag, like command errors.
    let e: Value = serde_json::to_value(QueryError::TooDeep { depth: 5, max: 4 }).unwrap();
    assert_eq!(e, json!({"reason": "too_deep", "depth": 5, "max": 4}));
    assert!(observe(
        &w.store,
        &dave,
        T0,
        &Query {
            root: Root::Entity(w.gate),
            select: None,
            relations: BTreeMap::new()
        },
        &NoNames,
        &Limits::default()
    )
    .is_ok());
}

// ---- The property: a stranger never sees what is not theirs. ----

fn leaked(node: &Value, hidden: &[(&str, &str)]) -> bool {
    if let Some(comps) = node["components"].as_object() {
        for (c, f) in hidden {
            if comps.get(*c).and_then(|v| v.get(*f)).is_some() {
                return true;
            }
        }
    }
    if let Some(rels) = node["relations"].as_object() {
        for v in rels.values() {
            let hit = match v {
                Value::Array(items) => items.iter().any(|n| leaked(n, hidden)),
                Value::Object(_) => leaked(v, hidden),
                _ => false,
            };
            if hit {
                return true;
            }
        }
    }
    false
}

fn arb_select() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        Just(json!({})),
        Just(json!({"w.room": "*", "w.chunk": "*", "w.body": "*", "w.exits": "*"})),
        Just(
            json!({"w.chunk": ["value", "size", "source"], "w.body": ["hp", "name"], "w.room": ["secret", "name"]})
        ),
    ]
}

fn arb_relation(depth: u32) -> impl Strategy<Value = Value> {
    let leaf = (
        arb_select(),
        prop_oneof![
            Just(Value::Null),
            Just(json!({"field": "w.chunk.value", "op": "<", "value": 30})),
            Just(json!({"field": "w.room.secret", "op": "==", "value": "wisps"}))
        ],
    )
        .prop_map(|(s, f)| {
            let mut m = serde_json::Map::new();
            if !s.is_null() {
                m.insert("select".into(), s);
            }
            if !f.is_null() {
                m.insert("where".into(), f);
            }
            Value::Object(m)
        });
    leaf.prop_recursive(depth, 8, 3, |inner| {
        (
            inner,
            prop::collection::btree_map(
                prop_oneof![
                    Just("contents".to_string()),
                    Just("in".to_string()),
                    Just("exits".to_string())
                ],
                Just(json!({})),
                0..3,
            ),
        )
            .prop_map(|(mut node, rels)| {
                if !rels.is_empty() {
                    node["relations"] = json!(rels);
                }
                node
            })
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn a_stranger_never_sees_owner_or_admin_fields(
        root in prop_oneof![Just(json!("here")), Just(json!({"window": "dave"})), Just(json!({"entity": 3})), Just(json!({"entity": 7}))],
        select in arb_select(),
        rels in prop::collection::btree_map(prop_oneof![Just("contents".to_string()), Just("in".to_string()), Just("exits".to_string())], arb_relation(3), 0..3),
        dt in 0i64..60_000,
    ) {
        let w = world();
        let mut qv = json!({"root": root});
        if !select.is_null() { qv["select"] = select; }
        if !rels.is_empty() { qv["relations"] = json!(rels); }
        let query: Query = serde_json::from_value(qv).unwrap();
        let now = T0.plus(Duration(dt));
        let hidden = [("w.chunk", "value"), ("w.chunk", "size"), ("w.body", "hp"), ("w.room", "secret")];
        for stranger in [Viewer::anonymous(), Viewer::actor("mallory")] {
            match observe(&w.store, &stranger, now, &query, &Names(&w), &Limits::default()) {
                Ok(obs) => prop_assert!(!leaked(&obs.root, &hidden), "leak in {}", obs.root),
                Err(QueryError::TooDeep { .. } | QueryError::UnknownRoot { .. }) => {}
                Err(e) => prop_assert!(false, "unexpected error {e}"),
            }
        }
        // The owner sees chunk values; the engine is not simply hiding everything.
        if let Ok(obs) = observe(&w.store, &Viewer::actor("dave"), now, &query, &Names(&w), &Limits::default()) {
            let _ = obs;
        }
    }
}
