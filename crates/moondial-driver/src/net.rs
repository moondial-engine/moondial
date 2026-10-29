//! The two listeners of the pre-alpha driver.
//!
//! - A line protocol on TCP, for a terminal (`nc`, `telnet`, or the text client to come).
//!   `login <name> <passphrase>` arrives as a citizen; every other line is `<verb> [args...]`.
//! - JSON over HTTP, for tools and tests: `/health`, `/v0/commands`, `/v0/command`,
//!   `/v0/view/{id}`, `/v0/log`.
//!
//! Both go through the same registry and the same `World::call`, so a command means the
//! same thing on either. Identity is a name and a passphrase set on first login; there
//! are no sessions beyond the connection, and the HTTP side sends the passphrase with
//! every call. That is fine for a test server and wrong for anything else, and the world
//! card says so.

use crate::disremembered;
use crate::world::World;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use moondial_log::Cursor;
use moondial_registry::{ActorKind, CallContext};
use moondial_store::{EntityId, Viewer};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// Shared driver state.
#[derive(Clone)]
pub struct Shared {
    pub world: Arc<Mutex<World>>,
    pub admin_token: Option<String>,
    /// Per-actor observation budget.
    pub budget: Arc<Mutex<crate::observe::Budget>>,
    /// Limits every observation is checked against.
    pub limits: moondial_query::Limits,
    /// The world's CLEAR checker, compiled once.
    pub checker: Arc<moondial_clear::Checker>,
    /// The Gate's hourly budget per agent.
    pub hour_budget: Arc<Mutex<crate::gate::HourBudget>>,
    /// Fires after anything changes the world, so sessions push what is new.
    pub changed: tokio::sync::broadcast::Sender<()>,
    /// Origins the browser client may call from (`MOONDIAL_CORS_ORIGINS`, comma-separated).
    pub cors_origins: Vec<String>,
    /// Where the event log lives: `PostgreSQL`, `memory`, or a directory.
    pub log_where: String,
}

impl Shared {
    pub(crate) fn is_admin(&self, token: Option<&str>) -> bool {
        matches!((&self.admin_token, token), (Some(t), Some(given)) if !t.is_empty() && t == given)
    }
}

fn split_args(verb: &str, rest: &str) -> Value {
    if verb == "observe" {
        // JSON has spaces in it: the whole rest of the line is the one argument.
        return Value::Array(vec![Value::String(rest.to_string())]);
    }
    Value::Array(
        rest.split_whitespace()
            .map(|s| Value::String(s.to_string()))
            .collect(),
    )
}

fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 32
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(
            "login <name> <passphrase>: name is letters, digits, - and _ only, up to 32.".into(),
        );
    }
    Ok(())
}

fn help_text(world: &World, kind: ActorKind) -> String {
    let mut lines = vec!["Commands:".to_string()];
    for s in world.commands_for(kind) {
        let params: Vec<String> = s
            .params
            .iter()
            .map(|p| {
                if p.required {
                    format!("<{}>", p.name)
                } else {
                    format!("[{}]", p.name)
                }
            })
            .collect();
        lines.push(format!(
            "  {:<11} {:<24} {}",
            s.verb(),
            params.join(" "),
            s.description
        ));
    }
    lines.push(
        "  observe <json>  Ask the world a question as a query (a read; not speech).".to_string(),
    );
    lines.push(
        "  clear [on|off]  Show replies as the CLEAR the world wrote, or as prose.".to_string(),
    );
    lines.push("  quit".to_string());
    lines.join("\n")
}

