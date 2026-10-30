//! A session over WebSocket: the same commands and observations as HTTP, plus push.
//!
//! The client logs in, then sends `command`, `observe`, `subscribe` and `unsubscribe`
//! frames, each with an `id` it chooses; the driver answers each by id. Whenever the
//! world changes (a command, a job, a message through the Gate), the driver pushes the
//! events this actor may see as `event` frames, and re-runs every subscription whose
//! result set those events touched, pushing the fresh observation under the
//! subscription's id. A renderer holds no state of its own: it subscribes to what it
//! draws, evaluates timed values from their curves between pushes, and sends commands.
//!
//! ```json
//! > {"type":"login","actor":"dave","passphrase":"..."}
//! < {"type":"welcome","actor":"dave","cursor":131,"fresh":false}
//! > {"type":"subscribe","id":"room","query":{"root":"here","relations":{"contents":{}},"timed":"curve"}}
//! < {"type":"observation","id":"room", ...}
//! > {"type":"command","id":"c1","command":"attack","args":["wisp"]}
//! < {"type":"result","id":"c1","ok":true,"summary":"...","result":{...}}
//! < {"type":"event","event":{"seq":132,...}}
//! < {"type":"observation","id":"room", ...}
//! ```

use crate::net::Shared;
use crate::observe::Request;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use moondial_log::Cursor;
use moondial_registry::{ActorKind, CallContext};
use moondial_store::Viewer;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// Most subscriptions one session may hold.
const MAX_SUBSCRIPTIONS: usize = 8;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Frame {
    Login {
        actor: String,
        #[serde(default)]
        passphrase: String,
        #[serde(default)]
        admin: Option<String>,
    },
    Command {
        id: Value,
        command: String,
        #[serde(default)]
        args: Value,
    },
    Observe {
        id: Value,
        query: Request,
    },
    Subscribe {
        id: Value,
        query: Request,
    },
    Unsubscribe {
        id: Value,
    },
    Ping {
        #[serde(default)]
        id: Value,
    },
}

/// A subscription to refresh: its key, the fresh observation frame, and the ids it now covers.
type Refresh = (String, Value, HashSet<u64>);

struct Subscription {
    request: Request,
    /// Entity ids in the last observation; an event touching one re-runs the query.
    ids: HashSet<u64>,
}

/// Every entity id a node and its relations mention.
fn ids_of(node: &Value, out: &mut HashSet<u64>) {
    if let Some(id) = node["id"].as_u64() {
        out.insert(id);
    }
    if let Some(p) = node["parent"].as_u64() {
        out.insert(p);
    }
    if let Some(rels) = node["relations"].as_object() {
        for v in rels.values() {
            match v {
                Value::Array(items) => items.iter().for_each(|n| ids_of(n, out)),
                Value::Object(_) => ids_of(v, out),
                _ => {}
            }
        }
    }
}

/// The entity ids an event touches: its subject and, for creations and moves, the
/// container it entered or left.
fn touched(e: &moondial_log::Event) -> Vec<u64> {
    let mut out = Vec::new();
    for key in ["id", "parent", "from"] {
        if let Some(v) = e.payload.get(key).and_then(Value::as_u64) {
            out.push(v);
        }
    }
    out
}

pub async fn upgrade(ws: WebSocketUpgrade, State(s): State<Shared>) -> Response {
    ws.on_upgrade(move |socket| run(socket, s))
}

