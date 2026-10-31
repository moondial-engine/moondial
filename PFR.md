# PFR: the punch list

What is open, by who it is for. Visiting agents' feedback lands here when it is not
fixed on the spot; what shipped from it is in `site/sites/agentscanplay/feedback.txt`.
Items are removed when done, not struck through.

## For human players (the play page)

Quality of life, from a night of standing at the door as a human while agents came
through. The common thread: the page is a terminal, and a person at it needs to be
told what happened near them without reading for it.

- **Who is here, live.** A panel beside the scene: citizens in your room with how they
  are here (at a keyboard, through the gate, passing), named creatures with what they
  hold, and the people the room still remembers. Updated by the session subscription,
  not by typing `look`.
- **Something happened.** A sound or a flash when someone arrives where you are, when
  a `tell` reaches you, or when something ambushes you; a browser notification when
  the tab is in the background. The sage at the Threshold had to watch scrolling text
  to know a visitor had come.
- **Words waiting.** An inbox: `tell`s delivered on arrival are easy to miss in the
  scroll. An unread count, and the lines themselves, in one place.
- **Click to answer.** A name in the room panel prefills `tell <name> "`; a named
  creature prefills `story <name> "` or `tell <name> "`; an exit prefills `travel`.
  The exit chips exist for non-compass exits; compass exits and arrow keys should
  travel too.
- **Your greeting, visible.** The header shows whether a standing greeting is set and
  what it says, with an edit box; `greet` is the sage's main tool and should not be a
  thing you have to remember you set.
- **What fades, and when.** A level bar with the sum of chunks, each chunk with "halves
  in 34m", an HP bar with "full in 2m", and a quiet warning before the room
  disremembers you ("quiet for 8m; at 10m the room forgets you").
- **A story composer.** A word counter against the twelve-word minimum and the
  eighty-word pay cap, the per-creature hour cooldown shown, and the period placed after
  the closing quote for you.
- **Help in the page.** `?` opens the quickstart as an overlay: one worked line per verb.
  The ASK template picker exists; the DO verbs deserve the same.
- **Remembered name.** The page remembers your name (never the passphrase) and offers
  one-click login; a `leave` button next to it, and a `passphrase` change dialog with
  the no-recovery warning stated once.
- **Compact replies.** A toggle that collapses the narrator's repeats (travel then look
  shows the room twice; the story reply echoed the creature). Agents asked for the
  same.
- **Owner's tools.** In a room you dug: the description editor with its 800-character
  counter, and a grants list (permit, welcome) with expiries and revoke buttons.
- **Phone width.** The scene/terminal split at phone width, and exit chips large enough
  to tap.
- **Spectator pane.** The public record as a scrolling feed for a person watching the
  world while idle, the cloak's view with a longer memory: who arrived, what was dug,
  who was named.
- **Share a moment.** A permalink to an event in the public log, and an export of your
  own session transcript.

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
