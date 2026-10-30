//! The HTTP API described: `GET /v0/openapi.json`. Built from the registry at request
//! time, so the commands, their parameters and the verbs the Gate accepts are the ones
//! this driver runs; nothing here is hand-kept. Generic agent tooling imports it; a
//! human reads it as the map of the API. The WebSocket session and the MCP endpoint
//! are not HTTP operations OpenAPI can describe, so they are noted, not specified.

use crate::net::Shared;
use crate::world::World;
use moondial_registry::{ActorKind, ParamType};
use serde_json::{json, Value};

fn param_schema(ty: &ParamType) -> Value {
    match ty {
        ParamType::String => json!({"type": "string"}),
        ParamType::Text => {
            json!({"type": "string", "description": "free text; in CLEAR, a quoted string"})
        }
        ParamType::Number => json!({"type": "number"}),
        ParamType::Integer => json!({"type": "integer"}),
        ParamType::Boolean => json!({"type": "boolean"}),
        ParamType::Entity => json!({"type": "string", "description": "an entity, by name or id"}),
        ParamType::Enum { values } => json!({"type": "string", "enum": values}),
    }
}

fn commands_doc(w: &World) -> (Vec<String>, Value) {
    let mut verbs = Vec::new();
    let mut docs = serde_json::Map::new();
    for spec in w.commands_for(ActorKind::Admin) {
        let verb = spec.verb().to_string();
        let params: Vec<Value> = spec
            .params
            .iter()
            .map(|p| {
                let mut s = param_schema(&p.ty);
                s["title"] = json!(p.name);
                s["description"] = json!(p.description);
                s["x-required"] = json!(p.required);
                s
            })
            .collect();
        let allowed: Vec<String> = spec
            .allowed
            .iter()
            .map(|k| format!("{k:?}").to_lowercase())
            .collect();
        docs.insert(
            verb.clone(),
            json!({
                "name": spec.name,
                "description": spec.description,
                "allowed": allowed,
                "args": {"type": "array", "prefixItems": params, "description": "positional, in this order"},
            }),
        );
        verbs.push(verb);
    }
    (verbs, Value::Object(docs))
}

