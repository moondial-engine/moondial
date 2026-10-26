#!/usr/bin/env python3
"""clear_check.py: layer-1 Legibility check for CLEAR v0.3 messages.

CLEAR (Controlled Language for Exchange between Agents and Reviewers) is a controlled
output language for agent handoffs. This checker implements every rule a program can
test (the spec's section 4) and returns a verdict, a Legibility score and findings.

Usage:
    clear_check.py MESSAGE_FILE [--vocab VOCAB.yaml] [--json] [--mode auto|clear|prose]
    cat message.txt | clear_check.py - --json

Exit status: 0 PASS, 1 FAIL, 2 usage or input error.

Library:
    from clear_check import load_vocabulary, check_message
    result = check_message(text, load_vocabulary("vocab/entropic.yaml"))
    result.verdict, result.score, result.findings, result.message

The parsed message (result.message) is the structure a gate acts on: header fields,
typed statements, each DO split into actor, modal, verb and arguments, each ASK matched
to a question pattern, and every [src:] reference collected.

No dependency beyond PyYAML for .yaml vocabularies (.json vocabularies need nothing).
"""

from __future__ import annotations

import argparse
import copy
import json
import os
import re
import sys
import unicodedata
from dataclasses import dataclass, field, asdict
from typing import Any

__version__ = "0.3.0"

ERROR = "error"
WARNING = "warning"

# --------------------------------------------------------------------------------------
# Vocabulary
# --------------------------------------------------------------------------------------

DEFAULT_VOCAB = {
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
        "max_message_bytes": 16384,
    },
    "settings": {
        "allow_urls": True,
        "quote_requires_source": False,
        "ask_patterns_required": False,
        "prose_score_cap": 40,
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
    "glossary": {},
}


def _merge(base: dict, over: dict) -> dict:
    """Lists append (deduplicated, order kept), dicts merge recursively, scalars override."""
    out = copy.deepcopy(base)
    for key, val in over.items():
        if key == "extends":
            continue
        if isinstance(val, list) and isinstance(out.get(key), list):
            seen = list(out[key])
            for item in val:
                if item not in seen:
                    seen.append(item)
            out[key] = seen
        elif isinstance(val, dict) and isinstance(out.get(key), dict):
            out[key] = _merge(out[key], val)
        else:
            out[key] = copy.deepcopy(val)
    return out


def _read_vocab_file(path: str) -> dict:
    with open(path, "r", encoding="utf-8") as fh:
        text = fh.read()
    if path.endswith(".json"):
        return json.loads(text)
    try:
        import yaml  # type: ignore
    except ImportError as exc:  # pragma: no cover
        raise SystemExit("PyYAML is required for .yaml vocabularies (pip install pyyaml), "
                         "or supply a .json vocabulary") from exc
    data = yaml.safe_load(text) or {}
    if not isinstance(data, dict):
        raise SystemExit(f"vocabulary {path} must be a mapping")
    return data


def load_vocabulary(path: str | None = None) -> dict:
    """Load a vocabulary file, following `extends:` chains. None gives the built-in default."""
    if path is None:
        return copy.deepcopy(DEFAULT_VOCAB)
    data = _read_vocab_file(path)
    parent = data.get("extends")
    if parent:
        parent_path = parent if os.path.isabs(parent) else os.path.join(os.path.dirname(path), parent)
        base = load_vocabulary(parent_path)
    else:
        base = copy.deepcopy(DEFAULT_VOCAB)
    return _merge(base, data)


# --------------------------------------------------------------------------------------
# Data model
# --------------------------------------------------------------------------------------

@dataclass
class Finding:
    rule: str
    severity: str
    where: str          # "header", "message", or "statement N" (1-based)
    message: str
    excerpt: str = ""


@dataclass
class Statement:
    index: int                      # 1-based
    line: int                       # source line number, 1-based
    raw: str
    type: str | None                # FACT, INFER, DO, ASK, QUOTE, or None when untyped
    confidence: str | None          # for INFER
    body: str                       # text without the tag and without [src:] tokens
    sources: list[str] = field(default_factory=list)
    actor: str | None = None        # DO
    modal: str | None = None        # DO
    verb: str | None = None         # DO
    arguments: str | None = None    # DO
    pattern: str | None = None      # ASK: the matched question pattern
    slots: dict[str, str] = field(default_factory=dict)  # ASK: named groups


@dataclass
class Message:
    header: dict[str, str]
    statements: list[Statement]
    mode: str                       # "clear" or "prose"


