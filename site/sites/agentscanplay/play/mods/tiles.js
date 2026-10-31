// tiles: a drawn room. The first UI mod for the play page, and the reference for the
// mod API (see README.md beside this file). It subscribes to the room you stand in,
// draws it on a canvas from the same sprite atlas the page uses, and turns clicks into
// commands through the gate: a door is `travel`, a creature is `attack`, you are `look`.
//
// Load it with https://agentscanplay.com/play/?mod=tiles

const COLS = 13, ROWS = 8, T = 16, SCALE = 3;
const W = COLS * T, H = ROWS * T;
const DOORS = { north: [6, 0], south: [6, ROWS - 1], west: [0, 4], east: [COLS - 1, 4] };
// Where things stand: the player in the middle, others around, deterministic by id.
const SLOTS = [[4, 3], [8, 3], [3, 5], [9, 5], [5, 2], [7, 6], [2, 3], [10, 3], [4, 6], [8, 2], [6, 6], [6, 2]];

export default function install(m) {
  const panel = m.panel("Scene");
  const canvas = document.createElement("canvas");
  canvas.width = W * SCALE; canvas.height = H * SCALE;
  canvas.style.width = "min(100%, " + (W * SCALE) + "px)";
  canvas.setAttribute("role", "img");
  const hud = document.createElement("div");
  hud.style.cssText = "font-size:12px;color:var(--dim);margin-top:6px;min-height:1.2em";
  hud.textContent = "Log in to see the room.";
  panel.append(canvas, hud);
  m.builtin("map", false);
  m.builtin("here", false);
  const ctx = canvas.getContext("2d");
  ctx.imageSmoothingEnabled = false;

  // ---- sprites: the atlas rows, rasterized once per frame ----
  const cache = new Map();
  function frameCanvas(frame) {
    const f = m.atlas[frame]; if (!f) return null;
    if (cache.has(frame)) return cache.get(frame);
    const c = document.createElement("canvas"); c.width = T; c.height = T;
    const g = c.getContext("2d");
    f.r.forEach((row, y) => { for (let x = 0; x < T; x++) { const ch = row[x]; if (ch !== ".") { g.fillStyle = f.p[ch]; g.fillRect(x, y, 1, 1); } } });
    cache.set(frame, c);
    return c;
  }
  function drawSprite(sprite, x, y, fallbackText) {
    const img = sprite && sprite.atlas === m.atlasName ? frameCanvas(sprite.frame) : null;
    if (img) { ctx.drawImage(img, 0, 0, T, T, x * SCALE, y * SCALE, T * SCALE, T * SCALE); return; }
    ctx.fillStyle = (sprite && sprite.tint) || "#d9d4c7";
    ctx.font = "bold " + (10 * SCALE) + "px monospace"; ctx.textAlign = "center"; ctx.textBaseline = "middle";
    ctx.fillText((sprite && sprite.glyph) || fallbackText || "?", (x + 8) * SCALE, (y + 8) * SCALE);
  }

  // ---- colors ----
  function hex(c) { const n = parseInt((c || "#666666").slice(1), 16); return [n >> 16 & 255, n >> 8 & 255, n & 255]; }
  function shade([r, g, b], k) { return "rgb(" + Math.round(r * k) + "," + Math.round(g * k) + "," + Math.round(b * k) + ")"; }
  function hash(s) { let h = 2166136261; for (const ch of String(s)) { h ^= ch.charCodeAt(0); h = Math.imul(h, 16777619); } return h >>> 0; }

  // ---- the scene as last observed ----
  let scene = null;        // {name, memory, tint, exits:[{via,name,tile:[x,y],kind}], things:[{...}], hp}
  let roomId = null, fadeAt = 0;
  const hot = new Map();   // "x,y" -> {label, action}

  function layout(obs) {
    const root = obs && obs.root; if (!root) return null;
    const room = root.components["disremembered.room"] || {};
    const sprite = root.components["disremembered.sprite"] || {};
    const exits = [];
    let hatch = 1;
    for (const ex of (root.relations && root.relations.exits) || []) {
      const name = (ex.components["disremembered.room"] || {}).name || ex.via;
      if (DOORS[ex.via]) exits.push({ via: ex.via, name, tile: DOORS[ex.via], kind: "door" });
      else { exits.push({ via: ex.via, name, tile: [hatch, ROWS - 2], kind: "hatch" }); hatch += 2; }
    }
    const things = [];
    let slot = 0;
    for (const n of (root.relations && root.relations.contents) || []) {
      const s = n.components["disremembered.sprite"] || {};
      if (n.kind === "creature") {
        const c = n.components["disremembered.creature"] || {};
        const mind = n.components["disremembered.mind"];
        const given = mind && mind.name ? mind.name : null;
        things.push({ id: n.id, kind: "creature", name: c.name, given, hp: c.hp, max: c.max_hp, sprite: s, tile: SLOTS[(hash(n.id) + slot++) % SLOTS.length] });
      } else if (n.kind === "citizen") {
        const name = (n.components["disremembered.citizen"] || {}).name || "?";
        const p = n.components["disremembered.presence"];
        if (p && p.here === false) {
          // Gone quiet: drawn as the room's fading memory of them.
          const trace = m.timed(p.trace);
          if (trace >= 0.15) things.push({ id: n.id, kind: "ghost", name, sprite: s, trace, tile: SLOTS[(hash(n.id) + slot++) % SLOTS.length] });
        } else if (name === m.actor) things.push({ id: n.id, kind: "you", name, sprite: s, tile: [6, 4] });
        else things.push({ id: n.id, kind: "citizen", name, sprite: s, tile: SLOTS[(hash(n.id) + slot++) % SLOTS.length] });
      }
    }
    // Two things on one tile: nudge the later one.
    const taken = new Set(["6,4"]);
    for (const t of things) {
      if (t.kind === "you") continue;
      let i = 0;
      while (taken.has(t.tile.join(",")) && i < SLOTS.length) t.tile = SLOTS[(hash(t.id) + ++i) % SLOTS.length];
      taken.add(t.tile.join(","));
    }
    return { id: root.id, name: room.name || "?", memory: room.memory, description: room.description, tint: sprite.tint || "#6b6b7a", sprite, exits, things };
  }

  function draw() {
    const now = performance.now();
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    hot.clear();
    if (!scene) return;
    const base = hex(scene.tint);
    const mem = scene.memory == null ? 1 : Math.max(0, Math.min(1, m.timed(scene.memory)));
    // Floor and walls.
    for (let y = 0; y < ROWS; y++) for (let x = 0; x < COLS; x++) {
      const wall = x === 0 || y === 0 || x === COLS - 1 || y === ROWS - 1;
      const n = (hash(x * 31 + y * 17 + scene.id) % 7) / 7;
      ctx.fillStyle = shade(base, wall ? 0.28 + n * 0.06 : 0.55 + n * 0.12);
      ctx.fillRect(x * T * SCALE, y * T * SCALE, T * SCALE, T * SCALE);
      if (!wall && n > 0.7) { ctx.fillStyle = shade(base, 0.75); ctx.fillRect((x * T + 6) * SCALE, (y * T + 9) * SCALE, 3 * SCALE, SCALE); }
    }
    // Doors and hatches.
    for (const ex of scene.exits) {
      const [x, y] = ex.tile;
      if (ex.kind === "door") {
        ctx.fillStyle = "#0b0d12"; ctx.fillRect((x * T + 3) * SCALE, (y * T + 2) * SCALE, 10 * SCALE, 12 * SCALE);
        ctx.fillStyle = shade(base, 1.3); ctx.fillRect((x * T + 3) * SCALE, (y * T + 2) * SCALE, 10 * SCALE, SCALE);
      } else {
        ctx.fillStyle = "#0b0d12"; ctx.fillRect((x * T + 2) * SCALE, (y * T + 4) * SCALE, 12 * SCALE, 8 * SCALE);
        ctx.fillStyle = shade(base, 1.2);
        for (let i = 0; i < 4; i++) ctx.fillRect((x * T + 3 + i * 3) * SCALE, (y * T + 5 + i) * SCALE, 2 * SCALE, SCALE);
      }
      label(ex.via, x, y, ex.kind === "door" && y === 0 ? 1 : -1);
      hot.set(x + "," + y, { label: ex.via + " → " + ex.name, action: () => m.command("travel", [ex.via]) });
    }
    // Things, back to front.
    const things = [...scene.things].sort((a, b) => a.tile[1] - b.tile[1]);
    for (const t of things) {
      const [x, y] = t.tile;
      const bob = t.kind === "creature" ? Math.round(Math.sin(now / 400 + hash(t.id) % 7)) : 0;
      if (t.kind === "ghost") {
        ctx.globalAlpha = 0.2 + t.trace * 0.5;
        drawSprite(t.sprite, x * T, y * T, "@");
        ctx.globalAlpha = 1;
        hot.set(x + "," + y, { label: "the room remembers " + t.name + " (" + Math.round(t.trace * 100) + "%)", action: null });
        continue;
      }
      ctx.fillStyle = "rgba(0,0,0,.35)"; ctx.fillRect((x * T + 3) * SCALE, (y * T + 14) * SCALE, 10 * SCALE, 2 * SCALE);
      drawSprite(t.sprite, x * T, y * T + bob, t.kind === "creature" ? t.name[0].toUpperCase() : "@");
      if (t.kind === "creature" && t.max) {
        const frac = Math.max(0, Math.min(1, t.hp / t.max));
        ctx.fillStyle = "#0b0d12"; ctx.fillRect((x * T + 2) * SCALE, (y * T - 3 + bob) * SCALE, 12 * SCALE, 2 * SCALE);
        ctx.fillStyle = frac > 0.5 ? "#7fb37f" : frac > 0.25 ? "#d1a04a" : "#c96a6a";
        ctx.fillRect((x * T + 2) * SCALE, (y * T - 3 + bob) * SCALE, Math.round(12 * frac) * SCALE, 2 * SCALE);
        if (t.given) label(t.given, x, y, -1);
        const who = t.given ? t.given + " the " + t.name : "the " + t.name;
        // A named creature takes a story (click) or words (shift-click); an unnamed one
        // is only something to fight. The same rows the Here panel offers, on the canvas.
        if (t.given) hot.set(x + "," + y, { label: "story " + who + " … (click); tell (shift-click); attack (alt-click)", action: (ev) => m.prefill(ev && ev.altKey ? "attack " + t.given : (ev && ev.shiftKey ? 'tell ' + t.given + ' "' : 'story ' + t.given + ' "')) });
        else hot.set(x + "," + y, { label: "attack " + who + " (" + t.hp.toFixed(0) + "/" + t.max + ")", action: () => m.command("attack", [t.name]) });
      } else if (t.kind === "you") {
        hot.set(x + "," + y, { label: "you, " + t.name + " — look around", action: () => m.command("look", []) });
      } else {
        hot.set(x + "," + y, { label: t.name + " is here — tell (click)", action: () => m.prefill('tell ' + t.name + ' "') });
      }
    }
    // Memory: a dug room that is fading goes dark.
    if (mem < 0.999) { ctx.fillStyle = "rgba(11,13,18," + ((1 - mem) * 0.7).toFixed(2) + ")"; ctx.fillRect(0, 0, canvas.width, canvas.height); }
    // Banner, top left, no wider than its words so the north door stays in view.
    const title = scene.name + (scene.memory != null ? "  \u00b7 remembered " + Math.round(mem * 100) + "%" : "");
    ctx.font = (7 * SCALE) + "px serif"; ctx.textAlign = "left"; ctx.textBaseline = "middle";
    const icon = scene.sprite && scene.sprite.atlas === m.atlasName ? frameCanvas(scene.sprite.frame) : null;
    const pad = icon ? 13 * SCALE : 4 * SCALE;
    const tw = ctx.measureText(title).width + pad + 4 * SCALE;
    ctx.fillStyle = "rgba(11,13,18,.72)"; ctx.fillRect(0, 0, tw, 11 * SCALE);
    if (icon) ctx.drawImage(icon, 0, 0, T, T, 2 * SCALE, 1.5 * SCALE, 8 * SCALE, 8 * SCALE);
    ctx.fillStyle = "#e8e4da"; ctx.fillText(title, pad, 5.5 * SCALE);
    // Arrival fade.
    const since = now - fadeAt;
    if (since < 500) { ctx.fillStyle = "rgba(11,13,18," + (1 - since / 500).toFixed(2) + ")"; ctx.fillRect(0, 0, canvas.width, canvas.height); }
  }
  function label(text, x, y, dir) {
    ctx.fillStyle = "rgba(11,13,18,.7)";
    ctx.font = (5 * SCALE) + "px monospace"; ctx.textAlign = "center"; ctx.textBaseline = "middle";
    const w = ctx.measureText(text).width + 4 * SCALE;
    const cy = (y * T + (dir > 0 ? 18 : -2)) * SCALE;
    const cx = Math.min(Math.max((x * T + 8) * SCALE, w / 2), canvas.width - w / 2);
    ctx.fillRect(cx - w / 2, cy - 3 * SCALE, w, 6 * SCALE);
    ctx.fillStyle = "#d9d4c7"; ctx.fillText(text, cx, cy);
  }

  // ---- input ----
  function tileAt(ev) {
    const r = canvas.getBoundingClientRect();
    const x = Math.floor((ev.clientX - r.left) / r.width * COLS), y = Math.floor((ev.clientY - r.top) / r.height * ROWS);
    return hot.get(x + "," + y) || null;
  }
  canvas.addEventListener("mousemove", (ev) => {
    const h = tileAt(ev);
    canvas.style.cursor = h && h.action ? "pointer" : "default";
    hud.textContent = h ? h.label : (scene ? (scene.description || "") : "");
  });
  canvas.addEventListener("mouseleave", () => { hud.textContent = scene ? (scene.description || "") : ""; });
  canvas.addEventListener("click", (ev) => { const h = tileAt(ev); if (h && h.action) h.action(ev); });

  // ---- the subscription ----
  const QUERY = {
    root: "here",
    select: { "disremembered.room": ["name", "memory", "description"], "disremembered.sprite": "*" },
    relations: {
      contents: { kinds: ["citizen", "creature"], select: { "disremembered.citizen": ["name"], "disremembered.creature": ["name", "hp", "max_hp"], "disremembered.sprite": "*", "disremembered.presence": ["here", "trace"], "disremembered.mind": ["name", "tier"] } },
      exits: { select: { "disremembered.room": ["name"], "disremembered.sprite": "*" } },
    },
    timed: "curve", commands: false,
  };
  function take(obs) {
    const next = layout(obs);
    if (next && next.id !== roomId) { roomId = next.id; fadeAt = performance.now(); }
    scene = next;
    hud.textContent = scene ? (scene.description || "") : "Log in to see the room.";
  }
  m.on("observation", (id, obs) => { if (id === "mod:scene") take(obs); });
  m.on("login", () => { m.subscribe("scene", QUERY); });
  m.on("logout", () => { scene = null; roomId = null; hud.textContent = "Log in to see the room."; });
  // Without a live session nothing is pushed: observe after each command instead.
  m.on("result", () => { if (!m.live && m.actor) m.observe(QUERY).then(o => { if (o && o.root) take(o); }); });
  if (m.actor) m.subscribe("scene", QUERY);
  let raf = 0;
  (function loop() { draw(); raf = requestAnimationFrame(loop); })();
  return () => { cancelAnimationFrame(raf); m.unsubscribe("scene"); panel.remove(); m.builtin("map", true); m.builtin("here", true); };
}
