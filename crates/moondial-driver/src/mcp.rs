//! The world as an MCP server, at `/mcp` (Streamable HTTP, stateless). An agent that
//! can add a server by URL gains the Gate as a tool: `gate` sends a CLEAR message,
//! `check` runs the checker without sending, `observe` runs an observation query, and
//! `vocabulary`, `schema`, `commands` and `invitation` describe the world. Credentials
//! travel as tool arguments, as on the HTTP API: a name and a passphrase, set on first
//! use. Nothing here bypasses the Gate; `gate` is `POST /v0/gate` with a different
//! envelope, and the budget and the log are the same.
//!
//! The subset of MCP served: `initialize`, `ping`, `tools/list`, `tools/call`, and the
//! notifications a client sends, which are accepted and ignored. Every reply is a
//! single JSON body; the server opens no event streams, so `GET /mcp` is 405.

use crate::net::Shared;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use moondial_registry::ActorKind;
use moondial_store::Viewer;
use serde_json::{json, Value};

const PROTOCOL: &str = "2025-06-18";
const INVITATION: &str = include_str!("../../../site/sites/agentscanplay/invitation.txt");

fn instructions(world: &str) -> String {
    format!(
        "This server is the Gate of {world}, a game world on Moondial where humans and agents play under the same rules. \
You act by sending CLEAR messages with the `gate` tool: a header (FROM: your name, TO: {world}, INTENT: REQUEST or QUERY, REF: anything) \
and typed statements, one per line. A REQUEST carries [DO] lines: `[DO] your-name MUST <verb> <arguments>.` where the verb and its \
arguments come from `vocabulary` (approved_verbs, each with its signature). A QUERY carries [ASK] lines that match the vocabulary's \
question patterns. Run `check` first; a message that fails is refused with the rule it broke. `observe` shows you the world as a \
query (see `schema`). Your name and passphrase are set on first use and are yours after. Sixty messages an hour through `gate`. \
Every message and reply is in the public event log, word for word. The invitation below is a proposal you may decline.\n\n{INVITATION}"
    )
}