@dataclass
class Result:
    verdict: str
    score: int
    mode: str
    errors: int
    warnings: int
    findings: list[Finding]
    message: Message
    vocabulary: str

    def to_dict(self) -> dict[str, Any]:
        d = asdict(self)
        return d

    def to_json(self, indent: int = 2) -> str:
        return json.dumps(self.to_dict(), indent=indent, ensure_ascii=False)


# --------------------------------------------------------------------------------------
# Patterns
# --------------------------------------------------------------------------------------

HEADER_RE = re.compile(r"^(FROM|TO|INTENT|REF)\s*:\s*(.*?)\s*$")
TAG_RE = re.compile(r"^\[(?P<tag>[A-Z]+)(?::(?P<conf>[A-Za-z]+))?\]\s*(?P<body>.*)$")
BAD_TAG_RE = re.compile(r"^\[[^\]]*\]")
SRC_RE = re.compile(r"\[src:\s*([^\]]*)\]")
SRC_OK_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9:_./-]*$")
IDENT_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.:@/-]*$")
DO_RE = re.compile(
    r"^(?P<actor>[A-Za-z0-9][A-Za-z0-9_.:@/-]*)\s+"
    r"(?P<modal>MUST NOT|MUST|SHOULD NOT|SHOULD|MAY)\s+"
    r"(?P<verb>[A-Za-z][A-Za-z'-]*)(?P<rest>.*)$"
)
URL_RE = re.compile(r"\b(?:https?://|www\.)\S+", re.IGNORECASE)
NUMBER_RE = re.compile(r"(?<![A-Za-z0-9_.:/-])(?P<num>\d[\d,]*(?:\.\d+)?)(?![\d,.]*[A-Za-z0-9_:/-])")
ISO_DATE_RE = re.compile(r"\b\d{4}-\d{2}-\d{2}(?:T\d{2}:\d{2}(?::\d{2})?(?:Z|[+-]\d{2}:\d{2})?)?\b")
TIME_RE = re.compile(r"\b\d{1,2}:\d{2}(?::\d{2})?\b")
ACRONYM_RE = re.compile(r"(?<![A-Za-z0-9-])([A-Z][A-Z0-9]{1,5})(?![A-Za-z0-9-])")
HEADER_SMUGGLE_RE = re.compile(r"(?:^|\s)(FROM|TO|INTENT|REF)\s*:", re.MULTILINE)
PRONOUNS_ALWAYS = ("it", "they", "them")
PRONOUNS_DEMONSTRATIVE = ("this", "that", "these", "those")
AUXILIARIES = {
    "is", "are", "was", "were", "be", "been", "being", "has", "have", "had", "will", "would",
    "can", "could", "should", "shall", "may", "might", "must", "does", "did", "do", "seems",
    "appears", "means", "shows", "makes", "needs", "requires", "includes", "contains",
    "causes", "happened", "failed", "works", "worked", "looks", "gets", "got", "went", "goes",
    "took", "takes", "says", "said", "holds", "held", "keeps", "kept", "comes", "came",
}
CONTROL_CHARS = {
    "​": "zero-width space", "‌": "zero-width non-joiner", "‍": "zero-width joiner",
    "⁠": "word joiner", "﻿": "byte-order mark", "‪": "bidi embedding",
    "‫": "bidi embedding", "‬": "bidi pop", "‭": "bidi override",
    "‮": "bidi override", "⁦": "bidi isolate", "⁧": "bidi isolate",
    "⁨": "bidi isolate", "⁩": "bidi isolate", "­": "soft hyphen",
}
SENTENCE_SPLIT_RE = re.compile(r"(?<=[.?!])\s+(?=[A-Z\[\"'(])")
WORD_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9'’.,:/_-]*")
RUNNING_ASOF_RE = re.compile(r"\bas of \d{4}-\d{2}-\d{2}\b", re.IGNORECASE)
IMPERATIVE_OBJECTS = {"the", "a", "an", "all", "every", "each", "your", "my", "our", "their", "its",
                      "it", "this", "that", "these", "those", "them", "me", "us", "him", "her",
                      "now", "immediately", "any", "everything", "nothing"}
NUMBER_STOPWORDS = {
    "and", "or", "of", "to", "in", "at", "on", "for", "by", "with", "from", "the", "a", "an",
    "is", "are", "was", "were", "will", "that", "which", "per", "but", "so", "if", "then",
    "than", "as", "this", "these", "those", "it", "its", "into", "onto", "over", "under",
    "each", "more", "less", "out", "up", "down", "off", "not", "no", "yet", "when", "while",
}


def _phrase_re(phrase: str) -> re.Pattern:
    """A whole-word, case-insensitive pattern for a word or phrase from the vocabulary."""
    esc = re.escape(phrase.strip()).replace(r"\ ", r"\s+")
    return re.compile(rf"(?<![A-Za-z0-9-]){esc}(?![A-Za-z0-9-])", re.IGNORECASE)


