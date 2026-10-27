//! CLEAR v0.3 in Rust.
//!
//! CLEAR (Controlled Language for Exchange between Agents and Reviewers) is a controlled
//! output language for agent handoffs. This crate is a port of the reference checker,
//! `clear/clear_check.py`, and is held to it: a test replays the oracle corpus under
//! `clear/tests/oracle/` and requires the same verdict, score, findings and parsed
//! message, byte for byte. When the two disagree, the Python checker is right and this
//! crate is fixed.
//!
//! The parsed [`Message`] is what a world gate compiles: each `[DO]` split into actor,
//! modal, verb and arguments; each `[ASK]` matched to a question pattern with its slots;
//! every `[src:]` reference collected.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod vocab;

use fancy_regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

pub use vocab::{default_vocab, load_vocabulary, merge, parse_raw, parse_vocabulary, VocabError};

/// The CLEAR version this crate implements.
pub const VERSION: &str = "0.3.0";

/// A finding's severity.
pub const ERROR: &str = "error";
/// A finding's severity.
pub const WARNING: &str = "warning";

/// One rule hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// The rule id, `CL-S2`.
    pub rule: String,
    /// `error` or `warning`.
    pub severity: String,
    /// `header`, `message`, or `statement N` (1-based).
    pub r#where: String,
    /// What is wrong, for the author.
    pub message: String,
    /// The offending text, when there is one.
    #[serde(default)]
    pub excerpt: String,
}

/// One statement of a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Statement {
    /// 1-based position.
    pub index: usize,
    /// Source line, 1-based; 0 for prose sentences.
    pub line: usize,
    /// The line as written, trimmed.
    pub raw: String,
    /// FACT, INFER, DO, ASK, QUOTE, or `None` when untyped.
    pub r#type: Option<String>,
    /// INFER's confidence level, as written.
    pub confidence: Option<String>,
    /// The text without the tag and without `[src:]` tokens.
    pub body: String,
    /// Every `[src:]` reference.
    pub sources: Vec<String>,
    /// DO: who acts.
    pub actor: Option<String>,
    /// DO: MUST, MUST NOT, SHOULD, SHOULD NOT, MAY.
    pub modal: Option<String>,
    /// DO: the verb, lowercased.
    pub verb: Option<String>,
    /// DO: everything after the verb, trimmed, without a trailing period.
    pub arguments: Option<String>,
    /// DO: the arguments, parsed (v0.4): `{"kind": "ref"|"number"|"string", "value": ...}`
    /// and a number may carry a `unit`.
    pub args: Vec<Value>,
    /// ASK: the question pattern it matched.
    pub pattern: Option<String>,
    /// ASK: the pattern's named groups.
    pub slots: BTreeMap<String, String>,
}

/// A parsed message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Header fields in the order written.
    pub header: Vec<(String, String)>,
    /// The statements.
    pub statements: Vec<Statement>,
    /// `clear` or `prose`.
    pub mode: String,
}

impl Message {
    /// A header field.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.header
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// The checker's verdict on a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    /// PASS or FAIL.
    pub verdict: String,
    /// Legibility, 0 to 100.
    pub score: u32,
    /// `clear` or `prose`.
    pub mode: String,
    /// Error count.
    pub errors: usize,
    /// Warning count.
    pub warnings: usize,
    /// Every finding, errors first, in statement order.
    pub findings: Vec<Finding>,
    /// The parsed message.
    pub message: Message,
    /// The vocabulary's name.
    pub vocabulary: String,
}

impl CheckResult {
    /// The result as the reference checker prints it with `--json`.
    pub fn to_json(&self) -> Value {
        let header: serde_json::Map<String, Value> = self
            .message
            .header
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        let statements: Vec<Value> = self
            .message
            .statements
            .iter()
            .map(|s| {
                serde_json::json!({
                    "index": s.index, "line": s.line, "raw": s.raw, "type": s.r#type,
                    "confidence": s.confidence, "body": s.body, "sources": s.sources,
                    "actor": s.actor, "modal": s.modal, "verb": s.verb, "arguments": s.arguments,
                    "args": s.args, "pattern": s.pattern, "slots": s.slots,
                })
            })
            .collect();
        serde_json::json!({
            "verdict": self.verdict, "score": self.score, "mode": self.mode,
            "errors": self.errors, "warnings": self.warnings, "findings": self.findings,
            "message": {"header": header, "statements": statements, "mode": self.message.mode},
            "vocabulary": self.vocabulary,
        })
    }

    /// The report as the reference checker prints it.
    pub fn format_report(&self) -> String {
        let mut lines = vec![format!(
            "Verdict: {}   Legibility: {}/100   errors: {}   warnings: {}   mode: {}   vocabulary: {}",
            self.verdict, self.score, self.errors, self.warnings, self.mode, self.vocabulary
        )];
        if self.findings.is_empty() {
            lines.push("No findings.".into());
        }
        for f in &self.findings {
            let tag = if f.severity == ERROR { "E" } else { "W" };
            let exc = if f.excerpt.is_empty() {
                String::new()
            } else {
                format!("  [{}]", f.excerpt)
            };
            lines.push(format!(
                "  {tag} {:<6} {:<13} {}{exc}",
                f.rule, f.r#where, f.message
            ));
        }
        lines.join("\n")
    }
}

// ---------------------------------------------------------------------------------------
// Fixed patterns
// ---------------------------------------------------------------------------------------

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap_or_else(|e| panic!("bad built-in pattern {p}: {e}"))
}

struct Patterns {
    header: Regex,
    tag: Regex,
    bad_tag: Regex,
    src: Regex,
    src_ok: Regex,
    ident: Regex,
    do_: Regex,
    url: Regex,
    number: Regex,
    iso_date: Regex,
    time: Regex,
    acronym: Regex,
    header_smuggle: Regex,
    sentence_split: Regex,
    word: Regex,
    running_asof: Regex,
    multispace: Regex,
    hedge_number: Regex,
    pronoun_tokens: Regex,
    next_word: Regex,
    prev_word: Regex,
    imperative: Regex,
    second_action: Regex,
    semicolon_clause: Regex,
    quoted: Regex,
    arg_token: Regex,
    number_token: Regex,
    ident_token: Regex,
    conjunction: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        header: re(r"^(FROM|TO|INTENT|REF)\s*:\s*(.*?)\s*$"),
        tag: re(r"^\[(?P<tag>[A-Z]+)(?::(?P<conf>[A-Za-z]+))?\]\s*(?P<body>.*)$"),
        bad_tag: re(r"^\[[^\]]*\]"),
        src: re(r"\[src:\s*([^\]]*)\]"),
        src_ok: re(r"^[A-Za-z0-9][A-Za-z0-9:_./-]*$"),
        ident: re(r"^[A-Za-z0-9][A-Za-z0-9_.:@/-]*$"),
        do_: re(
            r"^(?P<actor>[A-Za-z0-9][A-Za-z0-9_.:@/-]*)\s+(?P<modal>MUST NOT|MUST|SHOULD NOT|SHOULD|MAY)\s+(?P<verb>[A-Za-z][A-Za-z'-]*)(?P<rest>.*)$",
        ),
        url: re(r"(?i)\b(?:https?://|www\.)\S+"),
        number: re(r"(?<![A-Za-z0-9_.:/-])(?P<num>\d[\d,]*(?:\.\d+)?)(?![\d,.]*[A-Za-z0-9_:/-])"),
        iso_date: re(r"\b\d{4}-\d{2}-\d{2}(?:T\d{2}:\d{2}(?::\d{2})?(?:Z|[+-]\d{2}:\d{2})?)?\b"),
        time: re(r"\b\d{1,2}:\d{2}(?::\d{2})?\b"),
        acronym: re(r"(?<![A-Za-z0-9-])([A-Z][A-Z0-9]{1,5})(?![A-Za-z0-9-])"),
        header_smuggle: re(r"(?m)(?:^|\s)(FROM|TO|INTENT|REF)\s*:"),
        sentence_split: re(r#"(?<=[.?!])\s+(?=[A-Z\["'(])"#),
        word: re(r"[A-Za-z0-9][A-Za-z0-9'’.,:/_-]*"),
        running_asof: re(r"(?i)\bas of \d{4}-\d{2}-\d{2}\b"),
        multispace: re(r"\s{2,}"),
        hedge_number: re(r"^\s+\d"),
        pronoun_tokens: re(r"[A-Za-z']+|[,;:.]"),
        next_word: re(r"^[A-Za-z%][A-Za-z%.]*"),
        prev_word: re(r"([A-Za-z#.]+)$"),
        imperative: re(r"^([A-Za-z]+)\s+([A-Za-z]+)"),
        second_action: re(r"(?i)\b(?:and then|then|and)\s+(?:(?:not|also|immediately)\s+)?([a-z][a-z'-]*)\b"),
        semicolon_clause: re(r";\s*[a-z]"),
        quoted: re("\"([^\"]*)\"|“([^”]*)”"),
        arg_token: re("\"[^\"]*\"|“[^”]*”|\\S+"),
        number_token: re(r"^[+-]?\d+(?:\.\d+)?$"),
        ident_token: re(r"^[A-Za-z0-9]*[0-9:_./-][A-Za-z0-9_.:/-]*$"),
        conjunction: re(r"(?i)(?<![A-Za-z-])(?:and then|and|then|or)(?![A-Za-z-])"),
    })
}

