# CLEAR v0.4 checker

Layer-1 Legibility check for CLEAR (Controlled Language for Exchange between Agents and Reviewers) messages, with the v0.3 spec (plus the v0.4 argument grammar, CL-I6) and vocabularies for an organization and for the Disremembered game world.

```
clear_check.py          the checker (library + CLI), Python 3.10+, PyYAML for .yaml vocabularies
clear-spec-v0.3.md      the spec; v0.2 rules kept, game-engine rules added and marked NEW
vocab/base.yaml         universal word lists every domain inherits
vocab/org-example.yaml  a sales / partner GTM vocabulary (the spec's own examples)
vocab/disremembered.yaml     the Disremembered world: registry verbs, question patterns, glossary
samples/                nine messages: the spec's three, plus Disremembered traffic both ways
tests/                  pytest suite, one test per rule plus the sample verdicts
```

## Use

```
python3 clear_check.py samples/06_invitation.txt --vocab vocab/disremembered.yaml
python3 clear_check.py samples/01_prose_handoff.txt --vocab vocab/org-example.yaml --json
cat message.txt | python3 clear_check.py - --vocab vocab/disremembered.yaml
```

Exit status 0 is PASS, 1 is FAIL, 2 is a usage or input error.

From Python:

```python
from clear_check import load_vocabulary, check_message
vocab = load_vocabulary("vocab/disremembered.yaml")
result = check_message(text, vocab)
result.verdict, result.score, result.findings
for st in result.message.statements:
    if st.type == "DO":
        st.actor, st.modal, st.verb, st.arguments     # what a gate compiles into a command
    if st.type == "ASK":
        st.pattern, st.slots                          # the query pattern and its arguments
```

## Tests

```
pip install pytest pyyaml
python3 -m pytest tests/
```

## Adding a domain

Create `vocab/<domain>.yaml` with `extends: base.yaml`, then add the glossary (term, definition, forbidden aliases), the approved verbs if the receiver has a command registry, the question patterns if it compiles questions, and the domain's units, acronyms and relative-time terms. Lists append to the base; `limits` and `settings` override it.
