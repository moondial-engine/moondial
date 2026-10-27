//! Vocabularies: the built-in default, `.yaml` or `.json` files, and `extends:` chains.
//! Merging follows `clear_check.py`: lists append without duplicates, maps merge
//! recursively, scalars override.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// The built-in minimal vocabulary.
pub fn default_vocab() -> Value {
    json!({
        "version": "0.3",
        "mode": "org",
        "statement_types": ["FACT", "INFER", "DO", "ASK", "QUOTE"],
        "intents": ["INFORM", "REQUEST", "PROPOSE", "REPORT", "QUERY"],
        "confidence_levels": ["HIGH", "MEDIUM", "LOW"],
        "modals": ["MUST", "MUST NOT", "SHOULD", "SHOULD NOT", "MAY"],
        "intent_rules": {},
        "limits": {
            "max_statement_words": 25,
            "max_instruction_words": 20,
            "max_quote_words": 100,
            "max_statements": 40,
            "max_message_bytes": 16384
        },
        "settings": {
            "allow_urls": true,
            "quote_requires_source": false,
            "ask_patterns_required": false,
            "prose_score_cap": 40
        },
        "hedges": [],
        "vague_intensifiers": [],
        "vague_verbs": [],
        "relative_time": [],
        "running_periods": [],
        "approved_synonyms": {},
        "units": [],
        "identifier_words": [],
        "approved_acronyms": {},
        "ambiguous_acronyms": {},
        "directive_markers": [],
        "soft_obligations": [],
        "irregular_participles": [],
        "adjectival_participles": [],
        "common_verbs": [],
        "approved_verbs": [],
        "ask_patterns": [],
        "glossary": {}
    })
}

/// Why a vocabulary could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum VocabError {
    /// The file could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The error.
        source: std::io::Error,
    },
    /// The file is not valid YAML or JSON, or not a mapping.
    #[error("{path}: {message}")]
    Parse {
        /// The file.
        path: PathBuf,
        /// What is wrong.
        message: String,
    },
}

/// Merge `over` into `base` the way the reference checker does.
pub fn merge(base: &Value, over: &Value) -> Value {
    let mut out = base.clone();
    let (Some(out_map), Some(over_map)) = (out.as_object_mut(), over.as_object()) else {
        return over.clone();
    };
    for (key, val) in over_map {
        if key == "extends" {
            continue;
        }
        match (out_map.get(key), val) {
            (Some(Value::Array(existing)), Value::Array(items)) => {
                let mut seen = existing.clone();
                for item in items {
                    if !seen.contains(item) {
                        seen.push(item.clone());
                    }
                }
                out_map.insert(key.clone(), Value::Array(seen));
            }
            (Some(b @ Value::Object(_)), Value::Object(_)) => {
                let merged = merge(b, val);
                out_map.insert(key.clone(), merged);
            }
            _ => {
                out_map.insert(key.clone(), val.clone());
            }
        }
    }
    out
}

fn read_file(path: &Path) -> Result<Value, VocabError> {
    let text = std::fs::read_to_string(path).map_err(|source| VocabError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let data: Value = if path.extension().is_some_and(|e| e == "json") {
        serde_json::from_str(&text).map_err(|e| VocabError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?
    } else {
        let y: serde_yaml::Value = serde_yaml::from_str(&text).map_err(|e| VocabError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        yaml_to_json(y)
    };
    match data {
        Value::Null => Ok(Value::Object(Map::new())),
        Value::Object(_) => Ok(data),
        _ => Err(VocabError::Parse {
            path: path.to_path_buf(),
            message: "vocabulary must be a mapping".into(),
        }),
    }
}

fn yaml_to_json(y: serde_yaml::Value) -> Value {
    match y {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(b) => Value::Bool(b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else if let Some(u) = n.as_u64() {
                Value::from(u)
            } else {
                n.as_f64().map(Value::from).unwrap_or(Value::Null)
            }
        }
        serde_yaml::Value::String(s) => Value::String(s),
        serde_yaml::Value::Sequence(items) => {
            Value::Array(items.into_iter().map(yaml_to_json).collect())
        }
        serde_yaml::Value::Mapping(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| {
                    let key = match k {
                        serde_yaml::Value::String(s) => s,
                        other => yaml_scalar_string(&other),
                    };
                    (key, yaml_to_json(v))
                })
                .collect(),
        ),
        serde_yaml::Value::Tagged(t) => yaml_to_json(t.value),
    }
}

fn yaml_scalar_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

/// Load a vocabulary file, following `extends:` chains relative to the file.
pub fn load_vocabulary(path: &Path) -> Result<Value, VocabError> {
    let data = read_file(path)?;
    let base = match data.get("extends").and_then(Value::as_str) {
        Some(parent) if !parent.is_empty() => {
            let parent_path = if Path::new(parent).is_absolute() {
                PathBuf::from(parent)
            } else {
                path.parent().unwrap_or(Path::new(".")).join(parent)
            };
            load_vocabulary(&parent_path)?
        }
        _ => default_vocab(),
    };
    Ok(merge(&base, &data))
}

/// Parse vocabulary text (YAML or JSON) without merging anything; `extends` is left
/// to the caller, who has the parent's text.
pub fn parse_raw(text: &str) -> Result<Value, String> {
    let y: serde_yaml::Value = serde_yaml::from_str(text).map_err(|e| e.to_string())?;
    match yaml_to_json(y) {
        Value::Null => Ok(Value::Object(Map::new())),
        v @ Value::Object(_) => Ok(v),
        _ => Err("vocabulary must be a mapping".into()),
    }
}

/// Load a vocabulary from text already in memory (YAML or JSON) over the built-in
/// default; `extends` is ignored.
pub fn parse_vocabulary(text: &str) -> Result<Value, String> {
    Ok(merge(&default_vocab(), &parse_raw(text)?))
}