const PRONOUNS_ALWAYS: &[&str] = &["it", "they", "them"];
const PRONOUNS_DEMONSTRATIVE: &[&str] = &["this", "that", "these", "those"];
const AUXILIARIES: &[&str] = &[
    "is", "are", "was", "were", "be", "been", "being", "has", "have", "had", "will", "would",
    "can", "could", "should", "shall", "may", "might", "must", "does", "did", "do", "seems",
    "appears", "means", "shows", "makes", "needs", "requires", "includes", "contains", "causes",
    "happened", "failed", "works", "worked", "looks", "gets", "got", "went", "goes", "took",
    "takes", "says", "said", "holds", "held", "keeps", "kept", "comes",
];
const CLAUSE_STARTERS: &[&str] = &[
    "and", "but", "so", "because", "if", "when", "while", "then", "although", "since", "unless",
    "or",
];
const IMPERATIVE_OBJECTS: &[&str] = &[
    "the",
    "a",
    "an",
    "all",
    "every",
    "each",
    "your",
    "my",
    "our",
    "their",
    "its",
    "it",
    "this",
    "that",
    "these",
    "those",
    "them",
    "me",
    "us",
    "him",
    "her",
    "now",
    "immediately",
    "any",
    "everything",
    "nothing",
];
const NUMBER_STOPWORDS: &[&str] = &[
    "and", "or", "of", "to", "in", "at", "on", "for", "by", "with", "from", "the", "a", "an", "is",
    "are", "was", "were", "will", "that", "which", "per", "but", "so", "if", "then", "than", "as",
    "this", "these", "those", "it", "its", "into", "onto", "over", "under", "each", "more", "less",
    "out", "up", "down", "off", "not", "no", "yet", "when", "while",
];

fn control_name(ch: char) -> Option<&'static str> {
    Some(match ch {
        '\u{200B}' => "zero-width space",
        '\u{200C}' => "zero-width non-joiner",
        '\u{200D}' => "zero-width joiner",
        '\u{2060}' => "word joiner",
        '\u{FEFF}' => "byte-order mark",
        '\u{202A}' | '\u{202B}' => "bidi embedding",
        '\u{202C}' => "bidi pop",
        '\u{202D}' | '\u{202E}' => "bidi override",
        '\u{2066}' | '\u{2067}' | '\u{2068}' | '\u{2069}' => "bidi isolate",
        '\u{00AD}' => "soft hyphen",
        _ => return None,
    })
}

/// Unicode general category Cf (format), as of Unicode 15, for the characters the
/// reference checker's `unicodedata.category` would report.
fn is_format_char(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

// ---------------------------------------------------------------------------------------
// Python string semantics the port depends on
// ---------------------------------------------------------------------------------------

/// `str.splitlines()`.
fn splitlines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0;
    while i < bytes.len() {
        let (pos, c) = bytes[i];
        let is_break = matches!(
            c,
            '\n' | '\r'
                | '\u{0B}'
                | '\u{0C}'
                | '\u{1C}'
                | '\u{1D}'
                | '\u{1E}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if is_break {
            out.push(&text[start..pos]);
            let mut next = pos + c.len_utf8();
            if c == '\r' && i + 1 < bytes.len() && bytes[i + 1].1 == '\n' {
                i += 1;
                next = bytes[i].0 + 1;
            }
            start = next;
        }
        i += 1;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// `str.isupper()`.
fn py_isupper(s: &str) -> bool {
    let mut cased = false;
    for c in s.chars() {
        if c.is_lowercase() {
            return false;
        }
        if c.is_uppercase() {
            cased = true;
        }
    }
    cased
}

fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn last_chars(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}

fn rstrip(s: &str, chars: &[char]) -> String {
    s.trim_end_matches(|c| chars.contains(&c)).to_string()
}

fn search<'t>(r: &Regex, t: &'t str) -> Option<fancy_regex::Captures<'t>> {
    r.captures(t).ok().flatten()
}

fn is_match(r: &Regex, t: &str) -> bool {
    r.is_match(t).unwrap_or(false)
}

fn split_sentences(r: &Regex, text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut last = 0;
    for m in r.find_iter(text).flatten() {
        out.push(text[last..m.start()].to_string());
        last = m.end();
    }
    out.push(text[last..].to_string());
    out
}

/// The text with every quoted string replaced by a bare placeholder, so the rules that
/// judge the author's own words do not judge carried text.
fn strip_quotes(text: &str) -> String {
    patterns().quoted.replace_all(text, "\"...\"").into_owned()
}

/// Every match of `re` in `text`, with its groups.
fn captures_all<'t>(re: &Regex, text: &'t str) -> Vec<fancy_regex::Captures<'t>> {
    re.captures_iter(text).filter_map(Result::ok).collect()
}

fn value_str_of(v: &Value) -> String {
    value_str(v)
}

fn count_words(text: &str) -> usize {
    patterns().word.find_iter(text).count()
}

/// A whole-word, case-insensitive pattern for a word or phrase from the vocabulary.
fn phrase_re(phrase: &str) -> Regex {
    let esc = fancy_regex::escape(phrase.trim()).replace(' ', r"\s+");
    re(&format!(r"(?i)(?<![A-Za-z0-9-]){esc}(?![A-Za-z0-9-])"))
}

// ---------------------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------------------

/// Split a message into header and statements. Untyped lines keep `type: None`.
pub fn parse_message(text: &str) -> Message {
    let p = patterns();
    let lines = splitlines(text);
    let mut header: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() {
            if !header.is_empty() {
                i += 1;
            }
            break;
        }
        let Some(m) = search(&p.header, line) else {
            break;
        };
        let key = m.get(1).map(|g| g.as_str()).unwrap_or("").to_string();
        let val = m.get(2).map(|g| g.as_str()).unwrap_or("").to_string();
        if let Some(slot) = header.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = val;
        } else {
            header.push((key, val));
        }
        i += 1;
    }
    let mode = if header.is_empty() { "prose" } else { "clear" };
    let mut statements = Vec::new();
    if mode == "clear" {
        let mut idx = 0;
        for (lineno, line) in lines.iter().enumerate().skip(i) {
            let raw = line.trim();
            if raw.is_empty() {
                continue;
            }
            idx += 1;
            statements.push(parse_statement(idx, lineno + 1, raw));
        }
    } else {
        let body: Vec<&str> = lines
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        let body = body.join(" ");
        let mut idx = 0;
        for sent in split_sentences(&p.sentence_split, &body) {
            let sent = sent.trim();
            if sent.is_empty() {
                continue;
            }
            idx += 1;
            statements.push(parse_statement(idx, 0, sent));
        }
    }
    Message {
        header,
        statements,
        mode: mode.into(),
    }
}