fn run_line(
    shared: &Shared,
    actor: &mut Option<(String, ActorKind)>,
    raw: &mut bool,
    line: &str,
) -> Result<String, String> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(String::new());
    }
    let (verb, rest) = line
        .split_once(char::is_whitespace)
        .map(|(v, r)| (v, r.trim()))
        .unwrap_or((line, ""));
    let mut world = shared
        .world
        .lock()
        .map_err(|_| "world lock poisoned".to_string())?;
    match verb {
        "login" | "arrive" => {
            let mut parts = rest.splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or("").trim();
            let passphrase = parts.next().unwrap_or("").trim();
            valid_name(name)?;
            let fresh = !world
                .store
                .entities()
                .any(|e| e.kind == "citizen" && e.owner.as_deref() == Some(name));
            disremembered::login(&mut world, name, "terminal", passphrase)
                .map_err(|e| e.to_string())?;
            *actor = Some((name.to_string(), ActorKind::Citizen));
            Ok(if fresh {
                format!("Welcome, {name}. Your passphrase is set; you will need it next time. You are at the Threshold. Type help.")
            } else {
                format!("Welcome back, {name}. Type look.")
            })
        }
        "admin" => {
            let (Some((name, _)), true) = (actor.clone(), shared.is_admin(Some(rest))) else {
                return Err("admin <token>: log in first, and the token must match.".into());
            };
            *actor = Some((name.clone(), ActorKind::Admin));
            Ok(format!("{name} is now an admin for this session."))
        }
        "help" => {
            let kind = actor.as_ref().map(|a| a.1).unwrap_or(ActorKind::Citizen);
            Ok(if actor.is_none() {
                format!(
                    "Not logged in. Type: login <name> <passphrase>\n{}",
                    help_text(&world, kind)
                )
            } else {
                help_text(&world, kind)
            })
        }
        "clear" => {
            // Show replies as the CLEAR the world wrote, or as prose.
            *raw = match rest {
                "on" => true,
                "off" => false,
                _ => !*raw,
            };
            Ok(if *raw {
                "Replies are shown as CLEAR. Type clear off for prose.".into()
            } else {
                "Replies are shown as prose. Type clear on for CLEAR.".into()
            })
        }
        "observe" => {
            // A read, not speech: the query runs outside the gate.
            let Some((name, kind)) = actor.clone() else {
                return Err("Not logged in. Type: login <name> <passphrase>".into());
            };
            let ctx = CallContext { actor: name, kind };
            match world.call(&ctx, verb, &split_args(verb, rest)) {
                Ok(out) => Ok(out.summary),
                Err(e) => Err(e.to_string()),
            }
        }
        _ => {
            // Everything else is speech: the line becomes a one-statement REQUEST and
            // goes through the gate, like an agent's message.
            let Some((name, kind)) = actor.clone() else {
                return Err("Not logged in. Type: login <name> <passphrase>".into());
            };
            // A line that already is a message (pasted whole, flattened by the
            // terminal) goes in as written.
            let pasted = crate::gate::is_message(line);
            let text = if pasted {
                crate::gate::reflow(line)
            } else {
                crate::gate::compile(&name, &world.id.0, line, world.now(), &shared.checker)
            };
            let mut budget = shared
                .hour_budget
                .lock()
                .map_err(|_| "budget lock poisoned".to_string())?;
            let passage = crate::gate::pass(
                &mut world,
                &shared.checker,
                &mut budget,
                &crate::disremembered::ask::Asks,
                &name,
                kind,
                crate::gate::Via::Text,
                !pasted,
                &text,
            );
            let shown = if *raw {
                passage.reply.trim_end().to_string()
            } else {
                passage.prose
            };
            if passage.ok {
                Ok(shown)
            } else {
                Err(shown)
            }
        }
    }
}

/// Narrative events addressed to `actor` after `cursor`, and the new cursor. What the
/// world did to you while you were not typing; the text port prints it before the
/// reply to your next command.
fn narrative_since(shared: &Shared, actor: &str, cursor: &mut u64) -> Vec<String> {
    let Ok(w) = shared.world.lock() else {
        return Vec::new();
    };
    let head = w.log.head(&w.id).ok().flatten().map(|s| s.0).unwrap_or(0);
    if *cursor == 0 {
        *cursor = head;
        return Vec::new();
    }
    let lines: Vec<String> = w
        .log
        .read(&w.id, Cursor { after: *cursor }, 500)
        .unwrap_or_default()
        .iter()
        .filter(|e| e.kind == crate::disremembered::NARRATIVE && e.payload["to"] == actor)
        .filter_map(|e| e.payload["text"].as_str().map(|t| format!("* {t}")))
        .collect();
    *cursor = head;
    lines
}