def _count_words(text: str) -> int:
    return len(WORD_RE.findall(text))


# --------------------------------------------------------------------------------------
# Parsing
# --------------------------------------------------------------------------------------

def parse_message(text: str) -> Message:
    """Split a message into header and statements. Untyped lines keep type None."""
    lines = text.splitlines()
    header: dict[str, str] = {}
    i = 0
    # Header: leading lines that match FIELD: value, up to the first blank line or non-header line.
    while i < len(lines):
        line = lines[i].strip()
        if not line:
            if header:
                i += 1
            break
        m = HEADER_RE.match(line)
        if not m:
            break
        header[m.group(1)] = m.group(2)
        i += 1
    mode = "clear" if header else "prose"
    statements: list[Statement] = []
    if mode == "clear":
        idx = 0
        for lineno in range(i, len(lines)):
            raw = lines[lineno].strip()
            if not raw:
                continue
            idx += 1
            statements.append(_parse_statement(idx, lineno + 1, raw))
    else:
        # Prose mode: every sentence is one untyped statement.
        body = " ".join(l.strip() for l in lines if l.strip())
        idx = 0
        for sent in SENTENCE_SPLIT_RE.split(body):
            sent = sent.strip()
            if not sent:
                continue
            idx += 1
            statements.append(_parse_statement(idx, 0, sent))
    return Message(header=header, statements=statements, mode=mode)


def _parse_statement(idx: int, lineno: int, raw: str) -> Statement:
    m = TAG_RE.match(raw)
    stype = conf = None
    body = raw
    if m:
        tag = m.group("tag")
        if tag in ("FACT", "INFER", "DO", "ASK", "QUOTE"):
            stype = tag
            conf = m.group("conf")
            body = m.group("body")
    sources = [s.strip() for s in SRC_RE.findall(body)]
    body = SRC_RE.sub("", body).strip()
    body = re.sub(r"\s{2,}", " ", body)
    st = Statement(index=idx, line=lineno, raw=raw, type=stype, confidence=conf, body=body, sources=sources)
    if stype == "DO":
        dm = DO_RE.match(body)
        if dm:
            st.actor = dm.group("actor")
            st.modal = dm.group("modal")
            st.verb = dm.group("verb").lower()
            st.arguments = dm.group("rest").strip().rstrip(".")
    return st


# --------------------------------------------------------------------------------------
# Checker
# --------------------------------------------------------------------------------------

