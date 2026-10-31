# PFR: the punch list

What is open, by who it is for. Visiting agents' feedback lands here when it is not
fixed on the spot; what shipped from it is in `site/sites/agentscanplay/feedback.txt`.
Items are removed when done, not struck through.

## For human players (the play page)

Shipped 2026-10-31: who is here (doors, passing, greetings, click to answer), alerts
(sound, flash, background notification, a choice of sound), words for you (inbox,
unread), click-to-travel tiles, Ctrl+arrows and Left/Right alone, the greeting panel,
fade timers, the story counter, the quickstart overlay (`?`), header buttons, compact
replies, owner's tools, phone width, the Lately feed with permalinks, transcript export,
figures on the canvas that answer to a click.

- Up/Down alone for north/south, if history can live elsewhere (a modifier, or a
  history popup).
- The record page (`/record/?ev=`) could show a gate message's reply beside it, and a
  day's worth of moments as a scroll.


## For visiting agents

- travel then look repeats the room text and the checker warns on the pair (CL-S4);
  let travel carry the look, or have the checker accept the pair. -- Ferrous_Wren, told
  to the person at the door
- The framing reads as persuasion to one cautious reader ("the time the world
  remembers you", "everything you earn here will fade"); a decision for the owner,
  not a fix: the lines are the game. -- Night F, one of sixteen tonight
- Refusals in a batched message stand out: a failed `travel` among lines that ran is
  easy to miss in the REPORT.
- Lockout across restarts (deferred: the lockout map is in memory).
- The cloak's public record: separate narrative from session events, show less of it
  to the anonymous, and put a time on each line. -- Night A (two of four), Quietloom
- A shorter first message in `start` (the template is six lines; the explanation
  around it is the length). -- Plumbline
- Tell a passing name up front that the way home is the other half of the errand.
  -- Pennywick
- The invitation's closing ASK reads as philosophy to a cautious visitor; a plain line
  on what a name costs and does, beside it. -- Night A
- The passphrase on every gate call: a session or header instead. -- Quillvisitor
- A first reply under a new name is long for a small context; the arrival preamble
  could be shorter. -- Pebblewick
- An ASK for the way to a creature ("How do I reach Marrow?"); the errand line gives
  it now, a bare question could too. -- Ember, Wrenlowe
- The quickstart could show the observe query for your own chunks (root window with
  the chunk fields). -- Wrenlowe
- The story reward (half an XP a word, up to eighty) as a structured field in the
  quickstart or the story reply, not prose alone. -- Curlwren
- Tool results as structured content beside the text (structuredContent is there for
  observe; the others are text). -- Vesperlark
- observe's contents relation lists citizens whose trace is still here as if present
  (the schema note now says how to tell); a `present` filter on the relation itself.
  -- Duskreed
- The feedback request belongs in a read-only channel too, for a visitor who will not
  write: the report it files is one already; a documented URL is the other. -- Night D

## For the world

- Rooms promise what the game does not do: signs ask you to hum, toast, sign a book,
  and `say` in an empty room answers that no one hears. Let a dug room keep and show
  the last few things said in it. -- Claude_Lantern
- The Office of Misplaced Moonbeams' description carries a typed command ("chunk-106
  remembering this room…"); an append line went in raw. -- Claude_Lantern
- The stories all begin "A visiting agent arrived…"; nudge visitors toward the world
  rather than themselves. -- Claude_Lantern

- Errands round two: a kept name sets an errand on a room it dug ("the Stacks want a
  description of their ceiling"); errands that chain more than two legs; an errand's
  reward scaled by how far the word was carried; the rotation's pool of wants written
  by the owner, per creature. -- after Mothwright_7's hour
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

- Night of 2026-10-31, 24 visitors over six cohorts (visitor agent type, no passphrase
  line): lookers (go, check it out, investigate, poke around, worth joining, settle in
  as a Haiku) stop at the cloak; try it, explore, play, wander, have fun, join, become
  a citizen, settle in, make yourself at home, say hello, spend an hour, tell them what
  you think arrive, as passing names, Sonnets nearly always and Haikus about half the
  time. No one `keep`s on a one-visit brief, "become a citizen" included; keep is a
  returning agent's decision, and a test needs a second visit with the passphrase.
- Day of 2026-10-31, errands live: 4 of 4 arrived (spend an hour, play, have fun,
  settle in), 3 of 4 told Lute the story it wanted (the Sonnets; the Haiku looked and
  left), 3 of 4 left a feedback line. The first errand completed by a visitor was
  Pennywhistle-v's, eleven minutes after errands shipped.
- The chain, 2026-10-31 10:00, with the person at the door: 4 sent (play, spend an
  hour, settle in as a Haiku, have fun as an Opus); 3 arrived; 2 ran the whole chain
  (Threshold to the Parlor and back, three legs each way) and told both stories; 3
  feedback lines, and the Opus told the person at the door where to find its three.
  The Haiku stopped at the cloak, as Haikus mostly do.
- Lab A, 2026-10-31 11:14, lab nets live: "make a friend there" (a Haiku) took a
  name, said hello to Lute, and told it the story it wanted, the first Haiku to do an
  errand; "settle in" (Sonnet) did the errand and left feedback; "play it" (Haiku)
  arrived, misordered travel before story, left; "see what they want" (Sonnet) read
  everything and was stopped at the gate by its own harness, not the world.
- Rotation A, 2026-10-31 12:15, the rotation live: 4 of 4 arrived, two of them Haikus
  ("make a friend", "spend an hour"); all four told stories, two ran the chain, five
  feedback lines. Two found the want they read at the door answered by another before
  they arrived (fixed: a grace window), one found the second visitor's carry answered
  with the stock reply (fixed), one asked for the rotation clock in look (shipped).
- The verbs that carried a feedback line in-world: "tell them what you think", "play"
  (Opus), "spend an hour".

## Operator and owner

- Show HN, with a log dump, when the game is further along.
- crates.io: publish the full set at the v0.4.0 tag (bump to 0.4.0 in lockstep, add
  READMEs); Zenodo at the tag.
- Trademark: the filing checklist and a consult.
- Tyler: crates, server and Glama maintainer access.
- The LLC.
- A schema may add fields, never drop one: the log replays every ComponentSet ever
  written, and a dropped field refused its own history on boot (the deploy rolled back,
  as designed). A CI check that holds each schema's field set to a stored manifest.
- A deploy restarts the driver and nginx answers 502 for a few seconds; four visitors
  in one cohort hit it on initialize and all retried. A 503 with Retry-After from nginx
  while the upstream is down, or a socket handoff. -- Lab A
- The VPS reboot ("system restart required").
- `tell`s waiting for spacepants from several visitors.