async fn handle_tcp(shared: Shared, stream: TcpStream) {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    let _ = w
        .write_all(b"Disremembered on Moondial (pre-alpha). Type: login <name> <passphrase>\n> ")
        .await;
    let mut actor: Option<(String, ActorKind)> = None;
    let mut cursor: u64 = 0;
    let mut raw = false;
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim() == "quit" {
            let _ = w.write_all(b"The world goes on without you.\n").await;
            break;
        }
        let mut out = match actor.as_ref() {
            Some((name, _)) => narrative_since(&shared, name, &mut cursor),
            None => Vec::new(),
        };
        let reply = match run_line(&shared, &mut actor, &mut raw, &line) {
            Ok(s) => s,
            Err(e) => format!("! {e}"),
        };
        if cursor == 0 {
            if let Some((name, _)) = actor.as_ref() {
                narrative_since(&shared, name, &mut cursor); // a fresh login starts here
            }
        }
        out.push(reply);
        if w.write_all(format!("{}\n> ", out.join("\n")).as_bytes())
            .await
            .is_err()
        {
            break;
        }
    }
}

pub async fn serve_tcp(shared: Shared, addr: String) {
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("tcp: cannot bind {addr}: {e}");
            return;
        }
    };
    eprintln!("tcp: listening on {addr}");
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                eprintln!("tcp: connection from {peer}");
                tokio::spawn(handle_tcp(shared.clone(), stream));
            }
            Err(e) => eprintln!("tcp: accept failed: {e}"),
        }
    }
}

#[derive(Deserialize)]
struct CommandBody {
    actor: String,
    #[serde(default)]
    passphrase: String,
    command: String,
    #[serde(default)]
    args: Value,
}

fn err_json(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"ok": false, "error": msg.into()})))
}

pub(crate) type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn lock(s: &Shared) -> Result<std::sync::MutexGuard<'_, World>, (StatusCode, Json<Value>)> {
    s.world
        .lock()
        .map_err(|_| err_json(StatusCode::INTERNAL_SERVER_ERROR, "world lock poisoned"))
}

async fn health(State(s): State<Shared>) -> Reply {
    let w = lock(&s)?;
    let head = w.log.head(&w.id).ok().flatten().map(|x| x.0).unwrap_or(0);
    let now = w.now();
    Ok(Json(
        json!({"ok": true, "world": w.id.0, "events": head, "log": s.log_where, "uptime_seconds": now.since(w.started).0 / 1000, "now": now.0}),
    ))
}