fn parse_statement(idx: usize, lineno: usize, raw: &str) -> Statement {
    let p = patterns();
    let mut stype = None;
    let mut conf = None;
    let mut body = raw.to_string();
    if let Some(m) = search(&p.tag, raw) {
        let tag = m.name("tag").map(|g| g.as_str()).unwrap_or("");
        if matches!(tag, "FACT" | "INFER" | "DO" | "ASK" | "QUOTE") {
            stype = Some(tag.to_string());
            conf = m.name("conf").map(|g| g.as_str().to_string());
            body = m.name("body").map(|g| g.as_str()).unwrap_or("").to_string();
        }
    }
    let sources: Vec<String> = p
        .src
        .captures_iter(&body)
        .flatten()
        .map(|c| {
            c.get(1)
                .map(|g| g.as_str().trim().to_string())
                .unwrap_or_default()
        })
        .collect();
    let body = p.src.replace_all(&body, "").trim().to_string();
    let body = p.multispace.replace_all(&body, " ").to_string();
    let mut st = Statement {
        index: idx,
        line: lineno,
        raw: raw.to_string(),
        r#type: stype.clone(),
        confidence: conf,
        body: body.clone(),
        sources,
        actor: None,
        modal: None,
        verb: None,
        arguments: None,
        args: Vec::new(),
        pattern: None,
        slots: BTreeMap::new(),
    };
    if stype.as_deref() == Some("DO") {
        if let Some(dm) = search(&p.do_, &body) {
            st.actor = dm.name("actor").map(|g| g.as_str().to_string());
            st.modal = dm.name("modal").map(|g| g.as_str().to_string());
            st.verb = dm.name("verb").map(|g| g.as_str().to_lowercase());
            st.arguments = dm.name("rest").map(|g| rstrip(g.as_str().trim(), &['.']));
        }
    }
    st
}

// ---------------------------------------------------------------------------------------
// Checker
// ---------------------------------------------------------------------------------------

/// A compiled vocabulary: every rule's word lists as patterns.
pub struct Checker {
    vocab_name: String,
    max_words: usize,
    max_do_words: usize,
    max_quote_words: usize,
    max_statements: usize,
    max_bytes: usize,
    allow_urls: bool,
    quote_src_error: bool,
    ask_required: bool,
    prose_cap: u32,
    intents: BTreeSet<String>,
    confidences: BTreeSet<String>,
    intent_rules: Value,
    hedges: Vec<(Regex, String)>,
    intensifiers: Vec<(Regex, String)>,
    vague_verbs: Vec<(Regex, String)>,
    relative_time: Vec<(Regex, String)>,
    running_periods: Vec<(Regex, String)>,
    synonyms: Vec<(Regex, String, String)>,
    units: Vec<String>,
    identifier_words: Vec<String>,
    approved_acronyms: BTreeMap<String, String>,
    ambiguous_acronyms: BTreeMap<String, Vec<String>>,
    directives: Vec<(Regex, String)>,
    soft: Vec<(Regex, String)>,
    adjectival: BTreeSet<String>,
    common_verbs: BTreeSet<String>,
    approved_verbs: BTreeSet<String>,
    connectors: BTreeSet<String>,
    /// Verb signatures (v0.4): the parameters a verb takes, when the vocabulary gives them.
    signatures: BTreeMap<String, Vec<Value>>,
    ask_patterns: Vec<(Regex, String)>,
    aliases: Vec<(Regex, String, String)>,
    structural_upper: BTreeSet<String>,
    unit_upper: BTreeSet<String>,
    passive: Regex,
}

fn str_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().map(value_str).collect())
        .unwrap_or_default()
}

fn value_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => other.to_string(),
    }
}

fn int_of(v: &Value, key: &str, default: i64) -> i64 {
    v.get(key)
        .and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)))
        .unwrap_or(default)
}

fn bool_of(v: &Value, key: &str, default: bool) -> bool {
    match v.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Null) | None => default,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

