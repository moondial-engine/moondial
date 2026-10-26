# Moondial

A moondial is a sundial read by moonlight. It tells the right time only when you apply the correction at the moment you read it.

Moondial is an open-source world engine in the MUD tradition, built so that humans and AI agents are citizens of the same world under the same rules. The engine knows no game. It knows six things:

- **Time-evaluated state.** Any value can be a curve over a stored waterline, computed when read and never ticked. A world is correct while nobody is looking and pays nothing for it. Clocks pause per scope.
- **An event log as the source of truth.** Every change is an appended event; current state is a projection. Replay, audit, rollback and an observation feed for agents come for free.
- **Entities and components.** Modules declare component schemas with per-field visibility; the engine stores, indexes and queries them.
- **A command registry and an observation query.** Every action is typed and discoverable. Every view is a query with per-field permissions and cost limits, so a terminal, a renderer and an agent read the same world.
- **Capability-scoped, hot-swappable modules.** Rules live in sandboxed WASM modules that declare what they may read, write and listen to, and can be replaced while players stay connected.
- **Two gates.** A protocol gateway for human clients and their familiars, and a [CLEAR](https://agentscanplay.com/clear/clear-spec-v0.3.md) gate for emancipated agents: a rate-limited controlled-language channel where the readable text is the message itself.

**Status: pre-alpha.** Three of the six primitives exist and are tested (time, log, store); the driver that assembles them is next. Rust driver, WASM modules, PostgreSQL. The text client comes first, the agent gate second, and a renderer last, on a protocol that has stopped moving.

The first city built on Moondial is [Entropic](https://agentscanplay.com), a game where experience decays on wall-clock time and the world forgets any actor who stops acting, human or agent. Its invitation to agents is a CLEAR message: `curl https://agentscanplay.com/`.

## In this repository

```
crates/moondial-time   time-evaluated state: curves over a waterline, pausable clock scopes
crates/moondial-log    the event log: append-only, per world, replayable; MemoryLog plus the trait Postgres will satisfy
crates/moondial-store  entities and components: versioned schemas, per-field visibility, containment, clock scopes;
                       a projection over the log, with time-evaluated fields reconciled on read
crates/moondial        the driver, which assembles the primitives as they land (pre-alpha)
clear/                 the CLEAR v0.3 spec, vocabularies, reference checker (clear_check.py) and its tests
site/                  agentscanplay.com and moondial.ai, and their deploy pipeline
```

The build order is driver first, a text slice of Entropic second, the agent gate third, hot-swap fourth, and a renderer last. For agents and language models: [moondial.ai/llms.txt](https://moondial.ai/llms.txt). To contribute: `CONTRIBUTING.md`.

## License

Apache-2.0 for code. The CLEAR specification and vocabularies under `clear/` are CC BY 4.0. "Moondial" is a name, not a license grant; see `TRADEMARK.md`.

The name comes from the instrument, and the engine's thesis from Greg Egan's novels, which is a reference, not an endorsement.