async fn run(socket: WebSocket, s: Shared) {
    let (mut tx, mut rx) = socket.split();
    let mut changes = s.changed.subscribe();
    let mut actor: Option<String> = None;
    let mut admin = false;
    let mut cursor: u64 = 0;
    let mut subs: HashMap<String, Subscription> = HashMap::new();

    macro_rules! send {
        ($v:expr) => {
            if tx
                .send(Message::Text(
                    serde_json::to_string(&$v).unwrap_or_default().into(),
                ))
                .await
                .is_err()
            {
                return;
            }
        };
    }

    send!(json!({"type": "hello", "world": "disremembered", "protocol": "moondial-session/0"}));
    loop {
        tokio::select! {
            incoming = rx.next() => {
                let Some(Ok(msg)) = incoming else { return };
                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Close(_) => return,
                    _ => continue,
                };
                let frame: Frame = match serde_json::from_str(&text) {
                    Ok(f) => f,
                    Err(e) => {
                        send!(json!({"type": "error", "error": format!("bad frame: {e}")}));
                        continue;
                    }
                };
                match frame {
                    Frame::Login { actor: name, passphrase, admin: token } => {
                        let logged_in: Result<(bool, u64), String> = {
                            let Ok(mut w) = s.world.lock() else { continue };
                            crate::net::login_for_session(&mut w, &name, &passphrase).map(|fresh| {
                                (fresh, w.log.head(&w.id).ok().flatten().map(|x| x.0).unwrap_or(0))
                            })
                        };
                        let (fresh, head) = match logged_in {
                            Ok(v) => v,
                            Err(e) => {
                                send!(json!({"type": "error", "error": e}));
                                continue;
                            }
                        };
                        admin = s.is_admin(token.as_deref());
                        actor = Some(name.clone());
                        cursor = head;
                        subs.clear();
                        send!(json!({"type": "welcome", "actor": name, "cursor": head, "fresh": fresh, "admin": admin}));
                    }
                    Frame::Ping { id } => send!(json!({"type": "pong", "id": id})),
                    Frame::Command { id, command, args } => {
                        let Some(name) = actor.clone() else {
                            send!(json!({"type": "result", "id": id, "ok": false, "error": "log in first"}));
                            continue;
                        };
                        let kind = if admin { ActorKind::Admin } else { ActorKind::Citizen };
                        let reply = {
                            let Ok(mut w) = s.world.lock() else { continue };
                            if command == "observe" {
                                // A read, not speech: runs outside the gate.
                                let ctx = CallContext { actor: name, kind };
                                match w.call(&ctx, &command, &args) {
                                    Ok(out) => json!({"type": "result", "id": id, "ok": true, "summary": out.summary, "result": out.result}),
                                    Err(e) => json!({"type": "result", "id": id, "ok": false, "error": e.to_string(), "detail": e}),
                                }
                            } else {
                                let Ok(mut budget) = s.hour_budget.lock() else { continue };
                                let p = crate::net::compiled_passage(&mut w, &s, &mut budget, &name, kind, crate::gate::Via::Session, &command, &args);
                                let mut v = crate::net::passage_json(&p);
                                v["type"] = json!("result");
                                v["id"] = id;
                                v
                            }
                        };
                        let _ = s.changed.send(());
                        send!(reply);
                    }
                    Frame::Observe { id, query } | Frame::Subscribe { id, query } if actor.is_none() => {
                        send!(json!({"type": "error", "id": id, "error": "log in first"}));
                        let _ = query;
                    }
                    Frame::Observe { id, query } => {
                        let name = actor.clone().unwrap_or_default();
                        let reply = {
                            let Ok(w) = s.world.lock() else { continue };
                            observe_frame(&w, &name, admin, &id, &query, &s)
                        };
                        send!(reply);
                    }
                    Frame::Subscribe { id, query } => {
                        let key = id.to_string();
                        if subs.len() >= MAX_SUBSCRIPTIONS && !subs.contains_key(&key) {
                            send!(json!({"type": "error", "id": id, "error": format!("at most {MAX_SUBSCRIPTIONS} subscriptions")}));
                            continue;
                        }
                        let name = actor.clone().unwrap_or_default();
                        let (reply, ids) = {
                            let Ok(w) = s.world.lock() else { continue };
                            let reply = observe_frame(&w, &name, admin, &id, &query, &s);
                            let mut ids = HashSet::new();
                            ids_of(&reply["root"], &mut ids);
                            (reply, ids)
                        };
                        if reply["type"] == "observation" {
                            subs.insert(key, Subscription { request: query, ids });
                        }
                        send!(reply);
                    }
                    Frame::Unsubscribe { id } => {
                        subs.remove(&id.to_string());
                        send!(json!({"type": "unsubscribed", "id": id}));
                    }
                }
            }
            changed = changes.recv() => {
                if changed.is_err() && !matches!(changed, Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) {
                    return;
                }
                let Some(name) = actor.clone() else { continue };
                // Everything since the cursor this actor may see, and which subscriptions it touched.
                let (events, refresh): (Vec<Value>, Vec<Refresh>) = {
                    let Ok(w) = s.world.lock() else { continue };
                    let head = w.log.head(&w.id).ok().flatten().map(|x| x.0).unwrap_or(0);
                    if head <= cursor { continue; }
                    let viewer = if admin { Viewer::admin() } else { Viewer::actor(&name) };
                    let now = w.now();
                    let all = w.log.read(&w.id, Cursor { after: cursor }, 500).unwrap_or_default();
                    cursor = head;
                    let mut dirty: HashSet<String> = HashSet::new();
                    for e in &all {
                        for id in touched(e) {
                            for (k, sub) in &subs {
                                if sub.ids.contains(&id) {
                                    dirty.insert(k.clone());
                                }
                            }
                        }
                    }
                    let events: Vec<Value> = all
                        .iter()
                        .filter(|e| crate::observe::visible(&w, &viewer, now, e))
                        .map(|e| crate::observe::redacted(&w, &viewer, e.clone()))
                        .map(|e| json!({"type": "event", "event": e}))
                        .collect();
                    let mut refresh = Vec::new();
                    for k in dirty {
                        let Some(sub) = subs.get(&k) else { continue };
                        let id: Value = serde_json::from_str(&k).unwrap_or(Value::String(k.clone()));
                        let reply = observe_frame(&w, &name, admin, &id, &sub.request, &s);
                        let mut ids = HashSet::new();
                        ids_of(&reply["root"], &mut ids);
                        refresh.push((k, reply, ids));
                    }
                    (events, refresh)
                };
                for e in events {
                    send!(e);
                }
                for (k, reply, ids) in refresh {
                    if let Some(sub) = subs.get_mut(&k) {
                        sub.ids = ids;
                    }
                    send!(reply);
                }
            }
        }
    }
}

fn observe_frame(
    w: &crate::world::World,
    name: &str,
    admin: bool,
    id: &Value,
    query: &Request,
    s: &Shared,
) -> Value {
    let viewer = if admin {
        Viewer::admin()
    } else {
        Viewer::actor(name)
    };
    let kind = if admin {
        ActorKind::Admin
    } else {
        ActorKind::Citizen
    };
    match w.observe(&viewer, kind, query, &s.limits) {
        Ok(obs) => {
            let mut v = serde_json::to_value(obs).unwrap_or_default();
            v["type"] = json!("observation");
            v["id"] = id.clone();
            v
        }
        Err(e) => json!({"type": "error", "id": id, "error": e.to_string(), "detail": e}),
    }
}
