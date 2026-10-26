//! The two listeners of the pre-alpha driver.
//!
//! - A line protocol on TCP, for a terminal (`nc`, `telnet`, or the text client to come).
//!   `login <name>` arrives as a citizen; every other line is `<verb> [args...]`.
//! - JSON over HTTP, for tools and tests: `/health`, `/v0/commands`, `/v0/command`,
//!   `/v0/view/{id}`, `/v0/log`.
//!
//! Both go through the same registry and the same `World::call`, so a command means the
//! same thing on either. There is no authentication in the slice beyond an admin token;
//! a name is an identity. That is fine for a test server and wrong for anything else,
//! and the world card says so.

use crate::entropic;
use crate::world::World;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use moondial_log::Cursor;
use moondial_registry::{ActorKind, CallContext, CommandError};
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
}

impl Shared {
    fn is_admin(&self, token: Option<&str>) -> bool {
        matches!((&self.admin_token, token), (Some(t), Some(given)) if !t.is_empty() && t == given)
    }
}

fn split_args(rest: &str) -> Value {
    Value::Array(
        rest.split_whitespace()
            .map(|s| Value::String(s.to_string()))
            .collect(),
    )
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
    lines.push("  quit".to_string());
    lines.join("\n")
}

fn run_line(
    shared: &Shared,
    actor: &mut Option<(String, ActorKind)>,
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
            let name = rest.split_whitespace().next().unwrap_or("");
            if name.is_empty()
                || name.len() > 32
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err("login <name>: letters, digits, - and _ only, up to 32.".into());
            }
            entropic::ensure_citizen(&mut world, name, "terminal")?;
            *actor = Some((name.to_string(), ActorKind::Citizen));
            Ok(format!("Welcome, {name}. You are at the Gate. Type help."))
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
                    "Not logged in. Type: login <name>\n{}",
                    help_text(&world, kind)
                )
            } else {
                help_text(&world, kind)
            })
        }
        _ => {
            let Some((name, kind)) = actor.clone() else {
                return Err("Not logged in. Type: login <name>".into());
            };
            let ctx = CallContext { actor: name, kind };
            match world.call(&ctx, verb, &split_args(rest)) {
                Ok(out) => Ok(out.summary),
                Err(CommandError::Unknown { name }) => {
                    Err(format!("Unknown command '{name}'. Type help."))
                }
                Err(e) => Err(e.to_string()),
            }
        }
    }
}

async fn handle_tcp(shared: Shared, stream: TcpStream) {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    let _ = w
        .write_all(b"Entropic on Moondial (pre-alpha). Type: login <name>\n> ")
        .await;
    let mut actor: Option<(String, ActorKind)> = None;
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim() == "quit" {
            let _ = w.write_all(b"The world goes on without you.\n").await;
            break;
        }
        let reply = match run_line(&shared, &mut actor, &line) {
            Ok(s) => s,
            Err(e) => format!("! {e}"),
        };
        if w.write_all(format!("{reply}\n> ").as_bytes())
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
    command: String,
    #[serde(default)]
    args: Value,
}

fn err_json(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"ok": false, "error": msg.into()})))
}

type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

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
        json!({"ok": true, "world": w.id.0, "events": head, "uptime_seconds": now.since(w.started).0 / 1000, "now": now.0}),
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
    if body.actor.is_empty() || body.actor.len() > 32 {
        return Err(err_json(
            StatusCode::BAD_REQUEST,
            "actor must be 1 to 32 characters",
        ));
    }
    entropic::ensure_citizen(&mut w, &body.actor, "http")
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let ctx = CallContext {
        actor: body.actor.clone(),
        kind,
    };
    match w.call(&ctx, &body.command, &body.args) {
        Ok(out) => Ok(Json(
            json!({"ok": true, "summary": out.summary, "result": out.result}),
        )),
        Err(e) => {
            let status = match e {
                CommandError::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
                CommandError::NotAllowed { .. } => StatusCode::FORBIDDEN,
                CommandError::Unknown { .. } => StatusCode::NOT_FOUND,
                _ => StatusCode::UNPROCESSABLE_ENTITY,
            };
            Err((
                status,
                Json(json!({"ok": false, "error": e.to_string(), "detail": e})),
            ))
        }
    }
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
    Router::new()
        .route("/health", get(health))
        .route("/v0/commands", get(commands))
        .route("/v0/command", post(command))
        .route("/v0/view/{id}", get(view))
        .route("/v0/log", get(log))
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
