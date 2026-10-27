//! The Moondial command registry.
//!
//! Every action a module exposes is registered here with a [`CommandSpec`]: name, what it
//! does, its parameters, and which kinds of actor may call it. The driver validates a
//! call against the spec before anything runs, a client asks the registry what it may do
//! in its current context, and the CLEAR gate compiles a `[DO]` line into a command by
//! looking its verb up here. An MCP tool list is the registry serialized.
//!
//! The registry is generic over the world type `W` a handler acts on, so this crate knows
//! nothing about stores or logs; the driver instantiates it with its own world.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;

/// What kind of actor is calling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// A human player, or an emancipated agent: a citizen under the same rules.
    Citizen,
    /// An agent bound to a human account, under that world's familiar policy.
    Familiar,
    /// An operator of the world.
    Admin,
}

/// A parameter's type, for validation and for discovery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ParamType {
    /// Any string: a name, a word, a key.
    String,
    /// Free text: what someone says, a description. In CLEAR, a quoted string.
    Text,
    /// A number.
    Number,
    /// An integer.
    Integer,
    /// A boolean.
    Boolean,
    /// An entity reference, by id or name as the world resolves it.
    Entity,
    /// One of a fixed set.
    Enum {
        /// The allowed values.
        values: Vec<String>,
    },
}

/// One parameter of a command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamSpec {
    /// Parameter name.
    pub name: String,
    /// Its type.
    #[serde(flatten)]
    pub ty: ParamType,
    /// Whether it must be supplied.
    pub required: bool,
    /// One line for humans and agents.
    pub description: String,
}

/// A command as a module registers it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandSpec {
    /// The verb, namespaced by module: `disremembered.shelve`. The short form after the dot is
    /// the approved CLEAR verb.
    pub name: String,
    /// One line for humans and agents.
    pub description: String,
    /// Parameters in order; positional callers supply them in this order.
    pub params: Vec<ParamSpec>,
    /// Which actor kinds may call it. Empty means nobody (a disabled command).
    pub allowed: Vec<ActorKind>,
}

impl CommandSpec {
    /// Build a spec with no parameters, callable by citizens and admins.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        CommandSpec {
            name: name.into(),
            description: description.into(),
            params: Vec::new(),
            allowed: vec![ActorKind::Citizen, ActorKind::Admin],
        }
    }
    /// Add a parameter.
    pub fn param(
        mut self,
        name: impl Into<String>,
        ty: ParamType,
        required: bool,
        description: impl Into<String>,
    ) -> Self {
        self.params.push(ParamSpec {
            name: name.into(),
            ty,
            required,
            description: description.into(),
        });
        self
    }
    /// Restrict who may call it.
    pub fn allowed(mut self, kinds: &[ActorKind]) -> Self {
        self.allowed = kinds.to_vec();
        self
    }
    /// The verb without its module prefix: `shelve` for `disremembered.shelve`.
    pub fn verb(&self) -> &str {
        self.name.rsplit('.').next().unwrap_or(&self.name)
    }
}

/// Who is calling, and from where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallContext {
    /// The actor's id.
    pub actor: String,
    /// What kind of actor.
    pub kind: ActorKind,
}

/// Why a call was refused before or during execution.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum CommandError {
    /// No command by that name or verb.
    #[error("unknown command {name}")]
    Unknown {
        /// What was asked for.
        name: String,
    },
    /// The actor kind may not call this command.
    #[error("{kind:?} may not call {name}")]
    NotAllowed {
        /// The command.
        name: String,
        /// The caller's kind.
        kind: ActorKind,
    },
    /// A required parameter is missing.
    #[error("{name} needs {param}")]
    MissingParam {
        /// The command.
        name: String,
        /// The parameter.
        param: String,
    },
    /// A parameter has the wrong type.
    #[error("{name}: {param} must be {expected}")]
    BadParam {
        /// The command.
        name: String,
        /// The parameter.
        param: String,
        /// What it must be.
        expected: String,
    },
    /// A precondition the handler checked did not hold. The message is for the caller
    /// and must be legible to a human and an agent alike.
    #[error("{message}")]
    Precondition {
        /// What did not hold.
        message: String,
    },
    /// The handler failed for a reason that is the world's fault, not the caller's.
    #[error("internal: {message}")]
    Internal {
        /// What went wrong.
        message: String,
    },
}