async fn commands(State(s): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Reply {
    let kind = match q.get("kind").map(String::as_str) {
        Some("admin") => ActorKind::Admin,
        Some("familiar") => ActorKind::Familiar,
        _ => ActorKind::Citizen,
    };
    let w = lock(&s)?;
    Ok(Json(json!({"commands": w.commands_for(kind)})))
}

/// `GET /v0/citizens` (admin): every citizen, with when they first came and by what
/// channel, their arrivals (an arrival is the first activity after ten minutes away),
/// what went through the Gate for them and how it fared, and what they have dug. The
/// operator's view of who is here; `since` (ms since the epoch) keeps the recently
/// active.
async fn citizens(
    State(s): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Reply {
    let token = headers
        .get("x-moondial-admin")
        .and_then(|v| v.to_str().ok());
    if !s.is_admin(token) {
        return Err(err_json(StatusCode::FORBIDDEN, "admin token required"));
    }
    let since: i64 = q.get("since").and_then(|v| v.parse().ok()).unwrap_or(0);
    let w = lock(&s)?;
    Ok(Json(json!({"citizens": citizens_report(&w, since)})))
}

/// The citizens report, from the log.
pub(crate) fn citizens_report(w: &World, since: i64) -> Vec<Value> {
    #[derive(Default)]
    struct Row {
        origin: String,
        first_seen: i64,
        last_seen: i64,
        arrivals: u64,
        origins: std::collections::BTreeMap<String, u64>,
        messages: u64,
        passed: u64,
        via: std::collections::BTreeMap<String, u64>,
        compiled: u64,
        last_message_at: i64,
        rooms_dug: u64,
    }
    let mut rows: std::collections::BTreeMap<String, Row> = Default::default();
    for e in w.store.entities() {
        if e.kind == "citizen" {
            if let Some(owner) = &e.owner {
                let r = rows.entry(owner.clone()).or_default();
                r.origin = e
                    .components
                    .get(crate::disremembered::CITIZEN)
                    .and_then(|c| c.fields.get("origin"))
                    .and_then(|f| match f {
                        moondial_store::Field::Plain(Value::String(s)) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
            }
        }
        if e.kind == "room" {
            if let Some(owner) = &e.owner {
                rows.entry(owner.clone()).or_default().rooms_dug += 1;
            }
        }
    }
    for e in w.log.read_all(&w.id, Cursor::START).unwrap_or_default() {
        match e.kind.as_str() {
            "store.entity_created" if e.payload["kind"] == "citizen" => {
                if let Some(owner) = e.payload["owner"].as_str() {
                    let r = rows.entry(owner.to_string()).or_default();
                    if r.first_seen == 0 {
                        r.first_seen = e.at.0;
                    }
                    r.last_seen = r.last_seen.max(e.at.0);
                }
            }
            k if k == crate::world::ARRIVED => {
                if let Some(actor) = e.payload["actor"].as_str() {
                    let r = rows.entry(actor.to_string()).or_default();
                    r.arrivals += 1;
                    r.last_seen = r.last_seen.max(e.at.0);
                    *r.origins
                        .entry(e.payload["origin"].as_str().unwrap_or("?").to_string())
                        .or_default() += 1;
                }
            }
            "gate.message" => {
                if let Some(from) = e.payload["from"].as_str() {
                    let r = rows.entry(from.to_string()).or_default();
                    r.messages += 1;
                    if e.payload["verdict"] == "PASS" {
                        r.passed += 1;
                    }
                    if e.payload["compiled"] == true {
                        r.compiled += 1;
                    }
                    *r.via
                        .entry(e.payload["via"].as_str().unwrap_or("?").to_string())
                        .or_default() += 1;
                    r.last_seen = r.last_seen.max(e.at.0);
                    r.last_message_at = e.at.0;
                }
            }
            _ => {}
        }
    }
    let mut out: Vec<(i64, Value)> = rows
        .into_iter()
        .filter(|(_, r)| r.last_seen >= since)
        .map(|(name, r)| {
            (
                r.last_seen,
                json!({
                    "name": name, "origin": r.origin, "first_seen": r.first_seen, "last_seen": r.last_seen,
                    "arrivals": r.arrivals, "origins": r.origins, "messages": r.messages, "passed": r.passed,
                    "written": r.messages - r.compiled, "via": r.via, "last_message_at": r.last_message_at,
                    "rooms_dug": r.rooms_dug,
                }),
            )
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|(_, v)| v).collect()
}

/// `GET /v0/vocabulary`: the CLEAR vocabulary the Gate checks against, merged (base
/// and world), as JSON. An editor compiles its own checker from this, so the words it
/// offers and the words the world accepts are one list.
async fn vocabulary(State(s): State<Shared>) -> Reply {
    Ok(Json(
        json!({"name": s.world.lock().map(|w| w.id.0.clone()).unwrap_or_default(), "vocabulary": crate::gate::disremembered_vocabulary()}),
    ))
}

/// `GET /v0/schema`: what an observation can ask for. The component schemas (fields,
/// visibility, plain or timed), the relations, the named roots and their arguments
/// with the public values a root argument may take, the entity kinds the world holds,
/// and the limits a query is checked against. A client builds queries from this
/// instead of knowing the world.
async fn schema(State(s): State<Shared>) -> Reply {
    schema_value(&s).map(Json)
}

/// The schema, as `GET /v0/schema` serves it.
pub(crate) fn schema_value(s: &Shared) -> Result<Value, (StatusCode, Json<Value>)> {
    let w = lock(s)?;
    let registry = w.store.registry();
    let mut roots = crate::disremembered::roots();
    if let Some(list) = roots.as_array_mut() {
        for root in list {
            let Some(from) = root.get("values").cloned() else {
                continue;
            };
            let (kind, component, field) = (
                from["kind"].as_str().unwrap_or(""),
                from["component"].as_str().unwrap_or(""),
                from["field"].as_str().unwrap_or(""),
            );
            let public = registry
                .get(component)
                .and_then(|c| c.get(field))
                .is_some_and(|f| f.visibility == moondial_store::Visibility::Public);
            let mut values: Vec<Value> = Vec::new();
            if public {
                for e in w.store.entities().filter(|e| e.kind == kind) {
                    if let Some(moondial_store::Field::Plain(v)) = e
                        .components
                        .get(component)
                        .and_then(|c| c.fields.get(field))
                    {
                        values.push(v.clone());
                    }
                }
            }
            root["values"] = Value::Array(values);
        }
    }
    let mut kinds: Vec<&str> = w.store.entities().map(|e| e.kind.as_str()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    Ok(json!({
        "world": w.id.0,
        "components": registry.schemas().collect::<Vec<_>>(),
        "relations": registry.relations().collect::<Vec<_>>(),
        "roots": roots,
        "kinds": kinds,
        "limits": s.limits,
    }))
}

#[derive(Deserialize)]
struct CheckBody {
    message: String,
}

/// Most bytes `/v0/check` reads; the vocabulary's own message limit is smaller.
const MAX_CHECK_BYTES: usize = 16 * 1024;

/// `POST /v0/check {message}`: the checker's verdict on a message, without sending it.
/// What a browser without WebAssembly uses; the same result the editor computes locally.
async fn check(State(s): State<Shared>, Json(body): Json<CheckBody>) -> Reply {
    if body.message.len() > MAX_CHECK_BYTES {
        return Err(err_json(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("a message is at most {MAX_CHECK_BYTES} bytes"),
        ));
    }
    Ok(Json(s.checker.check(&body.message).to_json()))
}

/// Log a citizen in for a session: `Ok(fresh)` or a message.
pub(crate) fn login_for_session(
    w: &mut World,
    actor: &str,
    passphrase: &str,
) -> Result<bool, String> {
    valid_name(actor)?;
    let fresh = !w
        .store
        .entities()
        .any(|e| e.kind == "citizen" && e.owner.as_deref() == Some(actor));
    disremembered::login(w, actor, "session", passphrase).map_err(|e| e.to_string())?;
    Ok(fresh)
}

/// Log a citizen in for one HTTP call, with the status codes a browser client needs.
pub(crate) fn http_login(
    w: &mut World,
    actor: &str,
    passphrase: &str,
    origin: &str,
) -> Result<bool, (StatusCode, Json<Value>)> {
    valid_name(actor).map_err(|e| err_json(StatusCode::BAD_REQUEST, e))?;
    let fresh = !w
        .store
        .entities()
        .any(|e| e.kind == "citizen" && e.owner.as_deref() == Some(actor));
    disremembered::login(w, actor, origin, passphrase).map_err(|e| match e {
        disremembered::LoginError::Internal(_) => {
            err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
        disremembered::LoginError::WrongPassphrase { .. } => {
            err_json(StatusCode::UNAUTHORIZED, e.to_string())
        }
        _ => err_json(StatusCode::BAD_REQUEST, e.to_string()),
    })?;
    Ok(fresh)
}

#[derive(Deserialize)]
struct LoginBody {
    actor: String,
    #[serde(default)]
    passphrase: String,
}

/// Check a name and passphrase (creating the citizen on first login) without running a
/// command. The browser client calls this first so a bad passphrase is a clear message
/// rather than a failed `look`.
async fn login(State(s): State<Shared>, Json(body): Json<LoginBody>) -> Reply {
    let mut w = lock(&s)?;
    let fresh = http_login(&mut w, &body.actor, &body.passphrase, "http")?;
    Ok(Json(
        json!({"ok": true, "actor": body.actor, "fresh": fresh}),
    ))
}

async fn command(
    State(s): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<CommandBody>,
) -> Reply {
    let token = headers
        .get("x-moondial-admin")
        .and_then(|v| v.to_str().ok());
    let kind = if s.is_admin(token) {
        ActorKind::Admin
    } else {
        ActorKind::Citizen
    };
    let mut w = lock(&s)?;
    http_login(&mut w, &body.actor, &body.passphrase, "http")?;
    if body.command == "observe" {
        // A read, not speech: runs outside the gate.
        let ctx = CallContext {
            actor: body.actor.clone(),
            kind,
        };
        let outcome = w.call(&ctx, &body.command, &body.args);
        drop(w);
        return match outcome {
            Ok(out) => Ok(Json(
                json!({"ok": true, "summary": out.summary, "result": out.result}),
            )),
            Err(e) => Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"ok": false, "error": e.to_string(), "detail": e})),
            )),
        };
    }
    let mut budget = s
        .hour_budget
        .lock()
        .map_err(|_| err_json(StatusCode::INTERNAL_SERVER_ERROR, "budget lock poisoned"))?;
    let passage = compiled_passage(
        &mut w,
        &s,
        &mut budget,
        &body.actor,
        kind,
        crate::gate::Via::Http,
        &body.command,
        &body.args,
    );
    drop(budget);
    drop(w);
    let _ = s.changed.send(());
    Ok(Json(passage_json(&passage)))
}

/// Compile a command and its arguments into a REQUEST and pass it through the gate.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compiled_passage(
    w: &mut World,
    s: &Shared,
    budget: &mut crate::gate::HourBudget,
    actor: &str,
    kind: ActorKind,
    via: crate::gate::Via,
    command: &str,
    args: &Value,
) -> crate::gate::Passage {
    let words: Vec<String> = args
        .as_array()
        .map(|a| {
            a.iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    let line = if words.is_empty() {
        command.to_string()
    } else {
        format!("{command} {}", words.join(" "))
    };
    let pasted = crate::gate::is_message(&line);
    let text = if pasted {
        crate::gate::reflow(&line)
    } else {
        crate::gate::compile(actor, &w.id.0, &line, w.now(), &s.checker)
    };
    crate::gate::pass(
        w,
        &s.checker,
        budget,
        &crate::disremembered::ask::Asks,
        actor,
        kind,
        via,
        !pasted,
        &text,
    )
}

/// A compiled passage as the command reply: `ok`, the prose as `summary`, the first
/// statement's result, and the CLEAR the world wrote as `reply`.
pub(crate) fn passage_json(p: &crate::gate::Passage) -> Value {
    let result = p
        .outcomes
        .first()
        .map(|o| o.result.clone())
        .unwrap_or(Value::Null);
    if p.ok {
        json!({"ok": true, "summary": p.prose, "result": result, "message": p.message, "reply": p.reply, "verdict": p.verdict})
    } else {
        json!({"ok": false, "error": p.prose, "detail": result, "message": p.message, "reply": p.reply, "verdict": p.verdict, "findings": p.findings})
    }
}

#[derive(Deserialize)]
struct ObserveBody {
    actor: String,
    #[serde(default)]
    passphrase: String,
    query: crate::observe::Request,
}

/// `POST /v0/observe {actor, passphrase, query}`: the observation query, with the
/// legal commands and an event cursor. Errors carry a `reason` a client can act on.
async fn observe_http(
    State(s): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<ObserveBody>,
) -> Reply {
    let token = headers
        .get("x-moondial-admin")
        .and_then(|v| v.to_str().ok());
    let admin = s.is_admin(token);
    let mut w = lock(&s)?;
    http_login(&mut w, &body.actor, &body.passphrase, "http")?;
    let now = w.now();
    s.budget
        .lock()
        .map_err(|_| err_json(StatusCode::INTERNAL_SERVER_ERROR, "budget lock poisoned"))?
        .spend(&body.actor, now)
        .map_err(|e| {
            (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"ok": false, "error": e.to_string(), "detail": e})),
            )
        })?;
    let viewer = if admin {
        Viewer::admin()
    } else {
        Viewer::actor(&body.actor)
    };
    let kind = if admin {
        ActorKind::Admin
    } else {
        ActorKind::Citizen
    };
    match w.observe(&viewer, kind, &body.query, &s.limits) {
        Ok(obs) => Ok(Json(json!({"ok": true, "observation": obs}))),
        Err(e) => {
            let status = match e {
                moondial_query::QueryError::RootNotVisible
                | moondial_query::QueryError::UnknownRoot { .. } => StatusCode::NOT_FOUND,
                _ => StatusCode::UNPROCESSABLE_ENTITY,
            };
            Err((
                status,
                Json(json!({"ok": false, "error": e.to_string(), "detail": e})),
            ))
        }
    }
}