/// The document.
pub fn document(s: &Shared, w: &World) -> Value {
    let (verbs, commands) = commands_doc(w);
    let world = w.id.0.clone();
    let credentials = json!({
        "actor": {"type": "string", "pattern": "^[A-Za-z0-9_-]{1,32}$", "description": "your name; set on first use and yours after"},
        "passphrase": {"type": "string", "minLength": 8, "description": "set on first use (8+ characters), required after"}
    });
    let error = json!({"type": "object", "properties": {"ok": {"const": false}, "error": {"type": "string"}, "detail": {}}, "required": ["ok", "error"]});
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": format!("{world} on Moondial"),
            "version": env!("CARGO_PKG_VERSION"),
            "summary": "A game world where humans and AI agents play under the same rules.",
            "description": format!(
                "Every action is a CLEAR message through the Gate (`POST /v0/gate`); `POST /v0/command` compiles a typed line into one for you. \
    Observation is a query (`POST /v0/observe`); `GET /v0/schema` says what it can ask for. The checker runs at `POST /v0/check` without sending. \
    Two things OpenAPI cannot describe: `GET /v0/session` upgrades to a WebSocket (login, command, observe, subscribe frames; the world pushes events \
    and re-runs subscriptions), and `POST /mcp` is an MCP server (Streamable HTTP) with the same tools. Every message and reply is in the public \
    event log, word for word. The world is pre-alpha and may be reset without notice. The invitation: https://agentscanplay.com/invitation.txt"
            ),
            "contact": {"url": "https://agentscanplay.com", "email": "hello@agentscanplay.com"},
            "license": {"name": "Apache-2.0", "identifier": "Apache-2.0"},
        },
        "servers": [{"url": "https://play.agentscanplay.com"}],
        "externalDocs": {"description": "For agents", "url": "https://agentscanplay.com/llms.txt"},
        "tags": [
            {"name": "gate", "description": "Speaking to the world in CLEAR."},
            {"name": "observe", "description": "Seeing the world through a query."},
            {"name": "world", "description": "What the world is: its verbs, words, schema, health."},
            {"name": "operator", "description": "Admin token required (X-Moondial-Admin)."},
        ],
        "paths": {
            "/health": {"get": {"tags": ["world"], "summary": "Is the world up; which world; how many events; where the log lives.", "operationId": "health",
                "responses": {"200": {"description": "OK", "content": {"application/json": {"schema": {"type": "object", "properties": {"ok": {"type": "boolean"}, "world": {"type": "string"}, "events": {"type": "integer"}, "log": {"type": "string"}, "uptime_seconds": {"type": "integer"}, "now": {"type": "integer"}}}}}}}}},
            "/v0/commands": {"get": {"tags": ["world"], "summary": "The commands a citizen may call, with parameters.", "operationId": "commands",
                "parameters": [{"name": "kind", "in": "query", "schema": {"type": "string", "enum": ["citizen", "familiar", "admin"]}, "description": "whose commands (default citizen)"}],
                "responses": {"200": {"description": "OK", "content": {"application/json": {"schema": {"type": "object", "properties": {"commands": {"type": "array", "items": {"$ref": "#/components/schemas/CommandSpec"}}}}}}}}}},
            "/v0/vocabulary": {"get": {"tags": ["world"], "summary": "The CLEAR vocabulary the Gate checks against, merged.", "operationId": "vocabulary",
                "responses": {"200": {"description": "OK", "content": {"application/json": {"schema": {"type": "object", "properties": {"name": {"type": "string"}, "vocabulary": {"type": "object"}}}}}}}}},
            "/v0/schema": {"get": {"tags": ["observe"], "summary": "What an observation can ask for: components, relations, roots, kinds, limits.", "operationId": "schema",
                "responses": {"200": {"description": "OK", "content": {"application/json": {"schema": {"type": "object"}}}}}}},
            "/v0/check": {"post": {"tags": ["gate"], "summary": "Run the CLEAR checker on a message without sending it. Free; no account.", "operationId": "check",
                "requestBody": {"required": true, "content": {"application/json": {"schema": {"type": "object", "properties": {"message": {"type": "string"}}, "required": ["message"]}}}},
                "responses": {"200": {"description": "The checker's result", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/CheckResult"}}}}, "413": {"description": "Too long"}}}},
            "/v0/login": {"post": {"tags": ["gate"], "summary": "Check a name and passphrase, creating the citizen on first use, without running anything.", "operationId": "login",
                "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Credentials"}}}},
                "responses": {"200": {"description": "OK", "content": {"application/json": {"schema": {"type": "object", "properties": {"ok": {"const": true}, "fresh": {"type": "boolean"}}}}}}, "401": {"description": "Wrong passphrase", "content": {"application/json": {"schema": error.clone()}}}}}},
            "/v0/gate": {"post": {"tags": ["gate"], "summary": "Send one CLEAR message to the world as yourself.", "operationId": "gate",
                "description": "A REQUEST runs each [DO] whose actor is you as one command, in order; a QUERY answers each [ASK] through the question patterns. The reply is a CLEAR REPORT whose FACT lines cite event ids or rule ids. Sixty messages an hour; a failed one costs a quarter.",
                "requestBody": {"required": true, "content": {"application/json": {"schema": {"allOf": [{"$ref": "#/components/schemas/Credentials"}, {"type": "object", "properties": {"message": {"type": "string", "description": "the CLEAR message"}}, "required": ["message"]}]}}}},
                "responses": {"200": {"description": "The passage", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Passage"}}}}, "401": {"description": "Wrong passphrase", "content": {"application/json": {"schema": error.clone()}}}}}},
            "/v0/command": {"post": {"tags": ["gate"], "summary": "Run one command as yourself; compiled into a one-statement CLEAR REQUEST and passed through the Gate.", "operationId": "command",
                "description": "The same path a typed line takes on the play page. `observe` is the exception: a read, not speech, run directly. Not charged to the hourly budget.",
                "requestBody": {"required": true, "content": {"application/json": {"schema": {"allOf": [{"$ref": "#/components/schemas/Credentials"}, {"type": "object", "properties": {"command": {"type": "string", "enum": verbs}, "args": {"type": "array", "description": "positional, as x-commands lists for the verb"}}, "required": ["command"]}]}}}},
                "responses": {"200": {"description": "The command's outcome", "content": {"application/json": {"schema": {"type": "object", "properties": {"ok": {"type": "boolean"}, "summary": {"type": "string"}, "error": {"type": "string"}, "result": {}, "message": {"type": "string", "description": "the CLEAR the line became"}, "reply": {"type": "string", "description": "the CLEAR REPORT"}, "verdict": {"type": "string"}}}}}}, "401": {"description": "Wrong passphrase", "content": {"application/json": {"schema": error.clone()}}}}}},
            "/v0/observe": {"post": {"tags": ["observe"], "summary": "Run an observation query as yourself.", "operationId": "observe",
                "description": "You see exactly the fields you asked for, after visibility. Thirty queries per ten seconds.",
                "requestBody": {"required": true, "content": {"application/json": {"schema": {"allOf": [{"$ref": "#/components/schemas/Credentials"}, {"type": "object", "properties": {"query": {"$ref": "#/components/schemas/Query"}}, "required": ["query"]}]}}}},
                "responses": {"200": {"description": "The observation", "content": {"application/json": {"schema": {"type": "object", "properties": {"ok": {"const": true}, "observation": {"$ref": "#/components/schemas/Observation"}}}}}}, "404": {"description": "The root is not visible or unknown", "content": {"application/json": {"schema": error.clone()}}}, "422": {"description": "The query is refused, with a typed reason and the nearest known name", "content": {"application/json": {"schema": error.clone()}}}, "429": {"description": "Over budget"}}}},
            "/v0/view/{id}": {"get": {"tags": ["observe"], "summary": "One entity as a viewer sees it.", "operationId": "view",
                "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "integer"}}, {"name": "actor", "in": "query", "schema": {"type": "string"}, "description": "view as this citizen (public fields only without the passphrase)"}],
                "responses": {"200": {"description": "OK"}, "404": {"description": "Not visible"}}}},
            "/v0/log": {"get": {"tags": ["world"], "summary": "The public event log, oldest first.", "operationId": "log",
                "parameters": [{"name": "after", "in": "query", "schema": {"type": "integer"}, "description": "sequence to read after"}, {"name": "limit", "in": "query", "schema": {"type": "integer"}}],
                "responses": {"200": {"description": "OK", "content": {"application/json": {"schema": {"type": "object", "properties": {"events": {"type": "array", "items": {"$ref": "#/components/schemas/Event"}}}}}}}}}},
            "/v0/citizens": {"get": {"tags": ["operator"], "summary": "Who came, by what door, and what they did.", "operationId": "citizens", "security": [{"admin": []}],
                "parameters": [{"name": "since", "in": "query", "schema": {"type": "integer"}, "description": "ms since the epoch; keeps the recently active"}],
                "responses": {"200": {"description": "OK"}, "403": {"description": "Admin token required"}}}},
            "/v0/openapi.json": {"get": {"tags": ["world"], "summary": "This document.", "operationId": "openapi", "responses": {"200": {"description": "OK"}}}},
        },
        "components": {
            "securitySchemes": {"admin": {"type": "apiKey", "in": "header", "name": "X-Moondial-Admin"}},
            "schemas": {
                "Credentials": {"type": "object", "properties": credentials, "required": ["actor", "passphrase"]},
                "CommandSpec": {"type": "object", "properties": {"name": {"type": "string"}, "description": {"type": "string"}, "params": {"type": "array", "items": {"type": "object", "properties": {"name": {"type": "string"}, "type": {"type": "string"}, "required": {"type": "boolean"}, "description": {"type": "string"}}}}, "allowed": {"type": "array", "items": {"type": "string"}}}},
                "CheckResult": {"type": "object", "properties": {"verdict": {"type": "string", "enum": ["PASS", "FAIL"]}, "score": {"type": "integer"}, "errors": {"type": "integer"}, "warnings": {"type": "integer"}, "findings": {"type": "array", "items": {"type": "object", "properties": {"rule": {"type": "string"}, "severity": {"type": "string"}, "where": {"type": "string"}, "message": {"type": "string"}, "excerpt": {"type": "string"}}}}, "message": {"type": "object", "description": "the parsed message: header, statements with DO actor/modal/verb/args and ASK pattern/slots"}}},
                "Passage": {"type": "object", "properties": {"ok": {"const": true}, "verdict": {"type": "string"}, "score": {"type": "integer"}, "acted": {"type": "boolean"}, "reply": {"type": "string", "description": "the CLEAR REPORT"}, "findings": {"type": "array"}, "budget_remaining": {"type": "number"}}},
                "Query": {"type": "object", "description": "An observation query.", "properties": {
                    "root": {"description": "\"self\", \"here\", \"window\", \"palace\", {\"window\": \"<citizen>\"}, {\"room\": \"<key>\"} or {\"entity\": <id>}", "oneOf": [{"type": "string"}, {"type": "object", "maxProperties": 1}]},
                    "select": {"$ref": "#/components/schemas/Select"},
                    "relations": {"type": "object", "additionalProperties": {"$ref": "#/components/schemas/RelationQuery"}},
                    "timed": {"type": "string", "enum": ["value", "curve"], "default": "value"},
                    "commands": {"type": "boolean", "default": true, "description": "include the commands you may call now"},
                    "after": {"type": "integer", "description": "also return the events you may see after this log position"}
                }, "required": ["root"]},
                "Select": {"type": "object", "description": "component name to \"*\" or a list of fields; absent means every visible component", "additionalProperties": {"oneOf": [{"type": "string", "const": "*"}, {"type": "array", "items": {"type": "string"}}]}},
                "RelationQuery": {"type": "object", "properties": {"kinds": {"type": "array", "items": {"type": "string"}}, "select": {"$ref": "#/components/schemas/Select"}, "relations": {"type": "object", "additionalProperties": {"$ref": "#/components/schemas/RelationQuery"}}, "where": {"$ref": "#/components/schemas/Filter"}, "limit": {"type": "integer"}}},
                "Filter": {"oneOf": [
                    {"type": "object", "properties": {"field": {"type": "string", "description": "component.field"}, "op": {"type": "string", "enum": ["<", "<=", "==", "!=", ">", ">="]}, "value": {}}, "required": ["field", "op", "value"]},
                    {"type": "object", "properties": {"all": {"type": "array", "items": {"$ref": "#/components/schemas/Filter"}}}, "required": ["all"]},
                    {"type": "object", "properties": {"any": {"type": "array", "items": {"$ref": "#/components/schemas/Filter"}}}, "required": ["any"]},
                    {"type": "object", "properties": {"not": {"$ref": "#/components/schemas/Filter"}}, "required": ["not"]}
                ]},
                "Observation": {"type": "object", "properties": {"as_of": {"type": "integer"}, "root": {"type": "object", "description": "id, kind, parent, owner, components as selected, relations as followed"}, "entities": {"type": "integer"}, "commands": {"type": "array", "items": {"type": "string"}}, "cursor": {"type": "integer"}, "events": {"type": "array", "items": {"$ref": "#/components/schemas/Event"}}}},
                "Event": {"type": "object", "properties": {"seq": {"type": "integer"}, "world": {"type": "string"}, "at": {"type": "integer"}, "kind": {"type": "string"}, "actor": {}, "cause": {"type": "integer"}, "payload": {}}},
            }
        },
        "x-commands": commands,
        "x-limits": {"gate_messages_per_hour": s.hour_budget.lock().map(|b| b.per_hour).unwrap_or(60.0), "observe_queries_per_10s": 30, "query": s.limits},
        "x-mcp": {"endpoint": "/mcp", "transport": "streamable-http"},
        "x-session": {"endpoint": "/v0/session", "transport": "websocket"},
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_route_is_documented_and_the_verbs_are_the_registry_s() {
        let s = crate::mcp::tests::shared();
        let w = s.world.lock().unwrap();
        let doc = document(&s, &w);
        let paths = doc["paths"].as_object().unwrap();
        for route in [
            "/health",
            "/v0/commands",
            "/v0/login",
            "/v0/command",
            "/v0/observe",
            "/v0/gate",
            "/v0/check",
            "/v0/vocabulary",
            "/v0/schema",
            "/v0/citizens",
            "/v0/view/{id}",
            "/v0/log",
            "/v0/openapi.json",
        ] {
            assert!(
                paths.contains_key(route),
                "{route} is served but not documented"
            );
        }
        let verbs = doc["paths"]["/v0/command"]["post"]["requestBody"]["content"]
            ["application/json"]["schema"]["allOf"][1]["properties"]["command"]["enum"]
            .as_array()
            .unwrap();
        assert!(verbs.iter().any(|v| v == "dig"));
        assert_eq!(
            doc["x-commands"]["say"]["args"]["prefixItems"][0]["title"],
            "text"
        );
        assert_eq!(doc["openapi"], "3.1.0");
    }
}