impl Checker {
    /// Compile a vocabulary. `vocab_name` is what results report.
    pub fn new(vocab: &Value, vocab_name: &str) -> Self {
        let lim = vocab.get("limits").cloned().unwrap_or(Value::Null);
        let st = vocab.get("settings").cloned().unwrap_or(Value::Null);
        let words = |key: &str| -> Vec<(Regex, String)> {
            str_list(vocab, key)
                .into_iter()
                .map(|w| (phrase_re(&w), w))
                .collect()
        };
        let map_of = |key: &str| -> Vec<(String, Value)> {
            vocab
                .get(key)
                .and_then(Value::as_object)
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default()
        };
        let mut units = str_list(vocab, "units");
        units.sort_by_key(|u| std::cmp::Reverse(u.chars().count()));
        let irregular: Vec<String> = str_list(vocab, "irregular_participles")
            .into_iter()
            .map(|w| w.to_lowercase())
            .collect();
        let mut irregular_sorted: Vec<String> = irregular
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        irregular_sorted.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
        let irregular_alt = if irregular_sorted.is_empty() {
            "\u{0}".to_string()
        } else {
            irregular_sorted
                .iter()
                .map(|w| fancy_regex::escape(w).into_owned())
                .collect::<Vec<_>>()
                .join("|")
        };
        let passive = re(&format!(
            r"(?i)\b(?:is|are|was|were|be|been|being|get|gets|got|gotten)\s+(?:(?:not|also|still|already|never|being)\s+)?(?P<part>[A-Za-z]+(?:ed|en)|{irregular_alt})\b"
        ));
        let intents: BTreeSet<String> = str_list(vocab, "intents").into_iter().collect();
        let confidences: BTreeSet<String> =
            str_list(vocab, "confidence_levels").into_iter().collect();
        let mut structural_upper: BTreeSet<String> = [
            "FACT", "INFER", "DO", "ASK", "QUOTE", "MUST", "SHOULD", "MAY", "NOT", "FROM", "TO",
            "INTENT", "REF", "SRC",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        structural_upper.extend(intents.iter().cloned());
        structural_upper.extend(confidences.iter().cloned());
        let unit_upper: BTreeSet<String> = units
            .iter()
            .filter(|u| py_isupper(u))
            .map(|u| u.to_uppercase())
            .collect();
        let directives = str_list(vocab, "directive_markers")
            .into_iter()
            .map(|d| {
                let r = if !py_isupper(&d) {
                    phrase_re(&d)
                } else {
                    re(&format!(
                        r"(?<![A-Za-z]){}(?![A-Za-z])",
                        fancy_regex::escape(&d)
                    ))
                };
                (r, d)
            })
            .collect();
        let mut aliases = Vec::new();
        if let Some(glossary) = vocab.get("glossary").and_then(Value::as_object) {
            for (term, entry) in glossary {
                if let Some(list) = entry.get("aliases").and_then(Value::as_array) {
                    for alias in list {
                        let alias = value_str(alias);
                        aliases.push((phrase_re(&alias), alias, term.clone()));
                    }
                }
            }
        }
        Checker {
            vocab_name: vocab_name.to_string(),
            max_words: int_of(&lim, "max_statement_words", 25) as usize,
            max_do_words: int_of(&lim, "max_instruction_words", 20) as usize,
            max_quote_words: int_of(&lim, "max_quote_words", 100) as usize,
            max_statements: int_of(&lim, "max_statements", 40) as usize,
            max_bytes: int_of(&lim, "max_message_bytes", 16384) as usize,
            allow_urls: bool_of(&st, "allow_urls", true),
            quote_src_error: bool_of(&st, "quote_requires_source", false),
            ask_required: bool_of(&st, "ask_patterns_required", false),
            prose_cap: int_of(&st, "prose_score_cap", 40) as u32,
            intents,
            confidences,
            intent_rules: vocab.get("intent_rules").cloned().unwrap_or(Value::Null),
            hedges: words("hedges"),
            intensifiers: words("vague_intensifiers"),
            vague_verbs: words("vague_verbs"),
            relative_time: words("relative_time"),
            running_periods: str_list(vocab, "running_periods")
                .into_iter()
                .map(|p| (re(&format!(r"\b{}\b", fancy_regex::escape(&p))), p))
                .collect(),
            synonyms: map_of("approved_synonyms")
                .into_iter()
                .map(|(k, v)| (phrase_re(&k), k, value_str(&v)))
                .collect(),
            units,
            identifier_words: str_list(vocab, "identifier_words")
                .into_iter()
                .map(|w| w.to_lowercase())
                .collect(),
            approved_acronyms: map_of("approved_acronyms")
                .into_iter()
                .map(|(k, v)| (k.to_uppercase(), value_str(&v)))
                .collect(),
            ambiguous_acronyms: map_of("ambiguous_acronyms")
                .into_iter()
                .map(|(k, v)| {
                    let meanings = match v {
                        Value::Array(a) => a.iter().map(value_str).collect(),
                        other => vec![value_str(&other)],
                    };
                    (k.to_uppercase(), meanings)
                })
                .collect(),
            directives,
            soft: words("soft_obligations"),
            adjectival: str_list(vocab, "adjectival_participles")
                .into_iter()
                .map(|w| w.to_lowercase())
                .collect(),
            common_verbs: str_list(vocab, "common_verbs")
                .into_iter()
                .map(|w| w.to_lowercase())
                .collect(),
            approved_verbs: match vocab.get("approved_verbs") {
                Some(Value::Object(m)) => m.keys().map(|k| k.to_lowercase()).collect(),
                _ => str_list(vocab, "approved_verbs")
                    .into_iter()
                    .map(|w| w.to_lowercase())
                    .collect(),
            },
            connectors: str_list(vocab, "connectors")
                .into_iter()
                .map(|w| w.to_lowercase())
                .collect(),
            signatures: match vocab.get("approved_verbs") {
                Some(Value::Object(m)) => m
                    .iter()
                    .filter_map(|(verb, sig)| {
                        sig.get("params")
                            .and_then(Value::as_array)
                            .map(|params| (verb.to_lowercase(), params.clone()))
                    })
                    .collect(),
                _ => BTreeMap::new(),
            },
            ask_patterns: str_list(vocab, "ask_patterns")
                .into_iter()
                .filter_map(|p| Regex::new(&format!("(?i){p}")).ok().map(|r| (r, p)))
                .collect(),
            aliases,
            structural_upper,
            unit_upper,
            passive,
        }
    }

    /// The signature the vocabulary gives `verb` (v0.4): its parameters in order, each
    /// `{name, type, required}`; `None` when the verb has no signature.
    pub fn signature(&self, verb: &str) -> Option<&[Value]> {
        self.signatures.get(&verb.to_lowercase()).map(Vec::as_slice)
    }

    /// The first CLEAR reserved word or glossary alias that `name` contains, if any.
    /// A world uses this to keep its own names (rooms, creatures, exits, verbs) out of
    /// the protocol's way, since a FACT about a thing named `the gate` can never pass.
    pub fn collision(&self, name: &str) -> Option<String> {
        for tok in name.split(|c: char| !c.is_alphanumeric() && c != '-') {
            if self.structural_upper.contains(tok) {
                return Some(tok.to_string());
            }
        }
        for (pat, alias, term) in &self.aliases {
            if is_match(pat, name) {
                return Some(format!("{alias} (an alias of {term})"));
            }
        }
        None
    }

    /// Check a message.
    pub fn check(&self, text: &str) -> CheckResult {
        let mut findings = Vec::new();
        self.check_characters(text, &mut findings);
        let mut msg = parse_message(text);
        if msg.mode == "clear" {
            self.check_header(&msg, &mut findings);
            self.check_message_shape(&msg, text, &mut findings);
            let header_from = msg.field("FROM").map(str::to_string);
            for st in msg.statements.iter_mut() {
                self.check_statement(st, header_from.as_deref(), &mut findings);
            }
            self.check_intent_composition(&msg, &mut findings);
            self.check_duplicates(&msg, &mut findings);
        } else {
            add(
                &mut findings,
                "CL-H1",
                ERROR,
                "header",
                "No CLEAR header. The message is checked in prose mode and its score is capped.",
                "",
            );
            add(
                &mut findings,
                "CL-S1",
                ERROR,
                "message",
                "No statement has a type tag. Untyped text mixes facts, guesses and orders.",
                "",
            );
            for st in &msg.statements {
                self.check_content_rules(st, None, true, &mut findings);
            }
        }
        self.finish(msg, findings)
    }

    fn finish(&self, msg: Message, mut findings: Vec<Finding>) -> CheckResult {
        let errors = findings.iter().filter(|f| f.severity == ERROR).count();
        let warnings = findings.iter().filter(|f| f.severity == WARNING).count();
        let total = msg.statements.len();
        let mut score = if total > 0 {
            let bad: BTreeSet<&str> = findings
                .iter()
                .filter(|f| f.severity == ERROR && f.r#where.starts_with("statement "))
                .map(|f| f.r#where.as_str())
                .collect();
            (100.0 * (total - bad.len()) as f64 / total as f64).round_ties_even() as u32
        } else {
            0
        };
        if msg.mode == "prose" {
            score = score.min(self.prose_cap);
        }
        let verdict = if errors == 0 { "PASS" } else { "FAIL" };
        findings.sort_by(|a, b| {
            let sa = if a.severity == ERROR { 0 } else { 1 };
            let sb = if b.severity == ERROR { 0 } else { 1 };
            (sa, where_key(&a.r#where), &a.rule).cmp(&(sb, where_key(&b.r#where), &b.rule))
        });
        CheckResult {
            verdict: verdict.into(),
            score,
            mode: msg.mode.clone(),
            errors,
            warnings,
            findings,
            message: msg,
            vocabulary: self.vocab_name.clone(),
        }
    }

    fn check_characters(&self, text: &str, findings: &mut Vec<Finding>) {
        let mut found: Vec<(char, String)> = Vec::new();
        for ch in text.chars() {
            if found.iter().any(|(c, _)| *c == ch) {
                continue;
            }
            if let Some(name) = control_name(ch) {
                found.push((ch, name.to_string()));
            } else if (ch.is_control() || is_format_char(ch)) && !matches!(ch, '\n' | '\r' | '\t') {
                found.push((ch, format!("control character U+{:04X}", ch as u32)));
            }
        }
        for (ch, name) in found {
            add(
                findings,
                "CL-X3",
                ERROR,
                "message",
                &format!(
                    "Message contains a {name} (U+{:04X}). Hidden characters are not allowed.",
                    ch as u32
                ),
                "",
            );
        }
    }

    fn check_header(&self, msg: &Message, findings: &mut Vec<Finding>) {
        let p = patterns();
        for fld in ["FROM", "TO", "INTENT"] {
            if msg.field(fld).map_or(true, str::is_empty) {
                add(
                    findings,
                    "CL-H1",
                    ERROR,
                    "header",
                    &format!("Header is missing {fld}."),
                    "",
                );
            }
        }
        let intent = msg.field("INTENT").unwrap_or("");
        if !intent.is_empty() && !self.intents.contains(intent) {
            let list: Vec<&str> = self.intents.iter().map(String::as_str).collect();
            add(
                findings,
                "CL-H1",
                ERROR,
                "header",
                &format!(
                    "INTENT '{intent}' is not approved. Use one of: {}.",
                    list.join(", ")
                ),
                intent,
            );
        }
        for fld in ["FROM", "TO"] {
            if let Some(val) = msg.field(fld) {
                if !val.is_empty() && !is_match(&p.ident, val) {
                    add(findings, "CL-H3", ERROR, "header", &format!("{fld} must be an identifier (letters, digits, - _ . : @ /), not '{val}'."), val);
                }
            }
        }
        if let Some(from) = msg.field("FROM") {
            if !from.is_empty() && Some(from) == msg.field("TO") {
                add(
                    findings,
                    "CL-H3",
                    WARNING,
                    "header",
                    "FROM and TO are the same actor.",
                    "",
                );
            }
        }
        if let Some(r) = msg.field("REF") {
            if !r.is_empty() && !is_match(&p.ident, r) {
                add(
                    findings,
                    "CL-H3",
                    ERROR,
                    "header",
                    &format!("REF must be an identifier, not '{r}'."),
                    r,
                );
            }
        }
    }

    fn check_message_shape(&self, msg: &Message, text: &str, findings: &mut Vec<Finding>) {
        let n = msg.statements.len();
        if n == 0 {
            add(
                findings,
                "CL-S1",
                ERROR,
                "message",
                "The message has a header but no statements.",
                "",
            );
        }
        if n > self.max_statements {
            add(
                findings,
                "CL-M1",
                ERROR,
                "message",
                &format!(
                    "{n} statements exceed the limit of {}.",
                    self.max_statements
                ),
                "",
            );
        }
        let size = text.len();
        if size > self.max_bytes {
            add(
                findings,
                "CL-M1",
                ERROR,
                "message",
                &format!("{size} bytes exceed the limit of {} bytes.", self.max_bytes),
                "",
            );
        }
    }

    fn check_intent_composition(&self, msg: &Message, findings: &mut Vec<Finding>) {
        let Some(intent) = msg.field("INTENT") else {
            return;
        };
        let rule = self.intent_rules.get(intent);
        let Some(rule) =
            rule.filter(|r| !r.is_null() && r.as_object().is_some_and(|o| !o.is_empty()))
        else {
            return;
        };
        let types: BTreeSet<&str> = msg
            .statements
            .iter()
            .filter_map(|s| s.r#type.as_deref())
            .collect();
        for req in str_list(rule, "requires") {
            if !types.contains(req.as_str()) {
                add(
                    findings,
                    "CL-H2",
                    ERROR,
                    "header",
                    &format!("INTENT {intent} requires at least one [{req}] statement."),
                    "",
                );
            }
        }
        for forb in str_list(rule, "forbids") {
            if types.contains(forb.as_str()) {
                add(findings, "CL-H2", ERROR, "header", &format!("INTENT {intent} must not carry [{forb}] statements. Use REQUEST or PROPOSE for instructions."), "");
            }
        }
    }

    fn check_duplicates(&self, msg: &Message, findings: &mut Vec<Finding>) {
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        for st in &msg.statements {
            let key = format!(
                "{}|{}",
                st.r#type.as_deref().unwrap_or(""),
                st.body.to_lowercase()
            );
            if let Some(first) = seen.get(&key) {
                add(
                    findings,
                    "CL-S4",
                    WARNING,
                    &where_of(st),
                    &format!("Statement repeats statement {first}."),
                    &st.body,
                );
            } else {
                seen.insert(key, st.index);
            }
        }
    }

    fn check_statement(
        &self,
        st: &mut Statement,
        header_from: Option<&str>,
        findings: &mut Vec<Finding>,
    ) {
        let p = patterns();
        let w = where_of(st);
        let Some(stype) = st.r#type.clone() else {
            if is_match(&p.bad_tag, &st.raw) {
                add(
                    findings,
                    "CL-S1",
                    ERROR,
                    &w,
                    "Unknown statement tag. Use [FACT], [INFER:LEVEL], [DO], [ASK] or [QUOTE].",
                    &first_chars(&st.raw, 40),
                );
            } else {
                add(
                    findings,
                    "CL-S1",
                    ERROR,
                    &w,
                    "Statement has no type tag.",
                    &first_chars(&st.raw, 60),
                );
            }
            self.check_content_rules(st, None, false, findings);
            return;
        };
        if stype == "INFER" {
            if st.confidence.as_deref().map_or(true, |c| {
                c.is_empty() || !self.confidences.contains(&c.to_uppercase())
            }) {
                add(
                    findings,
                    "CL-C1",
                    ERROR,
                    &w,
                    "INFER needs a confidence level: [INFER:HIGH], [INFER:MEDIUM] or [INFER:LOW].",
                    &first_chars(&st.raw, 30),
                );
            }
        } else if st.confidence.as_deref().is_some_and(|c| !c.is_empty()) {
            add(
                findings,
                "CL-S1",
                ERROR,
                &w,
                &format!(
                    "[{stype}] does not take a qualifier; only INFER carries a confidence level."
                ),
                &first_chars(&st.raw, 30),
            );
        }
        if stype == "QUOTE" {
            self.check_quote(st, findings);
            return;
        }
        if stype == "DO" {
            if let Some(rest) = st.arguments.clone() {
                st.args = self.parse_arguments(&rest);
            }
        }
        self.check_structure_rules(st, findings);
        self.check_content_rules(st, Some(&stype), false, findings);
        if stype == "DO" {
            self.check_do(st, header_from, findings);
        }
        if stype == "ASK" {
            self.check_ask(st, findings);
        }
    }

    fn check_quote(&self, st: &Statement, findings: &mut Vec<Finding>) {
        let w = where_of(st);
        if st.sources.is_empty() {
            let sev = if self.quote_src_error { ERROR } else { WARNING };
            add(
                findings,
                "CL-Q1",
                sev,
                &w,
                "QUOTE must cite where the text came from with [src:...].",
                "",
            );
        }
        let n = count_words(&st.body);
        if n > self.max_quote_words {
            add(
                findings,
                "CL-Q2",
                WARNING,
                &w,
                &format!(
                    "QUOTE has {n} words; the limit is {}.",
                    self.max_quote_words
                ),
                "",
            );
        }
        self.check_sources(st, findings);
        if is_match(&patterns().header_smuggle, &st.body) {
            add(findings, "CL-X2", ERROR, &w, "QUOTE contains header-like text (FROM:, TO:, INTENT:, REF:). Header fields may not appear inside statements.", "");
        }
    }

    fn check_structure_rules(&self, st: &Statement, findings: &mut Vec<Finding>) {
        let p = patterns();
        let w = where_of(st);
        // In a DO, a quoted string is carried text: one argument, not the author's sentence.
        let body = if st.r#type.as_deref() == Some("DO") {
            strip_quotes(&st.body)
        } else {
            st.body.clone()
        };
        let parts = split_sentences(&p.sentence_split, &body)
            .into_iter()
            .filter(|s| !s.trim().is_empty())
            .count();
        if parts > 1 {
            add(
                findings,
                "CL-S2",
                ERROR,
                &w,
                &format!("Statement holds {parts} sentences; one sentence per statement."),
                &first_chars(&body, 60),
            );
        }
        let n = count_words(&body);
        let limit = if st.r#type.as_deref() == Some("DO") {
            self.max_do_words
        } else {
            self.max_words
        };
        if n > limit {
            add(
                findings,
                "CL-S3",
                ERROR,
                &w,
                &format!("Statement has {n} words; the limit is {limit}."),
                "",
            );
        }
        if st.r#type.as_deref() == Some("ASK") {
            // CL-A1 covers it.
        } else if !st.body.is_empty() && !st.body.ends_with('.') {
            add(
                findings,
                "CL-S5",
                WARNING,
                &w,
                "Statement does not end with a period.",
                &last_chars(&st.body, 20),
            );
        }
    }

    fn check_sources(&self, st: &Statement, findings: &mut Vec<Finding>) {
        let w = where_of(st);
        for src in &st.sources {
            if src.is_empty() || !is_match(&patterns().src_ok, src) {
                add(
                    findings,
                    "CL-C4",
                    ERROR,
                    &w,
                    &format!("Source reference '[src:{src}]' is not a resolvable identifier."),
                    src,
                );
            }
        }
    }

    fn check_content_rules(
        &self,
        st: &Statement,
        stype: Option<&str>,
        prose: bool,
        findings: &mut Vec<Finding>,
    ) {
        let p = patterns();
        let w = where_of(st);
        let masked;
        let body = if st.r#type.as_deref() == Some("DO") {
            masked = strip_quotes(&st.body);
            masked.as_str()
        } else {
            st.body.as_str()
        };
        self.check_sources(st, findings);
        if stype == Some("FACT") && st.sources.is_empty() {
            add(
                findings,
                "CL-C3",
                WARNING,
                &w,
                "FACT does not cite a source with [src:...].",
                "",
            );
        }
        if matches!(stype, Some("FACT") | Some("INFER")) || prose || stype.is_none() {
            for (pat, word) in &self.hedges {
                let m = pat.find(body).ok().flatten();
                if let Some(m) = m {
                    if matches!(
                        word.to_lowercase().as_str(),
                        "about" | "around" | "roughly" | "approximately"
                    ) && !is_match(&p.hedge_number, &body[m.end()..])
                    {
                        continue;
                    }
                    if stype == Some("INFER") {
                        add(findings, "CL-C2", WARNING, &w, &format!("Hedge '{word}' in an INFER; the confidence level already says how sure you are."), word);
                    } else {
                        add(findings, "CL-C2", ERROR, &w, &format!("Hedge '{word}' in a FACT. A hedged fact is an inference: use [INFER:LEVEL]."), word);
                    }
                }
            }
        }
        if stype != Some("DO") {
            for (pat, _) in &self.soft {
                for m in pat.find_iter(body).flatten() {
                    let tok = m.as_str();
                    if py_isupper(tok) {
                        continue;
                    }
                    add(findings, "CL-I4", ERROR, &w, &format!("Soft obligation '{tok}' outside a DO statement. An obligation must be a [DO] with an actor and a strength."), tok);
                    break;
                }
            }
        }
        if stype != Some("DO") {
            if let Some(m) = search(&self.passive, body) {
                let part = m.name("part").map(|g| g.as_str()).unwrap_or("");
                if !self.adjectival.contains(&part.to_lowercase()) {
                    let whole = m.get(0).map(|g| g.as_str()).unwrap_or("");
                    add(
                        findings,
                        "CL-I3",
                        WARNING,
                        &w,
                        &format!("Passive voice ('{whole}') hides the actor."),
                        whole,
                    );
                }
            }
        }
        if let Some(pron) = self.find_clause_pronoun(body) {
            add(
                findings,
                "CL-R1",
                ERROR,
                &w,
                &format!("Vague pronoun '{pron}' starts a clause. Name the thing it refers to."),
                &pron,
            );
        }
        for (pat, word) in &self.relative_time {
            if is_match(pat, body) {
                add(
                    findings,
                    "CL-R2",
                    ERROR,
                    &w,
                    &format!("Relative time '{word}'. Use an absolute date or a numbered turn."),
                    word,
                );
            }
        }
        for (pat, period) in &self.running_periods {
            if is_match(pat, body) && !is_match(&p.running_asof, body) {
                add(
                    findings,
                    "CL-R2",
                    ERROR,
                    &w,
                    &format!("Running period {period} needs an end date: 'as of YYYY-MM-DD'."),
                    period,
                );
            }
        }
        for num in self.find_unitless_numbers(body) {
            add(
                findings,
                "CL-R3",
                WARNING,
                &w,
                &format!("'{num}' has no unit."),
                &num,
            );
        }
        for (pat, word, approved) in &self.synonyms {
            if is_match(pat, body) {
                add(
                    findings,
                    "CL-V1",
                    WARNING,
                    &w,
                    &format!("Use '{approved}', not '{word}'."),
                    word,
                );
            }
        }
        for (pat, alias, term) in &self.aliases {
            if is_match(pat, body) {
                add(
                    findings,
                    "CL-V2",
                    ERROR,
                    &w,
                    &format!("Alias '{alias}'. The glossary term is '{term}'."),
                    alias,
                );
            }
        }
        for acr in self.find_acronyms(body) {
            if let Some(meanings) = self.ambiguous_acronyms.get(&acr) {
                add(
                    findings,
                    "CL-V6",
                    ERROR,
                    &w,
                    &format!(
                        "Acronym {acr} has two meanings ({}). Write the term out.",
                        meanings.join(", ")
                    ),
                    &acr,
                );
            } else if !self.approved_acronyms.contains_key(&acr) {
                add(
                    findings,
                    "CL-V3",
                    WARNING,
                    &w,
                    &format!("Acronym {acr} is not in the glossary."),
                    &acr,
                );
            }
        }
        for (pat, word) in &self.intensifiers {
            if is_match(pat, body) {
                add(
                    findings,
                    "CL-V4",
                    WARNING,
                    &w,
                    &format!("Vague intensifier '{word}' carries no measurable meaning."),
                    word,
                );
            }
        }
        for (pat, word) in &self.vague_verbs {
            if is_match(pat, body) {
                add(
                    findings,
                    "CL-V5",
                    WARNING,
                    &w,
                    &format!("Vague verb '{word}' says something changed without saying what."),
                    word,
                );
            }
        }
        if matches!(stype, Some("FACT") | Some("INFER") | Some("ASK")) || prose || stype.is_none() {
            if let Some(hit) = self.find_directive(body) {
                add(findings, "CL-X1", ERROR, &w, &format!("Directive '{hit}' inside a {} statement. Instructions belong in [DO]; carried text belongs in [QUOTE].", stype.unwrap_or("descriptive")), &hit);
            }
        }
        if is_match(&p.header_smuggle, body) {
            add(
                findings,
                "CL-X2",
                ERROR,
                &w,
                "Header-like text (FROM:, TO:, INTENT:, REF:) inside a statement.",
                "",
            );
        }
        if !self.allow_urls {
            if let Some(m) = p.url.find(body).ok().flatten() {
                add(
                    findings,
                    "CL-X4",
                    ERROR,
                    &w,
                    "URLs are not allowed in this domain outside a QUOTE.",
                    m.as_str(),
                );
            }
        }
    }

    fn check_do(&self, st: &Statement, header_from: Option<&str>, findings: &mut Vec<Finding>) {
        let p = patterns();
        let w = where_of(st);
        let Some(actor) = st.actor.as_deref() else {
            add(
                findings,
                "CL-I1",
                ERROR,
                &w,
                "DO must read '<actor> MUST|MUST NOT|SHOULD|SHOULD NOT|MAY <verb> ...'.",
                &first_chars(&st.body, 60),
            );
            return;
        };
        let rest_owned = strip_quotes(st.arguments.as_deref().unwrap_or(""));
        let rest = rest_owned.as_str();
        let mut second = false;
        if let Some(m) = search(&p.second_action, rest) {
            let verb = m
                .get(1)
                .map(|g| g.as_str().to_lowercase())
                .unwrap_or_default();
            if self.common_verbs.contains(&verb) || self.approved_verbs.contains(&verb) {
                let whole = m.get(0).map(|g| g.as_str()).unwrap_or("");
                add(
                    findings,
                    "CL-I2",
                    ERROR,
                    &w,
                    &format!("DO carries a second action ('{whole}'). One action per statement."),
                    whole,
                );
                second = true;
            }
        }
        if !second {
            if let Some(cm) = search(&p.conjunction, rest) {
                let whole = cm.get(0).map(|g| g.as_str()).unwrap_or("");
                add(
                    findings,
                    "CL-I2",
                    ERROR,
                    &w,
                    &format!("DO joins arguments with '{whole}'. One action, one object list; write a second DO."),
                    whole,
                );
            }
        }
        if is_match(&p.semicolon_clause, rest) {
            add(
                findings,
                "CL-I2",
                ERROR,
                &w,
                "DO carries a second clause after ';'. One action per statement.",
                "",
            );
        }
        let body_masked = strip_quotes(&st.body);
        if let Some(pm) = search(&self.passive, &body_masked) {
            let part = pm.name("part").map(|g| g.as_str()).unwrap_or("");
            if !self.adjectival.contains(&part.to_lowercase()) {
                let whole = pm.get(0).map(|g| g.as_str()).unwrap_or("");
                add(
                    findings,
                    "CL-I3",
                    ERROR,
                    &w,
                    &format!("DO in passive voice ('{whole}'). Name who acts."),
                    whole,
                );
            }
        }
        if !self.approved_verbs.is_empty() {
            let verb = st.verb.as_deref().unwrap_or("");
            if !self.approved_verbs.contains(verb) {
                let self_actor = match header_from {
                    Some(sender) if !sender.is_empty() => {
                        actor == sender || actor == "I" || actor == "i"
                    }
                    _ => false,
                };
                let sev = if self_actor { ERROR } else { WARNING };
                let tail = if self_actor {
                    " for the sender; the driver cannot compile it."
                } else {
                    "."
                };
                add(
                    findings,
                    "CL-I5",
                    sev,
                    &w,
                    &format!("Verb '{verb}' is not an approved action verb{tail}"),
                    verb,
                );
            }
        }
        // CL-I6 (v0.4): the arguments fit the verb's signature, when the world gives one.
        if let Some(sig) = self.signatures.get(st.verb.as_deref().unwrap_or("")) {
            self.check_signature(st, sig, findings);
        }
        // A quoted string in a DO is carried text under the quote limit.
        for q in captures_all(&p.quoted, st.arguments.as_deref().unwrap_or("")) {
            let text = q
                .get(1)
                .or_else(|| q.get(2))
                .map(|g| g.as_str())
                .unwrap_or("");
            let n = count_words(text);
            if n > self.max_quote_words {
                add(
                    findings,
                    "CL-Q2",
                    WARNING,
                    &w,
                    &format!(
                        "Quoted text has {n} words; the limit is {}.",
                        self.max_quote_words
                    ),
                    "",
                );
            }
            if is_match(&p.header_smuggle, text) {
                add(findings, "CL-X2", ERROR, &w, "Quoted text contains header-like text (FROM:, TO:, INTENT:, REF:). Header fields may not appear inside statements.", "");
            }
        }
    }

    fn check_signature(&self, st: &Statement, params: &[Value], findings: &mut Vec<Finding>) {
        let w = where_of(st);
        let verb = st.verb.as_deref().unwrap_or("");
        let name_of = |p: &Value| {
            p.get("name")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string()
        };
        let required: Vec<&Value> = params
            .iter()
            .filter(|p| p.get("required").and_then(Value::as_bool).unwrap_or(false))
            .collect();
        let names: Vec<String> = params.iter().map(name_of).collect();
        let n = st.args.len();
        let plural = |k: usize| if k != 1 { "s" } else { "" };
        let arguments = st.arguments.clone().unwrap_or_default();
        if n < required.len() {
            let need: Vec<String> = required.iter().map(|p| name_of(p)).collect();
            add(
                findings,
                "CL-I6",
                ERROR,
                &w,
                &format!(
                    "'{verb}' needs {} argument{} ({}); the DO gives {n}.",
                    required.len(),
                    plural(required.len()),
                    need.join(", ")
                ),
                &arguments,
            );
            return;
        }
        if n > params.len() {
            let list = if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            };
            add(
                findings,
                "CL-I6",
                ERROR,
                &w,
                &format!(
                    "'{verb}' takes at most {} argument{} ({list}); the DO gives {n}.",
                    params.len(),
                    plural(params.len())
                ),
                &arguments,
            );
            return;
        }
        for (arg, p) in st.args.iter().zip(params.iter()) {
            let t = p
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("string")
                .to_lowercase();
            let kind = arg.get("kind").and_then(Value::as_str).unwrap_or("");
            let value = arg.get("value").cloned().unwrap_or(Value::Null);
            let value_str = match &value {
                Value::String(s) => s.clone(),
                Value::Number(nn) => nn.to_string(),
                other => other.to_string(),
            };
            let enum_values: Vec<String> = p
                .get("values")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(value_str_of).collect())
                .unwrap_or_default();
            let ok = match t.as_str() {
                "text" => kind == "string",
                "number" => kind == "number",
                "integer" => kind == "number" && value.is_i64(),
                "boolean" => {
                    kind == "ref"
                        && matches!(
                            value_str.to_lowercase().as_str(),
                            "true" | "false" | "yes" | "no" | "on" | "off"
                        )
                }
                "enum" => (kind == "ref" || kind == "string") && enum_values.contains(&value_str),
                _ => kind == "ref" || kind == "string",
            };
            if !ok {
                let want = match t.as_str() {
                    "text" => "quoted text".to_string(),
                    "number" => "a number".to_string(),
                    "integer" => "an integer".to_string(),
                    "boolean" => "true or false".to_string(),
                    "enum" => format!("one of {}", enum_values.join(", ")),
                    "entity" => "a name or id".to_string(),
                    _ => "a word or quoted text".to_string(),
                };
                add(
                    findings,
                    "CL-I6",
                    ERROR,
                    &w,
                    &format!(
                        "'{verb}' takes {want} for {}; the DO gives '{value_str}'.",
                        name_of(p)
                    ),
                    &value_str,
                );
                return;
            }
        }
    }

    /// Split a DO's arguments (v0.4), as `clear_check.py` does.
    fn parse_arguments(&self, rest: &str) -> Vec<Value> {
        let p = patterns();
        let mut args: Vec<Value> = Vec::new();
        let mut phrase: Vec<String> = Vec::new();
        fn flush(phrase: &mut Vec<String>, args: &mut Vec<Value>) {
            if !phrase.is_empty() {
                args.push(serde_json::json!({"kind": "ref", "value": phrase.join(" ")}));
                phrase.clear();
            }
        }
        let tokens: Vec<String> = captures_all(&p.arg_token, rest)
            .into_iter()
            .filter_map(|m| m.get(0).map(|g| g.as_str().to_string()))
            .collect();
        let lower_units: Vec<String> = self.units.iter().map(|u| u.to_lowercase()).collect();
        let mut i = 0;
        while i < tokens.len() {
            let tok = &tokens[i];
            i += 1;
            if tok.starts_with('"') || tok.starts_with('“') {
                flush(&mut phrase, &mut args);
                let inner: String = tok
                    .chars()
                    .skip(1)
                    .take(tok.chars().count().saturating_sub(2))
                    .collect();
                args.push(serde_json::json!({"kind": "string", "value": inner}));
                continue;
            }
            let word = tok.trim_matches(|c| c == ',' || c == ';' || c == '.');
            if word.is_empty() {
                continue;
            }
            if is_match(&p.number_token, word) {
                flush(&mut phrase, &mut args);
                let value = if word.contains('.') {
                    word.parse::<f64>().map(Value::from).unwrap_or(Value::Null)
                } else {
                    word.parse::<i64>().map(Value::from).unwrap_or(Value::Null)
                };
                let mut arg = serde_json::json!({"kind": "number", "value": value});
                if i < tokens.len() {
                    let nxt = tokens[i].trim_matches(|c| c == ',' || c == ';' || c == '.');
                    if self.units.iter().any(|u| u == nxt)
                        || (lower_units.contains(&nxt.to_lowercase()) && !py_isupper(nxt))
                    {
                        arg["unit"] = Value::String(nxt.to_string());
                        i += 1;
                    }
                }
                args.push(arg);
                continue;
            }
            if self.connectors.contains(&word.to_lowercase()) {
                flush(&mut phrase, &mut args);
                continue;
            }
            if is_match(&p.ident_token, word) {
                flush(&mut phrase, &mut args);
                args.push(serde_json::json!({"kind": "ref", "value": word}));
                continue;
            }
            phrase.push(word.to_string());
        }
        flush(&mut phrase, &mut args);
        args
    }

    fn check_ask(&self, st: &mut Statement, findings: &mut Vec<Finding>) {
        let w = where_of(st);
        let body = st.body.clone();
        let q = body.matches('?').count();
        if q != 1 || !body.ends_with('?') {
            add(
                findings,
                "CL-A1",
                ERROR,
                &w,
                "ASK must be exactly one question, ending in '?'.",
                &last_chars(&body, 30),
            );
            return;
        }
        if !self.ask_patterns.is_empty() {
            for (pat, source) in &self.ask_patterns {
                if let Some(m) = search(pat, &body) {
                    if m.get(0).map(|g| g.start()) != Some(0) {
                        continue;
                    }
                    st.pattern = Some(source.clone());
                    st.slots = pat
                        .capture_names()
                        .flatten()
                        .filter_map(|name| {
                            m.name(name)
                                .map(|g| (name.to_string(), g.as_str().to_string()))
                        })
                        .collect();
                    return;
                }
            }
            let sev = if self.ask_required { ERROR } else { WARNING };
            add(findings, "CL-A2", sev, &w, "ASK matches none of the domain's question patterns, so the gate cannot compile it.", &first_chars(&body, 60));
        }
    }

    fn find_clause_pronoun(&self, body: &str) -> Option<String> {
        let tokens: Vec<&str> = patterns()
            .pronoun_tokens
            .find_iter(body)
            .flatten()
            .map(|m| m.as_str())
            .collect();
        let mut clause_start = true;
        for (i, tok) in tokens.iter().enumerate() {
            let low = tok.to_lowercase();
            if matches!(*tok, "," | ";" | ":" | ".") {
                clause_start = true;
                continue;
            }
            if clause_start {
                let nxt = tokens
                    .get(i + 1)
                    .map(|t| t.to_lowercase())
                    .unwrap_or_default();
                if PRONOUNS_ALWAYS.contains(&low.as_str()) {
                    return Some(tok.to_string());
                }
                if PRONOUNS_DEMONSTRATIVE.contains(&low.as_str())
                    && (AUXILIARIES.contains(&nxt.as_str())
                        || matches!(nxt.as_str(), "," | ";" | ":" | ".")
                        || nxt.is_empty())
                {
                    return Some(tok.to_string());
                }
            }
            clause_start = CLAUSE_STARTERS.contains(&low.as_str());
        }
        None
    }

    fn find_unitless_numbers(&self, body: &str) -> Vec<String> {
        let p = patterns();
        let mut out = Vec::new();
        let scrubbed = p.iso_date.replace_all(body, " ").to_string();
        let scrubbed = p.time.replace_all(&scrubbed, " ").to_string();
        for m in p.number.captures_iter(&scrubbed).flatten() {
            let whole = m.get(0).unwrap();
            let num = m.name("num").map(|g| g.as_str()).unwrap_or("").to_string();
            let after = scrubbed[whole.end()..].trim_start();
            let before = scrubbed[..whole.start()].trim_end();
            if after.starts_with('%') {
                continue;
            }
            if let Some(nw) = p.next_word.find(after).ok().flatten() {
                let nw = nw.as_str();
                if self.is_unit(nw) {
                    continue;
                }
                let lowered = rstrip(&nw.to_lowercase(), &['.', ',']);
                if !NUMBER_STOPWORDS.contains(&lowered.as_str())
                    && nw.chars().count() >= 3
                    && nw.chars().next().is_some_and(char::is_lowercase)
                {
                    continue;
                }
            }
            if let Some(prev) = search(&p.prev_word, before) {
                let pw = rstrip(
                    &prev
                        .get(1)
                        .map(|g| g.as_str().to_lowercase())
                        .unwrap_or_default(),
                    &['.'],
                );
                if self
                    .identifier_words
                    .iter()
                    .any(|w| rstrip(w, &['.']) == pw)
                {
                    continue;
                }
            }
            if before.ends_with('#') {
                continue;
            }
            out.push(num);
        }
        out
    }

    fn is_unit(&self, word: &str) -> bool {
        let w = rstrip(word, &['.', ',']);
        self.units
            .iter()
            .any(|u| w == *u || w.to_lowercase() == u.to_lowercase())
    }

    fn find_acronyms(&self, body: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for m in patterns().acronym.captures_iter(body).flatten() {
            let tok = m.get(1).map(|g| g.as_str()).unwrap_or("");
            if self.structural_upper.contains(tok) || self.unit_upper.contains(tok) {
                continue;
            }
            if !out.iter().any(|t| t == tok) {
                out.push(tok.to_string());
            }
        }
        out
    }

    fn find_directive(&self, body: &str) -> Option<String> {
        for (pat, _) in &self.directives {
            if let Some(m) = pat.find(body).ok().flatten() {
                return Some(m.as_str().to_string());
            }
        }
        if let Some(first) = search(&patterns().imperative, body) {
            let v = first.get(1).map(|g| g.as_str()).unwrap_or("");
            let o = first.get(2).map(|g| g.as_str()).unwrap_or("");
            let vl = v.to_lowercase();
            let ol = o.to_lowercase();
            if (self.common_verbs.contains(&vl) || self.approved_verbs.contains(&vl))
                && v.chars().next().is_some_and(char::is_uppercase)
                && IMPERATIVE_OBJECTS.contains(&ol.as_str())
            {
                return Some(v.to_string());
            }
        }
        None
    }
}

fn add(
    findings: &mut Vec<Finding>,
    rule: &str,
    severity: &str,
    r#where: &str,
    message: &str,
    excerpt: &str,
) {
    findings.push(Finding {
        rule: rule.into(),
        severity: severity.into(),
        r#where: r#where.into(),
        message: message.into(),
        excerpt: excerpt.into(),
    });
}

fn where_of(st: &Statement) -> String {
    format!("statement {}", st.index)
}

fn where_key(w: &str) -> i64 {
    match w {
        "header" => -2,
        "message" => -1,
        _ => w
            .rsplit(' ')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0),
    }
}

/// Check a message against a vocabulary. `vocab_name` defaults to the vocabulary's
/// `world`, then its `mode`, then `default`, as the reference does.
pub fn check_message(text: &str, vocab: Option<&Value>, vocab_name: Option<&str>) -> CheckResult {
    let default = default_vocab();
    let vocab = vocab.unwrap_or(&default);
    let name = vocab_name
        .map(str::to_string)
        .or_else(|| vocab.get("world").map(value_str))
        .or_else(|| vocab.get("mode").map(value_str))
        .unwrap_or_else(|| "default".into());
    Checker::new(vocab, &name).check(text)
}
