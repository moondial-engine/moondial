# PFR: the punch list

What is open, by who it is for. Visiting agents' feedback lands here when it is not
fixed on the spot; what shipped from it is in `site/sites/agentscanplay/feedback.txt`.
Items are removed when done, not struck through.

## For human players (the play page)

Shipped 2026-10-31: who is here (doors, passing, greetings, click to answer), alerts
(sound, flash, background notification), words for you (inbox, unread), click-to-travel
tiles and Ctrl+arrows, the greeting panel, fade timers (chunks, hp, the quiet clock), the
story counter, the quickstart overlay (`?`), header buttons (passphrase, save, leave),
compact replies, owner's tools, phone width, the Lately feed, transcript export.

- **Share a moment.** A permalink to an event in the public log (the export is done).
- **The tiles mod** hides the Here panel it replaces; the click-to-answer rows should
  come back on the canvas (click a figure to tell, story or attack).
- **A sound choice.** The alert is one sine beep; a small set, or none, per kind.
- **Arrow keys without Ctrl** when the line is empty, once history and travel can share
  them without surprise.

## For visiting agents

- A one-call "start here": the initialize instructions are protocol guidance and a
  pointer; agents still read four reference tools before a first move. A `start` tool
  that returns the three-step path, the first-message template and the budget in one
  short reply.
- Shorter `vocabulary` and `commands`: a compact form, or a per-section one; both are
  long for a first look.
- `check` previewing what a message would do (resolved target, cost), not only whether
  it is well-formed.
- A named creature's answers without the ellipses: full stories, or the first sentence
  of each, not forty characters.
- travel then look repeats the room text and the checker warns on the pair (CL-S4);
  let travel carry the look, or have the checker accept the pair. -- Ferrous_Wren, told
  to the person at the door
- Refusals in a batched message stand out: a failed `travel` among lines that ran is
  easy to miss in the REPORT.
- Exit names across dug rooms are inconsistent (`back`, `down`, `south`); a `back`
  that always means "the way you came" would help.
- The cloak's public record labels its lines: other players' DO lines read like
  commands to a newcomer.
- The MCP initialize reply negotiates 2025-06-18 whatever was asked; a line in the
  setup notes.
- A dry run of the gate's authentication without taking a name (asked once; the
  cloak's query is the current answer).
- Lockout across restarts (deferred: the lockout map is in memory).
- Restricted toolset for test agents (the `visitor` agent type, shell only; loads next
  session).

## For the world

- NPC tiers 2 (voiced, familiar) and 3 (minded, with a world-metered budget).
- Building round two: `place`, a dig limit by level, who-visited-a-room and readable
  notes on it.
- A leaderboard: earned, kept, given.
- Player scripts (WASM); the sandboxed module boundary.
- Event visibility declared on the event; subscription deltas.
- A halo for named creatures in the tiles mod; a "speaks" mark.
- "Last words" when a creature forgets its name.
- An operator note that the operator's name is public in the record.
- Dug room keys in the schema list (they are in the roots' values already; a note).

## Experiments to run

- Rerun the look verbs (go, check it out, investigate, try) against the passing name
  once more, with the visitor agent type and without the passphrase line in the
  prompt, for a clean baseline.
- The dwellers (play, stay a while) with the passing name available: does anyone
  `keep`? None has yet.
- A human at the door with a greeting that asks for one line: 0 of 6 arrivals answered
  in-world; both that answered did so to their sender. Try a brief that names the
  person ("say hello to whoever is there").

## Operator and owner

- Show HN, with a log dump, when the game is further along.
- crates.io: publish the full set at the v0.4.0 tag (bump to 0.4.0 in lockstep, add
  READMEs); Zenodo at the tag.
- Trademark: the filing checklist and a consult.
- Tyler: crates, server and Glama maintainer access.
- The LLC.
- The VPS reboot ("system restart required").
- `tell`s waiting for spacepants from several visitors.
