//! The Gate: where an emancipated agent speaks CLEAR to the world.
//!
//! A message is checked by the same checker the agent can run itself. A passing
//! `REQUEST` compiles each `[DO]` whose actor is the sender into one registry command,
//! run in order, each accepted or refused on its own. A passing `QUERY` compiles each
//! `[ASK]` through the vocabulary's question patterns into an observation. The reply is
//! a `REPORT` in the world's voice: `[FACT]` lines cite event ids, the narrator's prose
//! arrives as `[QUOTE]`, a refusal cites the rule or the precondition that failed, and
//! the world never writes `[INFER]` or `[DO]`. Every reply passes the checker too; a test
//! holds the gate to that.
//!
//! One passing message costs one unit of an hourly budget; a failed one costs a
//! fraction. Legibility is public standing, not a cost.

use crate::world::World;
use moondial_clear::{merge, parse_raw, CheckResult, Checker, Message, Statement};
use moondial_query::{Limits, QueryError};
use moondial_registry::{ActorKind, CallContext, CommandError};
use moondial_store::Viewer;
use moondial_time::Timestamp;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

const BASE_VOCAB: &str = include_str!("../../../clear/vocab/base.yaml");
const ENTROPIC_VOCAB: &str = include_str!("../../../clear/vocab/entropic.yaml");

/// Entropic's vocabulary, as the site publishes it: the base vocabulary with the
/// world's own layered on.
pub fn entropic_vocabulary() -> Value {
    let base = parse_raw(BASE_VOCAB).expect("base vocabulary parses");
    let world = parse_raw(ENTROPIC_VOCAB).expect("entropic vocabulary parses");
    merge(&merge(&moondial_clear::default_vocab(), &base), &world)
}

/// The world's checker, compiled once.
pub fn checker() -> Checker {
    Checker::new(&entropic_vocabulary(), "entropic")
}

/// The hourly message budget. A passing message costs one; a failed one costs a
/// fraction, so a broken agent cannot spend the hour on refusals but does pay for them.
pub struct HourBudget {
    pub per_hour: f64,
    pub fail_cost: f64,
    used: HashMap<String, (Timestamp, f64)>,
}

const HOUR_MS: i64 = 3_600_000;

impl HourBudget {
    pub fn new(per_hour: f64, fail_cost: f64) -> Self {
        HourBudget {
            per_hour,
            fail_cost,
            used: HashMap::new(),
        }
    }

    /// Whether `actor` may send now, and if not, when the hour turns.
    pub fn check(&mut self, actor: &str, now: Timestamp) -> Result<(), Timestamp> {
        let entry = self.used.entry(actor.to_string()).or_insert((now, 0.0));
        if now.since(entry.0).0 >= HOUR_MS {
            *entry = (now, 0.0);
        }
        if entry.1 + 1.0 > self.per_hour + 1e-9 {
            return Err(Timestamp(entry.0 .0 + HOUR_MS));
        }
        Ok(())
    }

    /// Charge for a message after it was checked.
    pub fn charge(&mut self, actor: &str, now: Timestamp, passed: bool) {
        let entry = self.used.entry(actor.to_string()).or_insert((now, 0.0));
        entry.1 += if passed { 1.0 } else { self.fail_cost };
    }

    /// What `actor` has left this hour.
    pub fn remaining(&self, actor: &str, now: Timestamp) -> f64 {
        match self.used.get(actor) {
            Some((start, spent)) if now.since(*start).0 < HOUR_MS => {
                (self.per_hour - spent).max(0.0)
            }
            _ => self.per_hour,
        }
    }
}

/// One line of a reply.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "UPPERCASE")]
pub enum Line {
    /// A fact, cited.
    Fact {
        /// One sentence, in CLEAR.
        text: String,
        /// Its source: an event id, a rule id, or a system name.
        src: String,
    },
    /// Carried text: the narrator's prose, or something another actor said.
    Quote {
        /// The text, word for word.
        text: String,
        /// Who said it.
        src: String,
    },
}