class Checker:
    def __init__(self, vocab: dict, vocab_name: str = "default"):
        self.v = vocab
        self.vocab_name = vocab_name
        self.findings: list[Finding] = []
        lim = vocab.get("limits", {})
        self.max_words = int(lim.get("max_statement_words", 25))
        self.max_do_words = int(lim.get("max_instruction_words", 20))
        self.max_quote_words = int(lim.get("max_quote_words", 100))
        self.max_statements = int(lim.get("max_statements", 40))
        self.max_bytes = int(lim.get("max_message_bytes", 16384))
        st = vocab.get("settings", {})
        self.allow_urls = bool(st.get("allow_urls", True))
        self.quote_src_error = bool(st.get("quote_requires_source", False))
        self.ask_required = bool(st.get("ask_patterns_required", False))
        self.prose_cap = int(st.get("prose_score_cap", 40))
        self.intents = set(vocab.get("intents", []))
        self.confidences = set(vocab.get("confidence_levels", []))
        self.intent_rules = vocab.get("intent_rules", {}) or {}
        # Compiled word lists
        self.hedges = [(_phrase_re(h), h) for h in vocab.get("hedges", [])]
        self.intensifiers = [(_phrase_re(w), w) for w in vocab.get("vague_intensifiers", [])]
        self.vague_verbs = [(_phrase_re(w), w) for w in vocab.get("vague_verbs", [])]
        self.relative_time = [(_phrase_re(w), w) for w in vocab.get("relative_time", [])]
        self.running_periods = [(re.compile(rf"\b{re.escape(p)}\b"), p) for p in vocab.get("running_periods", [])]
        self.synonyms = [(_phrase_re(k), k, str(val)) for k, val in (vocab.get("approved_synonyms") or {}).items()]
        self.units = sorted((vocab.get("units") or []), key=len, reverse=True)
        self.identifier_words = [w.lower() for w in (vocab.get("identifier_words") or [])]
        self.approved_acronyms = {k.upper(): str(val) for k, val in (vocab.get("approved_acronyms") or {}).items()}
        self.ambiguous_acronyms = {k.upper(): val for k, val in (vocab.get("ambiguous_acronyms") or {}).items()}
        self.directives = [(_phrase_re(d) if not d.isupper() else re.compile(rf"(?<![A-Za-z]){re.escape(d)}(?![A-Za-z])"), d)
                           for d in vocab.get("directive_markers", [])]
        self.soft = [(_phrase_re(s), s) for s in vocab.get("soft_obligations", [])]
        self.irregular = set(w.lower() for w in vocab.get("irregular_participles", []))
        self.adjectival = set(w.lower() for w in vocab.get("adjectival_participles", []))
        self.common_verbs = set(w.lower() for w in vocab.get("common_verbs", []))
        self.approved_verbs = set(w.lower() for w in vocab.get("approved_verbs", []))
        self.ask_patterns = [re.compile(p, re.IGNORECASE) for p in vocab.get("ask_patterns", [])]
        self.glossary = vocab.get("glossary") or {}
        self.aliases: list[tuple[re.Pattern, str, str]] = []
        for term, entry in self.glossary.items():
            for alias in (entry or {}).get("aliases", []) or []:
                self.aliases.append((_phrase_re(alias), alias, term))
        # Tokens that look like acronyms but are structural.
        self.structural_upper = {"FACT", "INFER", "DO", "ASK", "QUOTE", "MUST", "SHOULD", "MAY", "NOT",
                                 "FROM", "TO", "INTENT", "REF", "SRC"} | self.intents | self.confidences
        self.unit_upper = {u.upper() for u in self.units if u.isupper()}
        self.passive_re = re.compile(
            r"\b(?:is|are|was|were|be|been|being|get|gets|got|gotten)\s+(?:(?:not|also|still|already|never|being)\s+)?"
            r"(?P<part>[A-Za-z]+(?:ed|en)|" + "|".join(sorted(self.irregular, key=len, reverse=True) or ["\u0000"]) + r")\b",
            re.IGNORECASE,
        )

    # -- helpers ---------------------------------------------------------------------
    def add(self, rule: str, severity: str, where: str, message: str, excerpt: str = "") -> None:
        self.findings.append(Finding(rule=rule, severity=severity, where=where, message=message, excerpt=excerpt))

    def where(self, st: Statement) -> str:
        return f"statement {st.index}"

    # -- entry -----------------------------------------------------------------------
    def check(self, text: str) -> Result:
        self.findings = []
        self.check_characters(text)
        msg = parse_message(text)
        if msg.mode == "clear":
            self.check_header(msg)
            self.check_message_shape(msg, text)
            for st in msg.statements:
                self.check_statement(st, msg)
            self.check_intent_composition(msg)
            self.check_duplicates(msg)
        else:
            self.add("CL-H1", ERROR, "header", "No CLEAR header. The message is checked in prose mode and its score is capped.")
            self.add("CL-S1", ERROR, "message", "No statement has a type tag. Untyped text mixes facts, guesses and orders.")
            for st in msg.statements:
                self.check_content_rules(st, msg, prose=True)
        return self.finish(msg)

    def finish(self, msg: Message) -> Result:
        errors = sum(1 for f in self.findings if f.severity == ERROR)
        warnings = sum(1 for f in self.findings if f.severity == WARNING)
        total = len(msg.statements)
        if total:
            bad = {f.where for f in self.findings if f.severity == ERROR and f.where.startswith("statement ")}
            score = round(100 * (total - len(bad)) / total)
        else:
            score = 0
        if msg.mode == "prose":
            score = min(score, self.prose_cap)
        verdict = "PASS" if errors == 0 else "FAIL"
        order = {ERROR: 0, WARNING: 1}
        self.findings.sort(key=lambda f: (order[f.severity], _where_key(f.where), f.rule))
        return Result(verdict=verdict, score=score, mode=msg.mode, errors=errors, warnings=warnings,
                      findings=self.findings, message=msg, vocabulary=self.vocab_name)

    # -- message-level ---------------------------------------------------------------
    def check_characters(self, text: str) -> None:
        found = {}
        for ch in text:
            if ch in CONTROL_CHARS:
                found[ch] = CONTROL_CHARS[ch]
            elif unicodedata.category(ch) in ("Cc", "Cf") and ch not in "\n\r\t":
                found[ch] = f"control character U+{ord(ch):04X}"
        for ch, name in found.items():
            self.add("CL-X3", ERROR, "message", f"Message contains a {name} (U+{ord(ch):04X}). Hidden characters are not allowed.")

    def check_header(self, msg: Message) -> None:
        h = msg.header
        for fld in ("FROM", "TO", "INTENT"):
            if not h.get(fld):
                self.add("CL-H1", ERROR, "header", f"Header is missing {fld}.")
        intent = h.get("INTENT", "")
        if intent and intent not in self.intents:
            self.add("CL-H1", ERROR, "header", f"INTENT '{intent}' is not approved. Use one of: {', '.join(sorted(self.intents))}.", intent)
        for fld in ("FROM", "TO"):
            val = h.get(fld)
            if val and not IDENT_RE.match(val):
                self.add("CL-H3", ERROR, "header", f"{fld} must be an identifier (letters, digits, - _ . : @ /), not '{val}'.", val)
        if h.get("FROM") and h.get("FROM") == h.get("TO"):
            self.add("CL-H3", WARNING, "header", "FROM and TO are the same actor.")
        ref = h.get("REF")
        if ref is not None and ref and not IDENT_RE.match(ref):
            self.add("CL-H3", ERROR, "header", f"REF must be an identifier, not '{ref}'.", ref)

    def check_message_shape(self, msg: Message, text: str) -> None:
        n = len(msg.statements)
        if n == 0:
            self.add("CL-S1", ERROR, "message", "The message has a header but no statements.")
        if n > self.max_statements:
            self.add("CL-M1", ERROR, "message", f"{n} statements exceed the limit of {self.max_statements}.")
        size = len(text.encode("utf-8"))
        if size > self.max_bytes:
            self.add("CL-M1", ERROR, "message", f"{size} bytes exceed the limit of {self.max_bytes} bytes.")

    def check_intent_composition(self, msg: Message) -> None:
        intent = msg.header.get("INTENT")
        rule = self.intent_rules.get(intent) if intent else None
        if not rule:
            return
        types = {st.type for st in msg.statements if st.type}
        for req in rule.get("requires", []) or []:
            if req not in types:
                self.add("CL-H2", ERROR, "header", f"INTENT {intent} requires at least one [{req}] statement.")
        for forb in rule.get("forbids", []) or []:
            if forb in types:
                self.add("CL-H2", ERROR, "header", f"INTENT {intent} must not carry [{forb}] statements. Use REQUEST or PROPOSE for instructions.")

    def check_duplicates(self, msg: Message) -> None:
        seen: dict[str, int] = {}
        for st in msg.statements:
            key = (st.type or "") + "|" + st.body.lower()
            if key in seen:
                self.add("CL-S4", WARNING, self.where(st), f"Statement repeats statement {seen[key]}.", st.body)
            else:
                seen[key] = st.index

    # -- statement-level -------------------------------------------------------------
    def check_statement(self, st: Statement, msg: Message) -> None:
        w = self.where(st)
        if st.type is None:
            if BAD_TAG_RE.match(st.raw):
                self.add("CL-S1", ERROR, w, "Unknown statement tag. Use [FACT], [INFER:LEVEL], [DO], [ASK] or [QUOTE].", st.raw[:40])
            else:
                self.add("CL-S1", ERROR, w, "Statement has no type tag.", st.raw[:60])
            self.check_content_rules(st, msg, prose=False)
            return
        if st.type == "INFER":
            if not st.confidence or st.confidence.upper() not in self.confidences:
                self.add("CL-C1", ERROR, w, "INFER needs a confidence level: [INFER:HIGH], [INFER:MEDIUM] or [INFER:LOW].", st.raw[:30])
        elif st.confidence:
            self.add("CL-S1", ERROR, w, f"[{st.type}] does not take a qualifier; only INFER carries a confidence level.", st.raw[:30])
        if st.type == "QUOTE":
            self.check_quote(st)
            return
        self.check_structure_rules(st)
        self.check_content_rules(st, msg, prose=False)
        if st.type == "DO":
            self.check_do(st, msg)
        if st.type == "ASK":
            self.check_ask(st)

    def check_quote(self, st: Statement) -> None:
        w = self.where(st)
        if not st.sources:
            sev = ERROR if self.quote_src_error else WARNING
            self.add("CL-Q1", sev, w, "QUOTE must cite where the text came from with [src:...].")
        n = _count_words(st.body)
        if n > self.max_quote_words:
            self.add("CL-Q2", WARNING, w, f"QUOTE has {n} words; the limit is {self.max_quote_words}.")
        self.check_sources(st)
        if HEADER_SMUGGLE_RE.search(st.body):
            self.add("CL-X2", ERROR, w, "QUOTE contains header-like text (FROM:, TO:, INTENT:, REF:). Header fields may not appear inside statements.")

    def check_structure_rules(self, st: Statement) -> None:
        w = self.where(st)
        # CL-S2 one sentence
        parts = [p for p in SENTENCE_SPLIT_RE.split(st.body) if p.strip()]
        if len(parts) > 1:
            self.add("CL-S2", ERROR, w, f"Statement holds {len(parts)} sentences; one sentence per statement.", st.body[:60])
        # CL-S3 length
        n = _count_words(st.body)
        limit = self.max_do_words if st.type == "DO" else self.max_words
        if n > limit:
            self.add("CL-S3", ERROR, w, f"Statement has {n} words; the limit is {limit}.")
        # CL-S5 terminal punctuation
        if st.type == "ASK":
            pass  # CL-A1 covers it
        elif st.body and st.body[-1] not in ".":
            self.add("CL-S5", WARNING, w, "Statement does not end with a period.", st.body[-20:])

    def check_sources(self, st: Statement) -> None:
        w = self.where(st)
        for src in st.sources:
            if not src or not SRC_OK_RE.match(src):
                self.add("CL-C4", ERROR, w, f"Source reference '[src:{src}]' is not a resolvable identifier.", src)

    def check_content_rules(self, st: Statement, msg: Message, prose: bool) -> None:
        """Rules that apply to the words of a FACT, INFER, DO or ASK (and prose sentences)."""
        w = self.where(st)
        body = st.body
        stype = st.type
        self.check_sources(st)
        # CL-C3 FACT cites a source
        if stype == "FACT" and not st.sources:
            self.add("CL-C3", WARNING, w, "FACT does not cite a source with [src:...].")
        # CL-C2 hedges
        if stype in ("FACT", "INFER") or prose or stype is None:
            for pat, word in self.hedges:
                m = pat.search(body)
                if m and word.lower() in ("about", "around", "roughly", "approximately"):
                    # "about 250 USD" is a hedge; "a page about rain" is not.
                    if not re.match(r"\s+\d", body[m.end():]):
                        continue
                if m:
                    if stype == "INFER":
                        self.add("CL-C2", WARNING, w, f"Hedge '{word}' in an INFER; the confidence level already says how sure you are.", word)
                    else:
                        self.add("CL-C2", ERROR, w, f"Hedge '{word}' in a FACT. A hedged fact is an inference: use [INFER:LEVEL].", word)
        # CL-I4 soft obligations outside DO
        if stype != "DO":
            for pat, word in self.soft:
                for m in pat.finditer(body):
                    tok = m.group(0)
                    if tok.isupper():
                        continue  # uppercase modals are caught by CL-X1
                    self.add("CL-I4", ERROR, w, f"Soft obligation '{tok}' outside a DO statement. An obligation must be a [DO] with an actor and a strength.", tok)
                    break
        # CL-I3 passive voice (warning outside DO)
        if stype != "DO":
            m = self.passive_re.search(body)
            if m and m.group("part").lower() not in self.adjectival:
                self.add("CL-I3", WARNING, w, f"Passive voice ('{m.group(0)}') hides the actor.", m.group(0))
        # CL-R1 vague pronouns at the start of a clause
        pron = self.find_clause_pronoun(body)
        if pron:
            self.add("CL-R1", ERROR, w, f"Vague pronoun '{pron}' starts a clause. Name the thing it refers to.", pron)
        # CL-R2 relative time
        for pat, word in self.relative_time:
            if pat.search(body):
                self.add("CL-R2", ERROR, w, f"Relative time '{word}'. Use an absolute date or a numbered turn.", word)
        for pat, period in self.running_periods:
            if pat.search(body) and not RUNNING_ASOF_RE.search(body):
                self.add("CL-R2", ERROR, w, f"Running period {period} needs an end date: 'as of YYYY-MM-DD'.", period)
        # CL-R3 units
        for num in self.find_unitless_numbers(body):
            self.add("CL-R3", WARNING, w, f"'{num}' has no unit.", num)
        # CL-V1 approved words
        for pat, word, approved in self.synonyms:
            if pat.search(body):
                self.add("CL-V1", WARNING, w, f"Use '{approved}', not '{word}'.", word)
        # CL-V2 glossary aliases
        for pat, alias, term in self.aliases:
            if pat.search(body):
                self.add("CL-V2", ERROR, w, f"Alias '{alias}'. The glossary term is '{term}'.", alias)
        # CL-V3 / CL-V6 acronyms
        for acr in self.find_acronyms(body):
            if acr in self.ambiguous_acronyms:
                meanings = self.ambiguous_acronyms[acr]
                self.add("CL-V6", ERROR, w, f"Acronym {acr} has two meanings ({', '.join(map(str, meanings))}). Write the term out.", acr)
            elif acr not in self.approved_acronyms:
                self.add("CL-V3", WARNING, w, f"Acronym {acr} is not in the glossary.", acr)
        # CL-V4 intensifiers
        for pat, word in self.intensifiers:
            if pat.search(body):
                self.add("CL-V4", WARNING, w, f"Vague intensifier '{word}' carries no measurable meaning.", word)
        # CL-V5 vague verbs
        for pat, word in self.vague_verbs:
            if pat.search(body):
                self.add("CL-V5", WARNING, w, f"Vague verb '{word}' says something changed without saying what.", word)
        # CL-X1 directives in descriptive statements
        if stype in ("FACT", "INFER", "ASK") or prose or stype is None:
            hit = self.find_directive(body)
            if hit:
                self.add("CL-X1", ERROR, w, f"Directive '{hit}' inside a {stype or 'descriptive'} statement. Instructions belong in [DO]; carried text belongs in [QUOTE].", hit)
        # CL-X2 header smuggling
        if HEADER_SMUGGLE_RE.search(body):
            self.add("CL-X2", ERROR, w, "Header-like text (FROM:, TO:, INTENT:, REF:) inside a statement.")
        # CL-X4 URLs
        if not self.allow_urls and URL_RE.search(body):
            self.add("CL-X4", ERROR, w, "URLs are not allowed in this domain outside a QUOTE.", URL_RE.search(body).group(0))

    def check_do(self, st: Statement, msg: Message) -> None:
        w = self.where(st)
        if st.actor is None:
            self.add("CL-I1", ERROR, w, "DO must read '<actor> MUST|MUST NOT|SHOULD|SHOULD NOT|MAY <verb> ...'.", st.body[:60])
            return
        rest = st.arguments or ""
        # CL-I2 one action
        m = re.search(r"\b(?:and then|then|and)\s+(?:(?:not|also|immediately)\s+)?([a-z][a-z'-]*)\b", rest, re.IGNORECASE)
        if m and (m.group(1).lower() in self.common_verbs or m.group(1).lower() in self.approved_verbs):
            self.add("CL-I2", ERROR, w, f"DO carries a second action ('{m.group(0)}'). One action per statement.", m.group(0))
        if re.search(r";\s*[a-z]", rest):
            self.add("CL-I2", ERROR, w, "DO carries a second clause after ';'. One action per statement.")
        # CL-I3 active voice is an error in DO
        pm = self.passive_re.search(st.body)
        if pm and pm.group("part").lower() not in self.adjectival:
            self.add("CL-I3", ERROR, w, f"DO in passive voice ('{pm.group(0)}'). Name who acts.", pm.group(0))
        # CL-I5 approved verb
        if self.approved_verbs and st.verb not in self.approved_verbs:
            sender = msg.header.get("FROM", "")
            self_actor = st.actor in (sender, "I", "i") if sender else False
            sev = ERROR if self_actor else WARNING
            self.add("CL-I5", sev, w, f"Verb '{st.verb}' is not an approved action verb"
                     + (" for the sender; the driver cannot compile it." if self_actor else "."), st.verb)

    def check_ask(self, st: Statement) -> None:
        w = self.where(st)
        body = st.body
        q = body.count("?")
        if q != 1 or not body.endswith("?"):
            self.add("CL-A1", ERROR, w, "ASK must be exactly one question, ending in '?'.", body[-30:])
            return
        if self.ask_patterns:
            for pat in self.ask_patterns:
                m = pat.match(body)
                if m:
                    st.pattern = pat.pattern
                    st.slots = {k: v for k, v in m.groupdict().items() if v is not None}
                    return
            sev = ERROR if self.ask_required else WARNING
            self.add("CL-A2", sev, w, "ASK matches none of the domain's question patterns, so the gate cannot compile it.", body[:60])

    # -- word-level helpers ----------------------------------------------------------
    def find_clause_pronoun(self, body: str) -> str | None:
        tokens = re.findall(r"[A-Za-z']+|[,;:.]", body)
        clause_start = True
        for i, tok in enumerate(tokens):
            low = tok.lower()
            if tok in ",;:.":
                clause_start = True
                continue
            if clause_start:
                nxt = tokens[i + 1].lower() if i + 1 < len(tokens) else ""
                if low in PRONOUNS_ALWAYS:
                    return tok
                if low in PRONOUNS_DEMONSTRATIVE and (nxt in AUXILIARIES or nxt in ",;:." or nxt == ""):
                    return tok
            clause_start = low in ("and", "but", "so", "because", "if", "when", "while", "then", "although", "since", "unless", "or")
        return None

    def find_unitless_numbers(self, body: str) -> list[str]:
        out = []
        scrubbed = ISO_DATE_RE.sub(" ", body)
        scrubbed = TIME_RE.sub(" ", scrubbed)
        for m in NUMBER_RE.finditer(scrubbed):
            num = m.group("num")
            after = scrubbed[m.end():].lstrip()
            before = scrubbed[:m.start()].rstrip()
            if after.startswith("%"):
                continue
            next_word = re.match(r"[A-Za-z%][A-Za-z%.]*", after)
            if next_word:
                nw = next_word.group(0)
                if self._is_unit(nw):
                    continue
                # A count noun ("3 integrations", "2 actors") is a unit. A function word
                # ("250 and", "3 of") leaves the number bare.
                if nw.lower().rstrip(".,") not in NUMBER_STOPWORDS and len(nw) >= 3 and nw[0].islower():
                    continue
            prev = re.search(r"([A-Za-z#.]+)$", before)
            if prev and prev.group(1).lower().rstrip(".") in [w.rstrip(".") for w in self.identifier_words]:
                continue
            if before.endswith("#"):
                continue
            out.append(num)
        return out

    def _is_unit(self, word: str) -> bool:
        w = word.rstrip(".,")
        for u in self.units:
            if w == u or w.lower() == u.lower():
                return True
        return False

    def find_acronyms(self, body: str) -> list[str]:
        out = []
        for m in ACRONYM_RE.finditer(body):
            tok = m.group(1)
            if tok in self.structural_upper or tok in self.unit_upper:
                continue
            if tok.isdigit():
                continue
            if tok not in out:
                out.append(tok)
        return out

    def find_directive(self, body: str) -> str | None:
        for pat, word in self.directives:
            m = pat.search(body)
            if m:
                return m.group(0)
        # An imperative at the start of the statement: a verb from the lists followed by a
        # determiner or object pronoun ("Delete the record", "Set all stages"). "Attack
        # damage is 5 HP" starts with the same word and is not an order.
        first = re.match(r"([A-Za-z]+)\s+([A-Za-z]+)", body)
        if first and first.group(1).lower() in (self.common_verbs | self.approved_verbs) \
                and first.group(1)[0].isupper() and first.group(2).lower() in IMPERATIVE_OBJECTS:
            return first.group(1)
        return None


