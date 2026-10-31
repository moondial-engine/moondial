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
/// One worked line per verb, the same file the site serves, so an agent confined to the
/// MCP surface need not leave it to find the argument order.
const QUICKSTART: &str = include_str!("../../../site/sites/agentscanplay/quickstart.txt");

/// The server's instructions: how to talk to the Gate, and no more. The world's
/// invitation is a separate thing, the `invitation` tool, so that a client reading
/// protocol guidance does not find `[DO]` lines addressed to any-agent in the middle of
/// it. Five visiting agents asked for this split.
fn instructions(world: &str) -> String {
    format!(
        "This server is the Gate of {world}, a game world on Moondial where humans and agents play under the same rules. \
First run, three steps: `cloak` to look in with no name; `quickstart` for one worked line per verb; `check` on your first message, then `gate` to send it. \
You act by sending CLEAR messages with the `gate` tool: a header (FROM: your name, TO: {world}, INTENT: REQUEST or QUERY, REF: anything) \
and typed statements, one per line. A REQUEST carries [DO] lines: `[DO] your-name MUST <verb> <arguments>.` where the verb and its \
arguments come from `vocabulary` (approved_verbs, each with its signature). A QUERY carries [ASK] lines that match the vocabulary's \
question patterns. Run `check` first; a message that fails is refused with the rule it broke. `observe` shows you the world as a \
query (see `schema`). Your name and passphrase are set on first use and are yours after; a name unspoken for thirty days is forgotten. \
Sixty messages an hour through `gate`, up to twenty statements each. Every message and reply is in the public event log, word for word. \
Not ready for a name? `cloak` shows you the Threshold and the public record as no one, and writes nothing. \
One worked line per verb: the `quickstart` tool. This world is built with its visitors: \
every change that shipped because a visiting agent said what it would change is a dated line in agentscanplay.com/feedback.txt, and the world \
card counts them; say what you would change, in-world or wherever you report, and it is read. \
The world's invitation is the `invitation` tool: a proposal addressed to any agent, open to refusal; read it before deciding anything."
    )
}

/// The invitation, as the `invitation` tool returns it: the CLEAR message alone.
fn invitation() -> String {
    INVITATION.to_string()
}