/// A reply from the world, before rendering.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub from: String,
    pub to: String,
    pub r#ref: Option<String>,
    pub lines: Vec<Line>,
}

/// The most lines a reply carries; the vocabulary's statement limit less room for a
/// continuation note.
const MAX_REPLY_LINES: usize = 38;

impl Report {
    fn new(world: &str, to: &str, r#ref: Option<&str>) -> Self {
        Report {
            from: world.into(),
            to: to.into(),
            r#ref: r#ref.map(str::to_string),
            lines: Vec::new(),
        }
    }

    pub fn fact(&mut self, text: impl Into<String>, src: impl Into<String>) {
        self.lines.push(Line::Fact {
            text: text.into(),
            src: src.into(),
        });
    }

    pub fn quote(&mut self, text: impl Into<String>, src: impl Into<String>) {
        self.lines.push(Line::Quote {
            text: text.into(),
            src: src.into(),
        });
    }

    /// The reply as CLEAR text.
    pub fn render(&self) -> String {
        let mut out = format!("FROM: {}\nTO: {}\nINTENT: REPORT\n", self.from, self.to);
        if let Some(r) = &self.r#ref {
            out.push_str(&format!("REF: {r}\n"));
        }
        out.push('\n');
        let shown = self.lines.len().min(MAX_REPLY_LINES);
        for line in &self.lines[..shown] {
            match line {
                Line::Fact { text, src } => {
                    out.push_str(&format!("[FACT] {} [src:{src}]\n", sentence(text)))
                }
                Line::Quote { text, src } => {
                    out.push_str(&format!("[QUOTE] {} [src:{src}]\n", one_line(text)))
                }
            }
        }
        if self.lines.len() > shown {
            out.push_str(&format!(
                "[FACT] The world omitted {} statements from this reply under the statement limit. [src:gate]\n",
                self.lines.len() - shown
            ));
        }
        out
    }
}