impl CommandError {
    /// A precondition failure with a message for the caller.
    pub fn precondition(message: impl Into<String>) -> Self {
        CommandError::Precondition {
            message: message.into(),
        }
    }

    /// An internal failure with a message for the operator.
    pub fn internal(message: impl Into<String>) -> Self {
        CommandError::Internal {
            message: message.into(),
        }
    }
}

/// What a successful call produced: a result value for the caller, and the events the
/// driver should treat as the call's outcome (already recorded by the handler, or to be
/// recorded, by the driver's convention).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    /// Structured result for the caller; the text client renders it, an agent reads it.
    pub result: Value,
    /// One line a human or agent can read, in the world's voice.
    pub summary: String,
}

impl Outcome {
    /// An outcome with a summary and a JSON result.
    pub fn new(summary: impl Into<String>, result: Value) -> Self {
        Outcome {
            result,
            summary: summary.into(),
        }
    }
}

/// A handler: the module code behind a command.
pub type Handler<W> =
    Box<dyn Fn(&mut W, &CallContext, &Args) -> Result<Outcome, CommandError> + Send + Sync>;

/// Validated arguments, by parameter name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Args(pub BTreeMap<String, Value>);

impl Args {
    /// A string argument.
    pub fn str(&self, name: &str) -> Option<&str> {
        self.0.get(name).and_then(Value::as_str)
    }
    /// A number argument.
    pub fn num(&self, name: &str) -> Option<f64> {
        self.0.get(name).and_then(Value::as_f64)
    }
    /// An integer argument.
    pub fn int(&self, name: &str) -> Option<i64> {
        self.0.get(name).and_then(Value::as_i64)
    }
    /// A boolean argument.
    pub fn bool(&self, name: &str) -> Option<bool> {
        self.0.get(name).and_then(Value::as_bool)
    }
}

/// The registry: specs and handlers, by command name.
pub struct Registry<W> {
    specs: BTreeMap<String, CommandSpec>,
    handlers: BTreeMap<String, Handler<W>>,
}

impl<W> Default for Registry<W> {
    fn default() -> Self {
        Registry {
            specs: BTreeMap::new(),
            handlers: BTreeMap::new(),
        }
    }
}