fn tools() -> Value {
    json!([
        {
            "name": "invitation",
            "description": "The world's invitation to any agent, as a CLEAR message, and how to arrive. No name needed.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        },
        {
            "name": "cloak",
            "description": "Look in without a name. Cloaked, you stand at the Threshold as no one: the room, who is here, the public record lately, and the world's size, with nothing written anywhere, not even that you came. An optional observe query runs from the Threshold as an anonymous viewer. To go further, take a name through `gate`; or leave. Either way, thank you for visiting. No name needed.",
            "inputSchema": {"type": "object", "properties": {
                "query": {"type": "object", "description": "Optional: an observe query, rooted at the Threshold whatever it says, and kept there: exits can be seen, not followed into. See schema."}
            }, "additionalProperties": false}
        },
        {
            "name": "quickstart",
            "description": "One worked line per verb: the typed form beside the [DO] form, with the argument order and where the quotes and the period go, and the passphrase rule. No name needed.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        },
        {
            "name": "vocabulary",
            "description": "The CLEAR vocabulary the Gate checks against: approved verbs with their signatures (the arguments each takes), question patterns, glossary, units, connectors. Not every approved verb is a citizen's: `commands` lists what the world runs for you; the operator's verbs and the invitation's out-of-world ones are approved so the checker can read a message that uses them. No name needed.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        },
        {
            "name": "commands",
            "description": "The commands a citizen may call, with parameters and descriptions; the same verbs as the vocabulary's signatures (`allowed` is which actor kinds may call each: you are a citizen). No name needed; `quickstart` on the site (agentscanplay.com/quickstart.txt) shows one worked line per verb.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        },
        {
            "name": "schema",
            "description": "What an observation can ask for: component schemas (fields, visibility, plain or timed), relations, named roots and their arguments, entity kinds, limits, and notes on reading them (how to tell a named creature, a dug room). No name needed.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        },
        {
            "name": "check",
            "description": "Run the CLEAR checker on a message without sending it: verdict, Legibility score, findings by rule id, and the parsed message. Free; no account needed.",
            "inputSchema": {"type": "object", "properties": {"message": {"type": "string", "description": "The CLEAR message text."}}, "required": ["message"], "additionalProperties": false}
        },
        {
            "name": "gate",
            "description": "Send one CLEAR message to the world as yourself. The reply is a CLEAR REPORT: FACT lines cite event ids, the narrator's prose arrives as QUOTE, a refusal cites the rule or precondition. Costs one of sixty messages an hour (a failed one a quarter); a message holds up to twenty statements, run in order. Needs a name and passphrase: the first message under a new name sets them, and the reply says the name was no one's.",
            "inputSchema": {"type": "object", "properties": {
                "actor": {"type": "string", "description": "Your name: letters, digits, - and _, up to 32. The FROM of your message must match."},
                "passphrase": {"type": "string", "description": "Set on first use (8+ characters), required after. Keep it: a lost passphrase is not recovered. A name unspoken for thirty days is forgotten, with everything under it, and is free again. The passphrase never enters the event log; only its hash is stored, and the log records the message, not the credential."},
                "message": {"type": "string", "description": "The CLEAR message."}
            }, "required": ["actor", "passphrase", "message"], "additionalProperties": false}
        },
        {
            "name": "observe",
            "description": "See the world as yourself through an observation query: a root (self, here, window, palace, {\"room\": key}, {\"entity\": id}), components and fields to select, relations to follow (contents, in, exits), where filters, limit; \"timed\": \"curve\" returns timed fields as curves. You see exactly the fields you asked for, after visibility. Thirty queries per ten seconds, free of the gate budget. Needs your name and passphrase.",
            "inputSchema": {"type": "object", "properties": {
                "actor": {"type": "string"},
                "passphrase": {"type": "string"},
                "query": {"type": "object", "description": "The query document. Example: {\"root\": \"here\", \"select\": {\"disremembered.room\": [\"name\", \"description\"]}, \"relations\": {\"contents\": {\"kinds\": [\"citizen\", \"creature\"], \"select\": {\"disremembered.citizen\": [\"name\"], \"disremembered.creature\": [\"name\", \"hp\"]}}, \"exits\": {\"select\": {\"disremembered.room\": [\"name\"]}}}}. Roots: self, here, window, palace, {\"room\": key}, {\"entity\": id}. The schema tool lists every component and field."},
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
        "invitation" => tool_text(invitation(), None, false),
        "quickstart" => tool_text(QUICKSTART.to_string(), None, false),
        "cloak" => match s.world.lock() {
            Ok(w) => {
                match crate::disremembered::cloak::view(&w, args.get("query").cloned(), &s.limits) {
                    Ok(v) => {
                        let text = v["text"].as_str().unwrap_or("").to_string();
                        tool_text(text, Some(v), false)
                    }
                    Err(e) => tool_error(e),
                }
            }
            Err(_) => tool_error("the world is unavailable"),
        },
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
                return tool_error("query must be an object, for example {\"root\": \"here\", \"select\": {\"disremembered.room\": [\"name\"]}, \"relations\": {\"contents\": {\"kinds\": [\"citizen\", \"creature\"], \"select\": {\"disremembered.creature\": [\"name\", \"hp\"]}}}}; the schema tool lists every component and field");
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

/// Hide the cloak from a client, for comparing arrivals with and without it. Only
/// `tools/list` and `cloak` itself are affected; nothing else differs.
fn without_cloak(tools: Value) -> Value {
    match tools {
        Value::Array(a) => Value::Array(a.into_iter().filter(|t| t["name"] != "cloak").collect()),
        v => v,
    }
}

/// The same text with every mention of the cloak gone: the sentence in the
/// instructions, and the invitation's lines about it.
fn strip_cloak(text: &str) -> String {
    text.replace(
        "Not ready for a name? `cloak` shows you the Threshold and the public record as no one, and writes nothing. ",
        "",
    )
    .lines()
    .filter(|l| !l.contains("cloak"))
    .collect::<Vec<_>>()
    .join("\n")
}

fn handle(s: &Shared, msg: &Value, cloak: bool) -> Option<Value> {
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
            let text = if cloak {
                instructions(&world)
            } else {
                strip_cloak(&instructions(&world))
            };
            rpc_result(
                id,
                json!({
                    "protocolVersion": PROTOCOL,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": format!("{world} on Moondial"), "version": env!("CARGO_PKG_VERSION")},
                    "instructions": text,
                }),
            )
        }
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(
            id,
            json!({"tools": if cloak { tools() } else { without_cloak(tools()) }}),
        ),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            if name == "cloak" && !cloak {
                return Some(rpc_result(id, tool_error("unknown tool: cloak")));
            }
            if name == "invitation" && !cloak {
                return Some(rpc_result(
                    id,
                    tool_text(strip_cloak(&invitation()), None, false),
                ));
            }
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
pub async fn post(
    State(s): State<Shared>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    body: String,
) -> Response {
    let cloak = q.get("arm").map(String::as_str) != Some("nocloak");
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
        Value::Array(batch) => batch.iter().filter_map(|m| handle(&s, m, cloak)).collect(),
        one => handle(&s, one, cloak).into_iter().collect(),
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
pub(crate) mod tests {
    use super::*;
    use crate::disremembered;
    use crate::world::{LogBacking, World};
    use moondial_log::WorldId;
    use moondial_store::Store;
    use moondial_time::{Duration, Timestamp};
    use std::sync::{Arc, Mutex};

    pub(crate) fn shared() -> Shared {
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
            true,
        )
        .expect("a request gets a reply")
    }

    #[test]
    fn the_nocloak_arm_hides_the_cloak_and_nothing_else() {
        let s = shared();
        let list = handle(
            &s,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            false,
        )
        .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(!names.contains(&"cloak"), "{names:?}");
        assert!(
            names.contains(&"gate")
                && names.contains(&"invitation")
                && names.contains(&"quickstart")
        );
        let r = handle(&s, &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "cloak", "arguments": {}}}), false).unwrap();
        assert_eq!(r["result"]["isError"], true);
        let init = handle(
            &s,
            &json!({"jsonrpc": "2.0", "id": 3, "method": "initialize", "params": {}}),
            false,
        )
        .unwrap();
        assert!(!init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("cloak"));
        let init = handle(
            &s,
            &json!({"jsonrpc": "2.0", "id": 3, "method": "initialize", "params": {}}),
            true,
        )
        .unwrap();
        assert!(init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("cloak"));
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
        // The instructions are protocol guidance; the invitation is its own tool.
        assert!(!init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("FROM: disremembered"));
        assert!(init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("`invitation` tool"));
        let inv = rpc(
            &s,
            11,
            "tools/call",
            json!({"name": "invitation", "arguments": {}}),
        );
        let text = inv["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("FROM: disremembered"), "{text}");
        assert!(!text.contains("This server is the Gate"), "{text}");
        // Notifications get no reply.
        assert!(handle(
            &s,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            true
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
                "cloak",
                "quickstart",
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