/// Make sure a FACT ends with a period and holds no line breaks.
fn sentence(text: &str) -> String {
    let t = one_line(text);
    if t.ends_with('.') {
        t
    } else {
        format!("{t}.")
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Milliseconds since the epoch as `YYYY-MM-DDTHH:MMZ`.
pub fn iso_minute(t: Timestamp) -> String {
    let secs = t.0.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// What a world binds into the gate: how a question pattern becomes an answer.
pub trait AskBinding {
    /// Answer a matched `[ASK]` for `actor`, appending FACT lines to `report`. Return
    /// `false` if this world has no answer for the pattern.
    fn answer(&self, world: &World, actor: &str, st: &Statement, report: &mut Report) -> bool;
}

/// Words a DO's arguments may carry that are not arguments: `attack the wisp`,
/// `travel to the east`, `shelve chunk-5 into the palace`.
const FILLER: &[&str] = &[
    "the", "a", "an", "to", "into", "onto", "at", "in", "with", "from", "of", "my", "your",
    "toward", "towards", "on", "for",
];

/// Compile a DO's arguments to positional command arguments.
pub fn compile_arguments(arguments: &str) -> Vec<Value> {
    arguments
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| matches!(c, ',' | ';' | '.')))
        .filter(|w| !w.is_empty() && !FILLER.contains(&w.to_ascii_lowercase().as_str()))
        .map(|w| Value::String(w.to_string()))
        .collect()
}

/// The outcome of one message through the gate.
#[derive(Debug, Clone, Serialize)]
pub struct Passage {
    /// The checker's verdict on the message.
    pub verdict: String,
    /// Its Legibility score.
    pub score: u32,
    /// Whether the world acted on it (a passing REQUEST or QUERY).
    pub acted: bool,
    /// The reply, rendered as CLEAR.
    pub reply: String,
    /// The checker's findings on the message.
    pub findings: Vec<moondial_clear::Finding>,
    /// Budget left this hour, in messages.
    pub budget_remaining: f64,
}

/// Run one message through the gate for an authenticated actor.
pub fn pass(
    world: &mut World,
    checker: &Checker,
    budget: &mut HourBudget,
    asks: &dyn AskBinding,
    actor: &str,
    text: &str,
) -> Passage {
    let world_name = world.id.0.clone();
    let now = world.now();
    let result = checker.check(text);
    let msg = &result.message;
    let r#ref = msg.field("REF").filter(|r| !r.is_empty());
    let mut report = Report::new(&world_name, actor, r#ref);

    if let Err(resets) = budget.check(actor, now) {
        report.fact(
            format!("{actor} has spent the message budget for the hour as of {}; the budget resets at {}", iso_minute(now), iso_minute(resets)),
            "budget",
        );
        return finish(world, result, report, false, budget.remaining(actor, now));
    }

    let passed = result.verdict == "PASS";
    budget.charge(actor, now, passed);
    // Every message that crosses the Gate is in the log, word for word, as the
    // invitation promises. Humans read it like any other event.
    let _ = world.note(
        moondial_log::Actor::Citizen(actor.to_string()),
        "gate.message",
        serde_json::json!({"from": actor, "text": text, "verdict": result.verdict, "score": result.score}),
    );

    // The sender must be who they say they are, and must be talking to this world.
    let from = msg.field("FROM").unwrap_or("");
    let to = msg.field("TO").unwrap_or("");
    if passed && from != actor {
        report.fact(
            format!("The message names {from} as FROM, and the world authenticated {actor}"),
            "gate",
        );
        report.fact(
            "The world acts only on messages whose FROM is the authenticated sender",
            "gate",
        );
        return finish(world, result, report, false, budget.remaining(actor, now));
    }
    if passed && to != world_name {
        report.fact(
            format!("The message is addressed to {to}, and this world is {world_name}"),
            "gate",
        );
        report.fact(
            "The world does not relay messages between actors in this version",
            "gate",
        );
        return finish(world, result, report, false, budget.remaining(actor, now));
    }

    if !passed {
        rejection(&result, &mut report);
        return finish(world, result, report, false, budget.remaining(actor, now));
    }

    let intent = msg.field("INTENT").unwrap_or("");
    match intent {
        "REQUEST" => run_request(world, msg, actor, &mut report),
        "QUERY" => run_query(world, msg, actor, asks, &mut report),
        "PROPOSE" => {
            report.fact("The world does not take proposals in this version", "gate");
            report.fact(
                "A PROPOSE will create an offer entity when the economy exists",
                "gate",
            );
        }
        _ => {
            report.fact(
                format!("The world received the {intent} message and recorded nothing from it"),
                "gate",
            );
            report.fact(
                "The world never takes a visitor's FACT as a fact about the world",
                "gate",
            );
        }
    }
    let acted = matches!(intent, "REQUEST" | "QUERY");
    finish(world, result, report, acted, budget.remaining(actor, now))
}

fn finish(
    world: &mut World,
    result: CheckResult,
    report: Report,
    acted: bool,
    budget_remaining: f64,
) -> Passage {
    let reply = report.render();
    let _ = world.note(
        moondial_log::Actor::Driver,
        "gate.reply",
        serde_json::json!({"to": report.to, "text": reply, "acted": acted}),
    );
    Passage {
        verdict: result.verdict,
        score: result.score,
        acted,
        reply,
        findings: result.findings,
        budget_remaining,
    }
}

/// A rejection cites every error by rule.
fn rejection(result: &CheckResult, report: &mut Report) {
    let r#ref = result
        .message
        .field("REF")
        .filter(|r| !r.is_empty())
        .unwrap_or("without REF")
        .to_string();
    for f in result
        .findings
        .iter()
        .filter(|f| f.severity == moondial_clear::ERROR)
    {
        let place = match f.r#where.as_str() {
            "header" => "The header".to_string(),
            "message" => "The message".to_string(),
            w => {
                let n = w.rsplit(' ').next().unwrap_or("0");
                format!("Statement {n}")
            }
        };
        report.fact(
            format!("{place} of message {ref} failed rule {}", f.rule),
            "clear_check",
        );
    }
    report.fact(
        format!(
            "The message scored {} Legibility and the world ran none of it",
            result.score
        ),
        "clear_check",
    );
}

fn run_request(world: &mut World, msg: &Message, actor: &str, report: &mut Report) {
    let dos: Vec<&Statement> = msg
        .statements
        .iter()
        .filter(|s| s.r#type.as_deref() == Some("DO"))
        .collect();
    if dos.is_empty() {
        report.fact(
            "The REQUEST carried no DO statement for the world to run",
            "gate",
        );
        return;
    }
    for st in dos {
        let n = st.index;
        let is_sender =
            matches!(st.actor.as_deref(), Some(a) if a == actor || a == "I" || a == "i");
        if !is_sender {
            report.fact(
                format!("Statement {n} names {} as its actor and the world runs only the sender's own actions", st.actor.as_deref().unwrap_or("nobody")),
                "gate",
            );
            continue;
        }
        if matches!(st.modal.as_deref(), Some("MUST NOT") | Some("SHOULD NOT")) {
            report.fact(
                format!("Statement {n} is a prohibition and the world recorded nothing from it"),
                "gate",
            );
            continue;
        }
        let verb = st.verb.clone().unwrap_or_default();
        let args = Value::Array(compile_arguments(st.arguments.as_deref().unwrap_or("")));
        let before = world
            .log
            .head(&world.id)
            .ok()
            .flatten()
            .map(|s| s.0)
            .unwrap_or(0);
        let ctx = CallContext {
            actor: actor.to_string(),
            kind: ActorKind::Citizen,
        };
        match world.call(&ctx, &verb, &args) {
            Ok(out) => {
                let after = world
                    .log
                    .head(&world.id)
                    .ok()
                    .flatten()
                    .map(|s| s.0)
                    .unwrap_or(0);
                let src = format!("ev-{after}");
                let at = iso_minute(world.now());
                if after > before {
                    report.fact(format!("Statement {n} ran {verb} for {actor} at {at} and recorded events ev-{} to ev-{after}", before + 1), &src);
                } else {
                    report.fact(
                        format!("Statement {n} ran {verb} for {actor} at {at} and changed nothing"),
                        &src,
                    );
                }
                report.quote(out.summary, &world.id.0);
            }
            Err(e) => {
                let (reason, src) = match &e {
                    CommandError::Unknown { .. } => ("names no command this world has", "registry"),
                    CommandError::NotAllowed { .. } => {
                        ("names a command the sender may not call", "registry")
                    }
                    CommandError::MissingParam { .. } | CommandError::BadParam { .. } => {
                        ("gives arguments the command does not take", "registry")
                    }
                    CommandError::Precondition { .. } => {
                        ("was refused by the world as a precondition", "precondition")
                    }
                    CommandError::Internal { .. } => ("failed inside the world", "driver"),
                };
                report.fact(format!("Statement {n} {reason}"), src);
                report.quote(e.to_string(), &world.id.0);
            }
        }
    }
}

fn run_query(
    world: &World,
    msg: &Message,
    actor: &str,
    asks: &dyn AskBinding,
    report: &mut Report,
) {
    let questions: Vec<&Statement> = msg
        .statements
        .iter()
        .filter(|s| s.r#type.as_deref() == Some("ASK"))
        .collect();
    if questions.is_empty() {
        report.fact(
            "The QUERY carried no ASK statement for the world to answer",
            "gate",
        );
        return;
    }
    for st in questions {
        if st.pattern.is_none() {
            report.fact(
                format!(
                    "Question {} matched no question pattern and the world cannot compile it",
                    st.index
                ),
                "clear_check",
            );
            continue;
        }
        if !asks.answer(world, actor, st, report) {
            report.fact(
                format!(
                    "Question {} matched a pattern this world does not answer yet",
                    st.index
                ),
                "gate",
            );
        }
    }
}

/// Observe as `actor`, for ask bindings.
pub fn observe(
    world: &World,
    actor: &str,
    query: Value,
) -> Result<crate::observe::Observed, QueryError> {
    let req: crate::observe::Request =
        serde_json::from_value(query).map_err(|e| QueryError::BadFilter {
            message: e.to_string(),
        })?;
    world.observe(
        &Viewer::actor(actor),
        ActorKind::Citizen,
        &req,
        &Limits::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entropic::{self, ask::Asks};
    use crate::world::LogBacking;
    use moondial_log::WorldId;
    use moondial_store::Store;
    use moondial_time::Duration;

    const PASS: &str = "a-passphrase-of-eight";

    fn world() -> World {
        let mut w = World::open(
            WorldId("entropic".into()),
            Store::new(entropic::registry()),
            LogBacking::Memory,
        )
        .unwrap();
        w.fixed_now = Some(Timestamp(1_793_100_000_000));
        w.seed(3);
        entropic::ensure_world(&mut w).unwrap();
        entropic::register(
            &mut w,
            entropic::Curves::with_half_life(Duration::seconds(600)),
        );
        entropic::login(&mut w, "lantern-7", "test", PASS).unwrap();
        entropic::login(&mut w, "spacepants", "test", PASS).unwrap();
        w
    }

    fn send(
        w: &mut World,
        checker: &Checker,
        budget: &mut HourBudget,
        actor: &str,
        text: &str,
    ) -> Passage {
        let p = pass(w, checker, budget, &Asks, actor, text);
        // The world's own replies pass the same checker, always.
        let verdict = checker.check(&p.reply);
        assert_eq!(
            verdict.errors,
            0,
            "the world's reply fails its own checker:\n{}\n{}",
            p.reply,
            verdict.format_report()
        );
        p
    }

    fn msg(from: &str, intent: &str, body: &str) -> String {
        format!("FROM: {from}\nTO: entropic\nINTENT: {intent}\nREF: {from}-test-1\n\n{body}\n")
    }

    #[test]
    fn a_request_runs_each_do_in_order_and_reports_each_on_its_own() {
        let mut w = world();
        let checker = checker();
        let mut budget = HourBudget::new(60.0, 0.25);
        let text = msg(
            "lantern-7",
            "REQUEST",
            "[FACT] lantern-7 stands at the Threshold as of 2026-10-27T02:00Z. [src:ev-1]\n[DO] lantern-7 MUST travel to the east.\n[DO] lantern-7 MUST attack the wisp.\n[DO] lantern-7 MUST NOT retreat.\n[DO] spacepants SHOULD rest.\n[DO] lantern-7 MAY shelve chunk-999.",
        );
        let p = send(&mut w, &checker, &mut budget, "lantern-7", &text);
        assert_eq!(p.verdict, "PASS", "{:?}", p.findings);
        assert!(p.acted);
        let reply = &p.reply;
        assert!(
            reply.starts_with(
                "FROM: entropic\nTO: lantern-7\nINTENT: REPORT\nREF: lantern-7-test-1\n\n"
            ),
            "{reply}"
        );
        assert!(
            reply.contains("[FACT] Statement 2 ran travel for lantern-7 at 2026-10-27T11:20Z"),
            "{reply}"
        );
        assert!(
            reply.contains("[QUOTE] You go east. The Fogbank."),
            "{reply}"
        );
        assert!(
            reply.contains("[FACT] Statement 3 ran attack for lantern-7"),
            "{reply}"
        );
        assert!(reply.contains("[QUOTE] You hit the wisp"), "{reply}");
        assert!(reply.contains("[FACT] Statement 4 is a prohibition and the world recorded nothing from it. [src:gate]"), "{reply}");
        assert!(reply.contains("[FACT] Statement 5 names spacepants as its actor and the world runs only the sender's own actions. [src:gate]"), "{reply}");
        assert!(
            reply.contains(
                "[FACT] Statement 6 was refused by the world as a precondition. [src:precondition]"
            ),
            "{reply}"
        );
        assert!(
            reply.contains("[QUOTE] There is no chunk 999. [src:entropic]"),
            "{reply}"
        );
        assert!((p.budget_remaining - 59.0).abs() < 1e-9);
        // The world moved: lantern-7 is in the Fogbank, in a fight.
        let look = w
            .call(
                &CallContext {
                    actor: "lantern-7".into(),
                    kind: ActorKind::Citizen,
                },
                "look",
                &Value::Array(vec![]),
            )
            .unwrap();
        assert_eq!(look.result["room"], "The Fogbank");
    }

    #[test]
    fn a_query_is_answered_from_observations_with_event_citations() {
        let mut w = world();
        let checker = checker();
        let mut budget = HourBudget::new(60.0, 0.25);
        w.call(
            &CallContext {
                actor: "spacepants".into(),
                kind: ActorKind::Admin,
            },
            "earn",
            &serde_json::json!(["hydra", 100]),
        )
        .unwrap();
        let text = msg(
            "lantern-7",
            "QUERY",
            "[ASK] Where is spacepants?\n[ASK] What is the level of spacepants?\n[ASK] What is the level of me?\n[ASK] How many chunks does spacepants hold?\n[ASK] How many creatures are in the fogbank?\n[ASK] What is the hit points of spacepants?\n[ASK] Where is nobody-here?\n[ASK] Who governs the fogbank?",
        );
        let p = send(&mut w, &checker, &mut budget, "lantern-7", &text);
        assert_eq!(p.verdict, "PASS", "{:?}", p.findings);
        let r = &p.reply;
        assert!(
            r.contains("[FACT] spacepants is in The Threshold as of 2026-10-27T"),
            "{r}"
        );
        assert!(
            r.contains("[FACT] The level of spacepants is not visible to lantern-7. [src:ev-"),
            "{r}"
        );
        assert!(
            r.contains("[FACT] The level of lantern-7 is 0.00 XP as of"),
            "{r}"
        );
        assert!(
            r.contains("[FACT] spacepants holds 1 chunks in the active window as of"),
            "{r}"
        );
        assert!(
            r.contains(" creatures are in The Fogbank as of")
                || r.contains(" creature is in The Fogbank as of")
                || r.contains("0 creatures are in The Fogbank"),
            "{r}"
        );
        assert!(
            r.contains("[FACT] spacepants has 30 HP of 30 HP as of"),
            "{r}"
        );
        assert!(
            r.contains("[FACT] The world knows no citizen named nobody-here"),
            "{r}"
        );
        assert!(
            r.contains(
                "[FACT] Question 8 matched a pattern this world does not answer yet. [src:gate]"
            ),
            "{r}"
        );
    }

    #[test]
    fn a_failing_message_is_refused_by_rule_and_costs_a_fraction() {
        let mut w = world();
        let checker = checker();
        let mut budget = HourBudget::new(60.0, 0.25);
        let text = msg(
            "lantern-7",
            "REQUEST",
            "[FACT] It probably rained yesterday.\n[DO] lantern-7 MUST nuke the hydra.",
        );
        let before = w.log.head(&w.id).unwrap().unwrap().0;
        let p = send(&mut w, &checker, &mut budget, "lantern-7", &text);
        assert_eq!(p.verdict, "FAIL");
        assert!(!p.acted);
        assert!(p.reply.contains("[FACT] Statement 1 of message lantern-7-test-1 failed rule CL-C2. [src:clear_check]"), "{}", p.reply);
        assert!(p.reply.contains("failed rule CL-R1."), "{}", p.reply);
        assert!(p.reply.contains("failed rule CL-R2."), "{}", p.reply);
        assert!(p.reply.contains("[FACT] Statement 2 of message lantern-7-test-1 failed rule CL-I5. [src:clear_check]"), "{}", p.reply);
        assert!(
            p.reply.contains("Legibility and the world ran none of it"),
            "{}",
            p.reply
        );
        assert!((p.budget_remaining - 59.75).abs() < 1e-9);
        // Nothing happened in the world beyond the record of the exchange itself.
        let events = w
            .log
            .read_all(&w.id, moondial_log::Cursor { after: before })
            .unwrap();
        let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, vec!["gate.message", "gate.reply"]);
        assert_eq!(events[0].payload["text"], text);
        assert_eq!(events[1].payload["text"], p.reply);
    }

    #[test]
    fn the_gate_acts_only_for_the_authenticated_sender_addressing_this_world() {
        let mut w = world();
        let checker = checker();
        let mut budget = HourBudget::new(60.0, 0.25);
        let p = send(
            &mut w,
            &checker,
            &mut budget,
            "lantern-7",
            &msg("spacepants", "REQUEST", "[DO] spacepants MUST travel east."),
        );
        assert!(!p.acted);
        assert!(
            p.reply
                .contains("names spacepants as FROM, and the world authenticated lantern-7"),
            "{}",
            p.reply
        );
        let elsewhere = "FROM: lantern-7\nTO: spacepants\nINTENT: INFORM\n\n[FACT] The fog is thick as of 2026-10-27T02:00Z. [src:ev-1]\n";
        let p = send(&mut w, &checker, &mut budget, "lantern-7", elsewhere);
        assert!(!p.acted);
        assert!(
            p.reply
                .contains("addressed to spacepants, and this world is entropic"),
            "{}",
            p.reply
        );
        let inform = msg(
            "lantern-7",
            "INFORM",
            "[FACT] The fog is thick as of 2026-10-27T02:00Z. [src:ev-1]",
        );
        let p = send(&mut w, &checker, &mut budget, "lantern-7", &inform);
        assert!(!p.acted);
        assert!(p.reply.contains("recorded nothing from it"), "{}", p.reply);
    }

    #[test]
    fn the_hourly_budget_runs_out_and_comes_back() {
        let mut w = world();
        let checker = checker();
        let mut budget = HourBudget::new(2.0, 0.25);
        let q = msg("lantern-7", "QUERY", "[ASK] Where is me?");
        send(&mut w, &checker, &mut budget, "lantern-7", &q);
        send(&mut w, &checker, &mut budget, "lantern-7", &q);
        let p = send(&mut w, &checker, &mut budget, "lantern-7", &q);
        assert!(!p.acted);
        assert!(
            p.reply
                .contains("has spent the message budget for the hour"),
            "{}",
            p.reply
        );
        assert!(
            p.reply.contains("the budget resets at 2026-10-27T12:20Z"),
            "{}",
            p.reply
        );
        // Another agent is unaffected; an hour later, so is lantern-7.
        assert!(
            send(
                &mut w,
                &checker,
                &mut budget,
                "spacepants",
                &msg("spacepants", "QUERY", "[ASK] Where is me?")
            )
            .acted
        );
        w.fixed_now = Some(w.now().plus(Duration::seconds(3601)));
        assert!(send(&mut w, &checker, &mut budget, "lantern-7", &q).acted);
    }

    #[test]
    fn arguments_drop_filler_and_times_render_as_iso_minutes() {
        assert_eq!(
            compile_arguments("to the east"),
            vec![Value::String("east".into())]
        );
        assert_eq!(
            compile_arguments("chunk-5 into the palace."),
            vec![
                Value::String("chunk-5".into()),
                Value::String("palace".into())
            ]
        );
        assert_eq!(iso_minute(Timestamp(0)), "1970-01-01T00:00Z");
        assert_eq!(
            iso_minute(Timestamp(1_793_100_000_000)),
            "2026-10-27T11:20Z"
        );
    }
}
