//! The CLEAR checker in the browser.
//!
//! `new Checker(vocabularyJson, name)` compiles a vocabulary once (the merged JSON the
//! driver serves at `/v0/vocabulary`, so the editor and the Gate read the same words);
//! `checker.check(text)` returns the same JSON `clear_check.py --json` prints: verdict,
//! score, findings with rule ids and where they fall, and the parsed message.

use wasm_bindgen::prelude::*;

/// A compiled vocabulary and the rules.
#[wasm_bindgen]
pub struct Checker {
    inner: moondial_clear::Checker,
}

#[wasm_bindgen]
impl Checker {
    /// Compile `vocabulary` (JSON text) under `name`.
    #[wasm_bindgen(constructor)]
    pub fn new(vocabulary: &str, name: &str) -> Result<Checker, JsError> {
        let vocab: serde_json::Value = serde_json::from_str(vocabulary)
            .map_err(|e| JsError::new(&format!("vocabulary is not JSON: {e}")))?;
        Ok(Checker {
            inner: moondial_clear::Checker::new(&vocab, name),
        })
    }

    /// Check a message; the result as JSON text.
    pub fn check(&self, text: &str) -> String {
        self.inner.check(text).to_json().to_string()
    }

    /// If `name` is a reserved word or a glossary alias, the term it collides with.
    pub fn collision(&self, name: &str) -> Option<String> {
        self.inner.collision(name)
    }
}

/// The crate's version, so a page can say which checker it runs.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
