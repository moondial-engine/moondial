# UI mods for the play page

The play page at `/play/` is one renderer among many. A mod is an ES module that gets
the page's session and draws, or re-draws, whatever it likes; the game itself never
changes, because every action still goes through the same gate an agent uses. To load one:

- The **mod** picker in the page header lists the mods in `mods/index.json`; the choice is
  remembered in the browser and mirrored into the URL (`/play/?mod=tiles`), so a link carries it.
- `/play/?mod=https://example.com/my-mod.js`, or "from a URL…" in the picker, loads from
  anywhere, after a confirmation.
- Picking "none" unloads the mod: its `install` may return a function that undoes what it did.

To list a mod here, add `{"id", "name", "blurb"}` to `mods/index.json` and put the module at
`mods/<id>.js`.

A mod runs on the page with your session. It never sees your passphrase, but it can act
as you, so load only mods you trust.

## The module

```js
export default function install(moondial) {
  // set up; return a function to undo, or nothing
}
```

## `window.moondial` (protocol `mod/0`)

| Member | What it is |
| --- | --- |
| `version` | `"mod/0"` |
| `api` | The driver's base URL |
| `actor` | Your name once logged in, else `null` |
| `live` | `true` when the WebSocket session is open; otherwise the page polls |
| `observe(query)` | One observation; resolves to the observation or `{type: "error", error}` |
| `command(verb, args)` | A command through the gate, printed to the transcript like a typed one; resolves to the result (`ok`, `summary`, `error`, and with show CLEAR on, `message` and `reply`) |
| `subscribe(id, query)` | A live subscription; the page re-sends it on reconnect. Five per mod. Observations arrive on `on("observation")` with id `"mod:" + id` and sit in `observations["mod:" + id]` |
| `unsubscribe(id)` | Drops it |
| `observations` | The page's own last observations too: `room`, `me`, `window` |
| `on(event, fn)` / `off` | Events: `login (name, live)`, `logout ()`, `observation (id, obs)`, `event (e)` for every pushed world event you may see, `result (verb, args, body)` after each command |
| `atlas`, `atlasName`, `spriteSVG(frame)` | The pixel atlas the page draws with: 16×16 frames as rows of palette keys, and an SVG element for one |
| `timed(t)` | Evaluates a timed value (a curve from the observation) at this moment, clock skew corrected |
| `panel(title, {wide, at})` | A section in the scene area, full width by default, prepended (or `at: "end"`) |
| `builtin(name, visible)` | Show or hide the page's own `map`, `here`, `you`, `window` panels |
| `print(text, cls)` | A line in the transcript (`sys`, `err`, `tell`, `clear`) |

Queries are the observe queries the driver documents at `/v0/schema` and in the Observe
tab: `root` (`here`, `self`, `window`, `palace`, or `{entity: id}`), `select` per
component, `relations` (`contents`, `exits`), `timed: "curve"` to get curves instead of
numbers. Visibility is applied before you see anything: a stranger's owner-only fields
are absent, not hidden.

## `tiles.js`

The reference mod. It subscribes to the room you stand in and draws it on a canvas:
floor in the room's tint, doors on the compass sides, other exits as hatches, creatures
with health bars, citizens, you in the middle, and a fading room going dark as its
memory fades. A door click is `travel`, a creature click is `attack`, a click on you is
`look`. About 200 lines; copy it to start your own.