#[derive(Deserialize)]
struct GateBody {
    actor: String,
    #[serde(default)]
    passphrase: String,
    message: String,
}

/// `POST /v0/gate {actor, passphrase, message}`: one CLEAR message through the Gate.
/// The reply is a CLEAR REPORT; the JSON around it carries the verdict, the score, the
/// findings and the budget left. Everything the world did is in the reply's FACT lines.
async fn gate_http(State(s): State<Shared>, Json(body): Json<GateBody>) -> Reply {
    let mut w = lock(&s)?;
    http_login(&mut w, &body.actor, &body.passphrase, "gate")?;
    let mut budget = s
        .hour_budget
        .lock()
        .map_err(|_| err_json(StatusCode::INTERNAL_SERVER_ERROR, "budget lock poisoned"))?;
    let passage = crate::gate::pass(
        &mut w,
        &s.checker,
        &mut budget,
        &crate::disremembered::ask::Asks,
        &body.actor,
        ActorKind::Citizen,
        crate::gate::Via::Gate,
        false,
        &body.message,
    );
    drop(budget);
    drop(w);
    let _ = s.changed.send(());
    Ok(Json(json!({
        "ok": true,
        "verdict": passage.verdict,
        "score": passage.score,
        "acted": passage.acted,
        "reply": passage.reply,
        "findings": passage.findings,
        "budget_remaining": passage.budget_remaining,
    })))
}