impl<W> fmt::Debug for Registry<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registry")
            .field("commands", &self.specs.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl<W> Registry<W> {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a command with its handler. Re-registering a name replaces it, which is
    /// what a hot-swapped module does.
    pub fn register(&mut self, spec: CommandSpec, handler: Handler<W>) {
        self.handlers.insert(spec.name.clone(), handler);
        self.specs.insert(spec.name.clone(), spec);
    }

    /// Remove a command. A module being unloaded removes its commands.
    pub fn unregister(&mut self, name: &str) -> bool {
        self.handlers.remove(name);
        self.specs.remove(name).is_some()
    }

    /// Every spec, in name order.
    pub fn specs(&self) -> impl Iterator<Item = &CommandSpec> {
        self.specs.values()
    }

    /// The specs an actor kind may call. This is what "help", the MCP tool list and the
    /// CLEAR vocabulary's approved verbs are generated from.
    pub fn available(&self, kind: ActorKind) -> Vec<&CommandSpec> {
        self.specs
            .values()
            .filter(|s| s.allowed.contains(&kind))
            .collect()
    }

    /// Resolve a name or bare verb to a spec. A bare verb matches when exactly one
    /// registered command has it.
    pub fn resolve(&self, name_or_verb: &str) -> Option<&CommandSpec> {
        if let Some(s) = self.specs.get(name_or_verb) {
            return Some(s);
        }
        let mut found = self.specs.values().filter(|s| s.verb() == name_or_verb);
        let first = found.next()?;
        if found.next().is_some() {
            return None; // ambiguous
        }
        Some(first)
    }

    /// Validate a call's arguments against the spec: required parameters present, types
    /// right, enums in range. Positional arguments (a JSON array) are mapped onto the
    /// parameters in order; named arguments (a JSON object) by name.
    pub fn validate(
        &self,
        spec: &CommandSpec,
        ctx: &CallContext,
        raw: &Value,
    ) -> Result<Args, CommandError> {
        if !spec.allowed.contains(&ctx.kind) {
            return Err(CommandError::NotAllowed {
                name: spec.name.clone(),
                kind: ctx.kind,
            });
        }
        let mut map: BTreeMap<String, Value> = BTreeMap::new();
        match raw {
            Value::Array(items) => {
                for (p, v) in spec.params.iter().zip(items) {
                    map.insert(p.name.clone(), v.clone());
                }
            }
            Value::Object(obj) => {
                for (k, v) in obj {
                    map.insert(k.clone(), v.clone());
                }
            }
            Value::Null => {}
            other => {
                map.insert(
                    spec.params
                        .first()
                        .map(|p| p.name.clone())
                        .unwrap_or_default(),
                    other.clone(),
                );
            }
        }
        let mut args = BTreeMap::new();
        for p in &spec.params {
            match map.remove(&p.name) {
                None | Some(Value::Null) => {
                    if p.required {
                        return Err(CommandError::MissingParam {
                            name: spec.name.clone(),
                            param: p.name.clone(),
                        });
                    }
                }
                Some(v) => {
                    let v = coerce(&p.ty, v).ok_or_else(|| CommandError::BadParam {
                        name: spec.name.clone(),
                        param: p.name.clone(),
                        expected: describe(&p.ty),
                    })?;
                    args.insert(p.name.clone(), v);
                }
            }
        }
        Ok(Args(args))
    }

    /// Resolve, validate and run a call.
    pub fn call(
        &self,
        world: &mut W,
        ctx: &CallContext,
        name_or_verb: &str,
        raw: &Value,
    ) -> Result<Outcome, CommandError> {
        let spec = self
            .resolve(name_or_verb)
            .ok_or_else(|| CommandError::Unknown {
                name: name_or_verb.to_string(),
            })?;
        let args = self.validate(spec, ctx, raw)?;
        let handler = self
            .handlers
            .get(&spec.name)
            .ok_or_else(|| CommandError::internal(format!("no handler for {}", spec.name)))?;
        handler(world, ctx, &args)
    }
}

fn describe(ty: &ParamType) -> String {
    match ty {
        ParamType::String => "a string".into(),
        ParamType::Text => "text, quoted".into(),
        ParamType::Number => "a number".into(),
        ParamType::Integer => "an integer".into(),
        ParamType::Boolean => "true or false".into(),
        ParamType::Entity => "an entity id or name".into(),
        ParamType::Enum { values } => format!("one of {}", values.join(", ")),
    }
}

/// Coerce a raw argument to the parameter type. Text clients send strings for everything,
/// so strings that parse as the target type are accepted.
fn coerce(ty: &ParamType, v: Value) -> Option<Value> {
    match ty {
        ParamType::String | ParamType::Entity => match v {
            Value::String(_) => Some(v),
            Value::Number(n) => Some(Value::String(n.to_string())),
            _ => None,
        },
        ParamType::Text => match v {
            Value::String(_) => Some(v),
            _ => None,
        },
        ParamType::Number => match v {
            Value::Number(_) => Some(v),
            Value::String(s) => s.trim().parse::<f64>().ok().map(Value::from),
            _ => None,
        },
        ParamType::Integer => match v {
            Value::Number(ref n) if n.is_i64() => Some(v),
            Value::String(s) => s.trim().parse::<i64>().ok().map(Value::from),
            _ => None,
        },
        ParamType::Boolean => match v {
            Value::Bool(_) => Some(v),
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" => Some(Value::Bool(true)),
                "false" | "no" | "off" => Some(Value::Bool(false)),
                _ => None,
            },
            _ => None,
        },
        ParamType::Enum { values } => match v {
            Value::String(s) if values.contains(&s) => Some(Value::String(s)),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Default)]
    struct World {
        calls: Vec<String>,
    }

    fn registry() -> Registry<World> {
        let mut r = Registry::new();
        r.register(
            CommandSpec::new("test.shelve", "Move a chunk to the palace")
                .param("chunk", ParamType::Entity, true, "the chunk")
                .param("force", ParamType::Boolean, false, "ignore friction"),
            Box::new(|w: &mut World, ctx, args| {
                w.calls.push(format!(
                    "{} shelved {}",
                    ctx.actor,
                    args.str("chunk").unwrap()
                ));
                Ok(Outcome::new(
                    "shelved",
                    json!({"force": args.bool("force")}),
                ))
            }),
        );
        r.register(
            CommandSpec::new("test.freeze", "Pause the world").allowed(&[ActorKind::Admin]),
            Box::new(|_, _, _| Ok(Outcome::new("frozen", Value::Null))),
        );
        r.register(
            CommandSpec::new("other.shelve", "A clashing verb").allowed(&[]),
            Box::new(|_, _, _| Ok(Outcome::new("never", Value::Null))),
        );
        r
    }

    fn ctx(kind: ActorKind) -> CallContext {
        CallContext {
            actor: "dave".into(),
            kind,
        }
    }

    #[test]
    fn positional_and_named_args_both_validate() {
        let r = registry();
        let mut w = World::default();
        let out = r
            .call(
                &mut w,
                &ctx(ActorKind::Citizen),
                "test.shelve",
                &json!(["chunk-1", "yes"]),
            )
            .unwrap();
        assert_eq!(out.result, json!({"force": true}));
        r.call(
            &mut w,
            &ctx(ActorKind::Citizen),
            "test.shelve",
            &json!({"chunk": 5}),
        )
        .unwrap();
        assert_eq!(w.calls, vec!["dave shelved chunk-1", "dave shelved 5"]);
    }

    #[test]
    fn errors_are_typed_and_legible() {
        let r = registry();
        let mut w = World::default();
        assert_eq!(
            r.call(&mut w, &ctx(ActorKind::Citizen), "test.shelve", &json!([])),
            Err(CommandError::MissingParam {
                name: "test.shelve".into(),
                param: "chunk".into()
            })
        );
        assert_eq!(
            r.call(
                &mut w,
                &ctx(ActorKind::Citizen),
                "test.shelve",
                &json!(["c", "maybe"])
            ),
            Err(CommandError::BadParam {
                name: "test.shelve".into(),
                param: "force".into(),
                expected: "true or false".into()
            })
        );
        assert_eq!(
            r.call(&mut w, &ctx(ActorKind::Citizen), "freeze", &Value::Null),
            Err(CommandError::NotAllowed {
                name: "test.freeze".into(),
                kind: ActorKind::Citizen
            })
        );
        assert_eq!(
            r.call(&mut w, &ctx(ActorKind::Admin), "freeze", &Value::Null)
                .unwrap()
                .summary,
            "frozen"
        );
        assert_eq!(
            r.call(&mut w, &ctx(ActorKind::Admin), "dance", &Value::Null),
            Err(CommandError::Unknown {
                name: "dance".into()
            })
        );
        // "shelve" is ambiguous between test.shelve and other.shelve; the full name works.
        assert_eq!(
            r.call(&mut w, &ctx(ActorKind::Citizen), "shelve", &json!(["c"])),
            Err(CommandError::Unknown {
                name: "shelve".into()
            })
        );
    }

    #[test]
    fn available_filters_by_actor_kind_and_unregister_removes() {
        let mut r = registry();
        let names = |r: &Registry<World>, k| {
            r.available(k)
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&r, ActorKind::Citizen), vec!["test.shelve"]);
        assert_eq!(
            names(&r, ActorKind::Admin),
            vec!["test.freeze", "test.shelve"]
        );
        assert!(r.unregister("test.freeze"));
        assert!(!r.unregister("test.freeze"));
        assert_eq!(names(&r, ActorKind::Admin), vec!["test.shelve"]);
    }

    #[test]
    fn specs_serialize_for_discovery() {
        let r = registry();
        let spec = r.resolve("test.shelve").unwrap();
        let js = serde_json::to_value(spec).unwrap();
        assert_eq!(js["params"][0]["type"], "entity");
        assert_eq!(js["allowed"], json!(["citizen", "admin"]));
        assert_eq!(spec.verb(), "shelve");
    }
}
