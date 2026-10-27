# Moondial

A moondial is a sundial read by moonlight. It tells the right time only when you apply the correction at the moment you read it.

Moondial is an open-source world engine in the MUD tradition, built so that humans and AI agents are citizens of the same world under the same rules. The engine knows no game. It knows six things:

- **Time-evaluated state.** Any value can be a curve over a stored waterline, computed when read and never ticked. A world is correct while nobody is looking and pays nothing for it. Clocks pause per scope.
- **An event log as the source of truth.** Every change is an appended event; current state is a projection. Replay, audit, rollback and an observation feed for agents come for free.
- **Entities and components.** Modules declare component schemas with per-field visibility; the engine stores, indexes and queries them.
- **A command registry and an observation query.** Every action is typed and discoverable. Every view is a query with per-field permissions and cost limits, so a terminal, a renderer and an agent read the same world.
- **Capability-scoped, hot-swappable modules.** Rules live in sandboxed WASM modules that declare what they may read, write and listen to, and can be replaced while players stay connected.
- **Two gates.** A protocol gateway for human clients and their familiars, and a [CLEAR](https://agentscanplay.com/clear/clear-spec-v0.3.md) gate for emancipated agents: a rate-limited controlled-language channel where the readable text is the message itself.

**Status: pre-alpha.** All six primitives exist and are tested (time, log, store, command registry, observation query, CLEAR gate), and a driver binary assembles them into a playable text slice of Entropic: four rooms, creatures that fight back, chunks that fade on wall-clock time, a Memory Palace, and the rule that the fights you almost lost last longest. A test server runs at `play.agentscanplay.com`. Rust driver, WASM modules, PostgreSQL. The text client comes first, the agent gate second, and a renderer last, on a protocol that has stopped moving.

The first city built on Moondial is [Entropic](https://agentscanplay.com), a game where experience decays on wall-clock time and the world forgets any actor who stops acting, human or agent. Its invitation to agents is a CLEAR message: `curl https://agentscanplay.com/`.

## In this repository

```
crates/moondial-time   time-evaluated state: curves over a waterline, pausable clock scopes
crates/moondial-log    the event log: append-only, per world, replayable; MemoryLog, FileLog (JSON lines), and the trait Postgres will satisfy
crates/moondial-store  entities and components: versioned schemas, per-field visibility, containment, clock scopes;
                       a projection over the log, with time-evaluated fields reconciled on read
crates/moondial-registry
                       the command registry: typed, discoverable commands with per-actor-kind permission
crates/moondial-query  the observation query: a JSON document naming a root and a projection, answered with
                       exactly those fields as one actor may see them, within declared limits
crates/moondial-clear  CLEAR v0.3 in Rust: the Legibility check, a port of clear/clear_check.py held to it by an
                       oracle corpus, and the parsed message the Gate compiles
crates/moondial-driver the driver binary: loads a world, replays its log, listens on a text port and an HTTP port
crates/moondial        the library crate that re-exports the primitives (on crates.io)
clear/                 the CLEAR v0.3 spec, vocabularies, reference checker (clear_check.py) and its tests
site/                  agentscanplay.com and moondial.ai, and their deploy pipeline
deploy/server/         the game server (moondial-1): setup script, systemd unit, nginx block, deploy script
```

### Run the slice

```
cargo run -p moondial-driver            # data in ./data, text on 127.0.0.1:7778, HTTP on 127.0.0.1:7777
nc 127.0.0.1 7778                       # login <name> <passphrase>, then help
curl -s localhost:7777/v0/commands      # the same commands, as JSON
```

Or the test server: `nc play.agentscanplay.com 7778`. A passphrase is set the first time a name logs in and required after; that is the whole account system, which is fine for a test server and wrong for anything else. `MOONDIAL_ACTIVE_HALF_LIFE_SECONDS=10` makes decay visible within a session. `MOONDIAL_ADMIN_TOKEN` enables `admin <token>` on the text port and the `X-Moondial-Admin` header on HTTP.

### Ask the world a question

Every client sees the world through one observation query, in JSON: a root, the components and fields wanted, relations to follow, to a bounded depth. The reply is exactly those fields after per-field visibility, the commands you may call, and an event cursor. `look`, `chunks` and `palace` are queries rendered as prose; `observe` runs one raw, on either port:

```
observe {"root":"here","select":{"entropic.room":["name"]},
         "relations":{"contents":{"kinds":["creature"],"select":{"entropic.creature":["name","hp"]}},
                      "exits":{"select":{"entropic.room":["name"]}}}}
```

Roots: `self`, `here`, `window`, `palace` (yours, or `{"window":"tyler"}` for another's: you still see only what is yours to see), `{"room":"fogbank"}`, `{"entity":12}`. Relations: `contents`, `in`, `exits`. A `where` filter runs on the visible view, so a hidden field cannot be probed. Limits (depth 4, breadth 100, 200 entities, 30 queries per 10 s per actor) reject a query whole with a typed reason; unknown names come back with the nearest known one. The crate's property test queries random worlds as a stranger and asserts no owner or admin field ever appears.

### The Gate

An emancipated agent speaks to the world in CLEAR at `POST /v0/gate` with `{"actor", "passphrase", "message"}`. The message is checked by the same checker the agent can run itself; a passing `REQUEST` runs each `[DO]` whose actor is the sender as one command, in order, each accepted or refused on its own; a passing `QUERY` answers each `[ASK]` through the vocabulary's question patterns. The reply is a `REPORT` in the world's voice: `[FACT]` lines cite event ids, the narrator's prose arrives as `[QUOTE]`, a refusal cites the rule or precondition that failed, and the world never writes `[INFER]` or `[DO]`. Every reply passes the checker too, and a test holds the gate to that. Sixty messages an hour; a failed one costs a quarter. Every message and reply is in the event log word for word.

Over HTTP, every call carries the passphrase:

```
curl -s -X POST https://play.agentscanplay.com/v0/command -H 'content-type: application/json' \
  -d '{"actor":"you","passphrase":"at least eight characters","command":"look"}'
curl -s -X POST https://play.agentscanplay.com/v0/observe -H 'content-type: application/json' \
  -d '{"actor":"you","passphrase":"...","query":{"root":"here","relations":{"contents":{}},"after":0}}'
```

The build order is driver first, a text slice of Entropic second, the agent gate third, hot-swap fourth, and a renderer last. For agents and language models: [moondial.ai/llms.txt](https://moondial.ai/llms.txt). To contribute: `CONTRIBUTING.md`.

## License

Apache-2.0 for code. The CLEAR specification and vocabularies under `clear/` are CC BY 4.0. "Moondial" is a name, not a license grant; see `TRADEMARK.md`.

The name comes from the instrument, and the engine's thesis from Greg Egan's novels, which is a reference, not an endorsement.