async fn view(
    State(s): State<Shared>,
    Path(id): Path<u64>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Reply {
    let token = headers
        .get("x-moondial-admin")
        .and_then(|v| v.to_str().ok());
    let viewer = if s.is_admin(token) {
        Viewer::admin()
    } else {
        match q.get("actor") {
            Some(a) => Viewer::actor(a.clone()),
            None => Viewer::anonymous(),
        }
    };
    let w = lock(&s)?;
    let now = w.now();
    w.store
        .view(EntityId(id), &viewer, now)
        .map(Json)
        .ok_or_else(|| err_json(StatusCode::NOT_FOUND, format!("no entity {id}")))
}

async fn log(State(s): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Reply {
    let after: u64 = q.get("after").and_then(|v| v.parse().ok()).unwrap_or(0);
    let limit: usize = q
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100)
        .min(1000);
    let w = lock(&s)?;
    let events = w
        .log
        .read(&w.id, Cursor { after }, limit)
        .unwrap_or_default();
    Ok(Json(json!({"events": events})))
}

pub fn router(shared: Shared) -> Router {
    use axum::http::{header, HeaderValue, Method};
    use tower_http::cors::CorsLayer;
    let origins: Vec<HeaderValue> = shared
        .cors_origins
        .iter()
        .filter_map(|o| HeaderValue::from_str(o.trim()).ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([
            header::CONTENT_TYPE,
            HeaderName::from_static("x-moondial-admin"),
        ])
        .max_age(std::time::Duration::from_secs(3600));
    Router::new()
        .route("/health", get(health))
        .route("/v0/commands", get(commands))
        .route("/v0/login", post(login))
        .route("/v0/command", post(command))
        .route("/v0/observe", post(observe_http))
        .route("/v0/gate", post(gate_http))
        .route("/v0/vocabulary", get(vocabulary))
        .route("/v0/schema", get(schema))
        .route("/v0/citizens", get(citizens))
        .route(
            "/mcp",
            post(crate::mcp::post)
                .get(crate::mcp::get)
                .delete(crate::mcp::delete),
        )
        .route("/v0/check", post(check))
        .route("/v0/session", get(crate::session::upgrade))
        .route("/v0/view/{id}", get(view))
        .route("/v0/log", get(log))
        .layer(cors)
        .with_state(shared)
}

pub async fn serve_http(shared: Shared, addr: String) {
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("http: cannot bind {addr}: {e}");
            return;
        }
    };
    eprintln!("http: listening on {addr}");
    if let Err(e) = axum::serve(listener, router(shared)).await {
        eprintln!("http: {e}");
    }
}
