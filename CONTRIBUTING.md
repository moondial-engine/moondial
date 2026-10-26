# Contributing to Moondial

Moondial is pre-alpha. The architecture is written, the driver is being built one primitive at a time, and the first city (Entropic) is the forcing function for every primitive. Until the first slice is playable in a terminal, the most useful contributions are small and sharp: a failing test, a bug in a primitive, a clearer doc, a false positive in the CLEAR checker.

## Ground rules

- **Nothing enters the driver unless a city needs it.** A primitive without a module that uses it is a proposal, not a pull request.
- **The driver knows no game.** If a pull request teaches the driver a word from Entropic (chunk, palace, reminiscence), a general primitive is missing; propose that instead.
- **Text first.** A feature that cannot be played or observed through the text protocol is not done.
- **Every primitive has property tests.** The event log is the oracle: lazy evaluation must agree with replay.

## Sign your commits (DCO)

Every commit must carry a `Signed-off-by:` line certifying the [Developer Certificate of Origin](https://developercertificate.org/): that you wrote the change or have the right to submit it under Apache-2.0. `git commit -s` adds the line.

## Before you push

```
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python -m pytest clear/tests -q
```

CI runs the same four commands plus the site gate, which refuses to deploy an invitation that fails its own checker.

## Layout

```
crates/moondial-time   time-evaluated state: curves over a waterline, pausable clock scopes
crates/moondial        the driver (assembles the primitives; pre-alpha)
clear/                 the CLEAR v0.3 spec, vocabularies, reference checker and its tests
site/                  agentscanplay.com and moondial.ai, and their deploy pipeline
```

## CLEAR

Changes to the checker need a test in `clear/tests/` and, if they change a rule, a line in the spec. The published copies under `site/sites/agentscanplay/clear/` must match the files under `clear/`; CI checks this with `cmp`.
