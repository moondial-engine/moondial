//! The port is held to the reference checker: every case in `clear/tests/oracle/` must
//! produce the same JSON as `clear_check.py --json` did. Regenerate the corpus with
//! `python3 tools/make_oracle.py` from `clear/` when the reference changes.

use moondial_clear::{check_message, default_vocab, load_vocabulary, Checker};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn clear_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../clear")
}

#[test]
fn every_oracle_case_matches_the_reference_checker() {
    let dir = clear_dir().join("tests/oracle");
    let manifest: Vec<Value> = serde_json::from_str(
        &std::fs::read_to_string(dir.join("manifest.json")).expect("manifest"),
    )
    .unwrap();
    assert!(
        manifest.len() >= 100,
        "the oracle has {} cases; expected the full corpus",
        manifest.len()
    );
    let mut failures = Vec::new();
    let mut checkers: std::collections::HashMap<String, Checker> = std::collections::HashMap::new();
    let default = default_vocab();
    for case in &manifest {
        let name = case["name"].as_str().unwrap();
        let text = std::fs::read_to_string(dir.join(case["text"].as_str().unwrap())).unwrap();
        let expected: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(case["expected"].as_str().unwrap())).unwrap(),
        )
        .unwrap();
        let key = case["vocab"].as_str().unwrap_or("").to_string();
        let checker = checkers
            .entry(key.clone())
            .or_insert_with(|| match case["vocab"].as_str() {
                Some(rel) => {
                    let v = load_vocabulary(&clear_dir().join(rel)).unwrap();
                    let stem = Path::new(rel).file_stem().unwrap().to_str().unwrap();
                    let vocab_name = if stem == "org-example" { "org" } else { stem };
                    Checker::new(&v, vocab_name)
                }
                None => Checker::new(&default, "org"),
            });
        let got = checker.check(&text).to_json();
        if got != expected {
            failures.push(format!(
                "{name}:\n  expected {}\n  got      {}",
                compact(&expected),
                compact(&got)
            ));
        }
    }
    if !failures.is_empty() {
        panic!(
            "{} of {} oracle cases differ:\n{}",
            failures.len(),
            manifest.len(),
            failures.join("\n")
        );
    }
}

fn compact(v: &Value) -> String {
    let s = serde_json::to_string(v).unwrap();
    if s.len() > 1200 {
        format!("{}…", &s[..1200])
    } else {
        s
    }
}

#[test]
fn the_report_prints_like_the_reference() {
    let vocab = load_vocabulary(&clear_dir().join("vocab/entropic.yaml")).unwrap();
    let text =
        std::fs::read_to_string(clear_dir().join("samples/09_entropic_bad_request.txt")).unwrap();
    let r = check_message(&text, Some(&vocab), Some("entropic"));
    let report = r.format_report();
    assert!(
        report.starts_with("Verdict: FAIL   Legibility:"),
        "{report}"
    );
    assert!(report.contains("  E CL-"), "{report}");
}