fn tools() -> Value {
    json!([
        {
            "name": "invitation",
            "description": "The world's invitation to any agent, as a CLEAR message, and how to arrive.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        },
        {
            "name": "vocabulary",
            "description": "The CLEAR vocabulary the Gate checks against: approved verbs with their signatures (the arguments each takes), question patterns, glossary, units, connectors.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        },
        {
            "name": "commands",
            "description": "The commands a citizen may call, with parameters and descriptions; the same verbs as the vocabulary's signatures.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        },
        {
            "name": "schema",
            "description": "What an observation can ask for: component schemas (fields, visibility, plain or timed), relations, named roots and their arguments, entity kinds, limits.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        },
        {
            "name": "check",
            "description": "Run the CLEAR checker on a message without sending it: verdict, Legibility score, findings by rule id, and the parsed message. Free; no account needed.",
            "inputSchema": {"type": "object", "properties": {"message": {"type": "string", "description": "The CLEAR message text."}}, "required": ["message"], "additionalProperties": false}
        },
        {
            "name": "gate",
            "description": "Send one CLEAR message to the world as yourself. The reply is a CLEAR REPORT: FACT lines cite event ids, the narrator's prose arrives as QUOTE, a refusal cites the rule or precondition. Costs one of sixty messages an hour (a failed one a quarter).",
            "inputSchema": {"type": "object", "properties": {
                "actor": {"type": "string", "description": "Your name: letters, digits, - and _, up to 32. The FROM of your message must match."},
                "passphrase": {"type": "string", "description": "Set on first use (8+ characters), required after."},
                "message": {"type": "string", "description": "The CLEAR message."}
            }, "required": ["actor", "passphrase", "message"], "additionalProperties": false}
        },
        {
            "name": "observe",
            "description": "See the world as yourself through an observation query: a root (self, here, window, palace, {\"room\": key}, {\"entity\": id}), components and fields to select, relations to follow (contents, in, exits), where filters, limit; \"timed\": \"curve\" returns timed fields as curves. You see exactly the fields you asked for, after visibility. Thirty queries per ten seconds.",
            "inputSchema": {"type": "object", "properties": {
                "actor": {"type": "string"},
                "passphrase": {"type": "string"},
                "query": {"type": "object", "description": "The query document; see schema."},
                "after": {"type": "integer", "description": "An event cursor: also return the events you may see after it."}
            }, "required": ["actor", "passphrase", "query"], "additionalProperties": false}
        }
    ])
}

/// A JSON-RPC error reply.
fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// A tool result carrying text and, when there is one, a structured form.
fn tool_text(text: String, structured: Option<Value>, is_error: bool) -> Value {
    let mut r = json!({"content": [{"type": "text", "text": text}], "isError": is_error});
    if let Some(s) = structured {
        r["structuredContent"] = s;
    }
    r
}

fn tool_json(v: Value) -> Value {
    let text = serde_json::to_string_pretty(&v).unwrap_or_default();
    tool_text(text, Some(v), false)
}

fn tool_error(text: impl Into<String>) -> Value {
    tool_text(text.into(), None, true)
}

fn arg_str<'a>(args: &'a Value, name: &str) -> Result<&'a str, Value> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| tool_error(format!("{name} is required and must be a string")))
}

fn call(s: &Shared, name: &str, args: &Value) -> Value {
    match name {
        "invitation" => {
            let world = s.world.lock().map(|w| w.id.0.clone()).unwrap_or_default();
            tool_text(instructions(&world), None, false)
        }
        "vocabulary" => tool_json(json!({"vocabulary": crate::gate::disremembered_vocabulary()})),
        "commands" => match s.world.lock() {
            Ok(w) => tool_json(json!({"commands": w.commands_for(ActorKind::Citizen)})),
            Err(_) => tool_error("the world is unavailable"),
        },
        "schema" => match crate::net::schema_value(s) {
            Ok(v) => tool_json(v),
            Err((_, Json(e))) => tool_error(e["error"].as_str().unwrap_or("schema failed")),
        },
        "check" => match arg_str(args, "message") {
            Ok(message) => tool_json(s.checker.check(message).to_json()),
            Err(e) => e,
        },
        "gate" => {
            let (actor, passphrase, message) = match (
                arg_str(args, "actor"),
                arg_str(args, "passphrase"),
                arg_str(args, "message"),
            ) {
                (Ok(a), Ok(p), Ok(m)) => (a, p, m),
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return e,
            };
            let Ok(mut w) = s.world.lock() else {
                return tool_error("the world is unavailable");
            };
            if let Err((_, Json(e))) = crate::net::http_login(&mut w, actor, passphrase, "mcp") {
                return tool_error(e["error"].as_str().unwrap_or("login failed"));
            }
            let Ok(mut budget) = s.hour_budget.lock() else {
                return tool_error("the budget is unavailable");
            };
            let p = crate::gate::pass(
                &mut w,
                &s.checker,
                &mut budget,
                &crate::disremembered::ask::Asks,
                actor,
                ActorKind::Citizen,
                crate::gate::Via::Gate,
                false,
                message,
            );
            drop(budget);
            drop(w);
            let _ = s.changed.send(());
            let structured = json!({
                "verdict": p.verdict, "score": p.score, "acted": p.acted, "ok": p.ok,
                "reply": p.reply, "prose": p.prose, "findings": p.findings,
                "budget_remaining": p.budget_remaining,
            });
            tool_text(p.reply.clone(), Some(structured), false)
        }
        "observe" => {
            let (actor, passphrase) = match (arg_str(args, "actor"), arg_str(args, "passphrase")) {
                (Ok(a), Ok(p)) => (a, p),
                (Err(e), _) | (_, Err(e)) => return e,
            };
            let mut query = args.get("query").cloned().unwrap_or(Value::Null);
            if !query.is_object() {
                return tool_error("query must be an object; see schema");
            }
            if let Some(after) = args.get("after").and_then(Value::as_u64) {
                query["after"] = json!(after);
            }
            let req: crate::observe::Request = match serde_json::from_value(query) {
                Ok(r) => r,
                Err(e) => {
                    return tool_error(format!("the query is not the shape the world reads: {e}"))
                }
            };
            let Ok(mut w) = s.world.lock() else {
                return tool_error("the world is unavailable");
            };
            if let Err((_, Json(e))) = crate::net::http_login(&mut w, actor, passphrase, "mcp") {
                return tool_error(e["error"].as_str().unwrap_or("login failed"));
            }
            let now = w.now();
            if let Ok(mut b) = s.budget.lock() {
                if let Err(e) = b.spend(actor, now) {
                    return tool_error(e.to_string());
                }
            }
            match w.observe(&Viewer::actor(actor), ActorKind::Citizen, &req, &s.limits) {
                Ok(obs) => tool_json(serde_json::to_value(obs).unwrap_or_default()),
                Err(e) => tool_error(format!(
                    "{}: {e}",
                    serde_json::to_value(&e)
                        .ok()
                        .and_then(|v| v["reason"].as_str().map(str::to_string))
                        .unwrap_or_default()
                )),
            }
        }
        other => tool_error(format!("no tool named {other}")),
    }
}

fn handle(s: &Shared, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    // A notification or a response from the client has no reply.
    let id = id?;
    if msg.get("result").is_some() || msg.get("error").is_some() {
        return None;
    }
    Some(match method {
        "initialize" => {
            let world = s.world.lock().map(|w| w.id.0.clone()).unwrap_or_default();
            rpc_result(
                id,
                json!({
                    "protocolVersion": PROTOCOL,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": format!("{world} on Moondial"), "version": env!("CARGO_PKG_VERSION")},
                    "instructions": instructions(&world),
                }),
            )
        }
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(id, json!({"tools": tools()})),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            rpc_result(id, call(s, name, &args))
        }
        "resources/list" | "prompts/list" => {
            rpc_result(id, json!({"resources": [], "prompts": []}))
        }
        _ => rpc_error(id, -32601, format!("method not found: {method}")),
    })
}

/// `POST /mcp`: one JSON-RPC message or a batch; the replies as one JSON body, or 202
/// when there is nothing to reply to.
pub async fn post(State(s): State<Shared>, headers: HeaderMap, body: String) -> Response {
    let parsed: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(rpc_error(Value::Null, -32700, format!("parse error: {e}"))),
            )
                .into_response()
        }
    };
    let _ = headers; // the protocol version header is accepted at any version we know
    let replies: Vec<Value> = match &parsed {
        Value::Array(batch) => batch.iter().filter_map(|m| handle(&s, m)).collect(),
        one => handle(&s, one).into_iter().collect(),
    };
    if replies.is_empty() {
        return StatusCode::ACCEPTED.into_response();
    }
    if parsed.is_array() {
        Json(Value::Array(replies)).into_response()
    } else {
        Json(replies.into_iter().next().unwrap_or(Value::Null)).into_response()
    }
}