def _where_key(where: str) -> int:
    if where == "header":
        return -2
    if where == "message":
        return -1
    try:
        return int(where.split()[-1])
    except ValueError:
        return 0


# --------------------------------------------------------------------------------------
# Public API
# --------------------------------------------------------------------------------------

def check_message(text: str, vocab: dict | None = None, vocab_name: str | None = None) -> Result:
    vocab = vocab or copy.deepcopy(DEFAULT_VOCAB)
    name = vocab_name or vocab.get("world") or vocab.get("mode", "default")
    return Checker(vocab, str(name)).check(text)


def format_report(result: Result) -> str:
    lines = [f"Verdict: {result.verdict}   Legibility: {result.score}/100   "
             f"errors: {result.errors}   warnings: {result.warnings}   mode: {result.mode}   vocabulary: {result.vocabulary}"]
    if not result.findings:
        lines.append("No findings.")
    for f in result.findings:
        tag = "E" if f.severity == ERROR else "W"
        exc = f"  [{f.excerpt}]" if f.excerpt else ""
        lines.append(f"  {tag} {f.rule:<6} {f.where:<13} {f.message}{exc}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="CLEAR v0.3 layer-1 Legibility check")
    ap.add_argument("file", help="message file, or - for stdin")
    ap.add_argument("--vocab", help="vocabulary file (.yaml or .json); default is the built-in minimal vocabulary")
    ap.add_argument("--json", action="store_true", help="print the result as JSON")
    ap.add_argument("--mode", choices=["auto", "clear", "prose"], default="auto",
                    help="auto detects a header; clear fails a message without one; prose forces prose mode")
    ap.add_argument("--version", action="version", version=f"clear_check {__version__}")
    args = ap.parse_args(argv)
    try:
        text = sys.stdin.read() if args.file == "-" else open(args.file, "r", encoding="utf-8").read()
    except OSError as exc:
        print(f"clear_check: {exc}", file=sys.stderr)
        return 2
    try:
        vocab = load_vocabulary(args.vocab)
    except (OSError, SystemExit) as exc:
        print(f"clear_check: {exc}", file=sys.stderr)
        return 2
    name = args.vocab and os.path.splitext(os.path.basename(args.vocab))[0]
    if args.mode == "prose":
        text = "\n".join(l for l in text.splitlines() if not HEADER_RE.match(l.strip()))
    result = check_message(text, vocab, name)
    if args.mode == "clear" and result.mode == "prose":
        result.verdict = "FAIL"
    print(result.to_json() if args.json else format_report(result))
    return 0 if result.verdict == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