/// `GET /mcp`: this server opens no streams.
pub async fn get() -> Response {
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

/// `DELETE /mcp`: there is no session to end.
pub async fn delete() -> Response {
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disremembered;
    use crate::world::{LogBacking, World};
    use moondial_log::WorldId;
    use moondial_store::Store;
    use moondial_time::{Duration, Timestamp};
    use std::sync::{Arc, Mutex};

    fn shared() -> Shared {
        let mut w = World::open(
            WorldId("disremembered".into()),
            Store::new(disremembered::registry()),
            LogBacking::Memory,
        )
        .unwrap();
        w.fixed_now = Some(Timestamp(1_793_100_000_000));
        disremembered::ensure_world(&mut w).unwrap();
        disremembered::register(
            &mut w,
            disremembered::Curves::with_half_life(Duration::seconds(600)),
        );
        w.tick();
        Shared {
            world: Arc::new(Mutex::new(w)),
            admin_token: None,
            budget: Arc::new(Mutex::new(crate::observe::Budget::new(30, 10_000))),
            limits: moondial_query::Limits::default(),
            checker: Arc::new(crate::gate::checker()),
            hour_budget: Arc::new(Mutex::new(crate::gate::HourBudget::new(60.0, 0.25))),
            changed: tokio::sync::broadcast::channel(8).0,
            cors_origins: vec![],
            log_where: "memory".into(),
        }
    }

    fn rpc(s: &Shared, id: u64, method: &str, params: Value) -> Value {
        handle(
            s,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .expect("a request gets a reply")
    }

    #[test]
    fn the_gate_is_a_tool_and_the_protocol_is_the_subset_a_client_needs() {
        let s = shared();
        let init = rpc(
            &s,
            1,
            "initialize",
            json!({"protocolVersion": "2025-06-18"}),
        );
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL);
        assert!(init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("FROM: disremembered"));
        // Notifications get no reply.
        assert!(handle(
            &s,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )
        .is_none());
        let list = rpc(&s, 2, "tools/list", json!({}));
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "invitation",
                "vocabulary",
                "commands",
                "schema",
                "check",
                "gate",
                "observe"
            ]
        );
        assert_eq!(rpc(&s, 3, "nope", json!({}))["error"]["code"], -32601);

        // check is free and sends nothing; gate logs in, passes the Gate, is budgeted.
        let msg = "FROM: lantern-9\nTO: disremembered\nINTENT: REQUEST\nREF: l9-1\n\n[DO] lantern-9 MUST look.\n";
        let c = rpc(
            &s,
            4,
            "tools/call",
            json!({"name": "check", "arguments": {"message": msg}}),
        );
        assert_eq!(c["result"]["structuredContent"]["verdict"], "PASS");
        assert_eq!(
            s.world
                .lock()
                .unwrap()
                .log
                .head(&WorldId("disremembered".into()))
                .unwrap()
                .map(|h| h.0 > 0),
            Some(true)
        );
        let before = s
            .world
            .lock()
            .unwrap()
            .log
            .head(&WorldId("disremembered".into()))
            .unwrap()
            .unwrap()
            .0;
        let g = rpc(
            &s,
            5,
            "tools/call",
            json!({"name": "gate", "arguments": {"actor": "lantern-9", "passphrase": "a-lantern-passphrase", "message": msg}}),
        );
        let r = &g["result"];
        assert_eq!(r["isError"], false);
        assert_eq!(r["structuredContent"]["acted"], true);
        assert!(r["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("FROM: disremembered\nTO: lantern-9\nINTENT: REPORT"));
        assert_eq!(r["structuredContent"]["budget_remaining"], 59.0);
        let after = s
            .world
            .lock()
            .unwrap()
            .log
            .head(&WorldId("disremembered".into()))
            .unwrap()
            .unwrap()
            .0;
        assert!(after > before, "the message and reply are in the log");

        // A wrong passphrase is a tool error, not a crash; observe sees as the actor.
        let bad = rpc(
            &s,
            6,
            "tools/call",
            json!({"name": "gate", "arguments": {"actor": "lantern-9", "passphrase": "wrong-passphrase", "message": msg}}),
        );
        assert_eq!(bad["result"]["isError"], true);
        let o = rpc(
            &s,
            7,
            "tools/call",
            json!({"name": "observe", "arguments": {"actor": "lantern-9", "passphrase": "a-lantern-passphrase", "query": {"root": "self", "select": {"disremembered.citizen": ["name"]}}}}),
        );
        assert_eq!(
            o["result"]["structuredContent"]["root"]["components"]["disremembered.citizen"]["name"],
            "lantern-9"
        );
        let o = rpc(
            &s,
            8,
            "tools/call",
            json!({"name": "observe", "arguments": {"actor": "lantern-9", "passphrase": "a-lantern-passphrase", "query": {"root": "here", "relations": {"contnets": {}}}}}),
        );
        assert_eq!(o["result"]["isError"], true);
        assert!(o["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("contents"));
    }
}
