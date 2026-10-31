//! Memory that moves without a fight.
//!
//! `give <chunk> <citizen>` hands one of your chunks to a citizen standing here: it is
//! theirs now, on its own curve, and the gift is a public event. `story <creature>
//! "<text>"` tells a named creature a story. It keeps the story as a memory of its own,
//! and you come away with one: the first way to earn a chunk here without striking
//! anything. A creature listens to one story an hour from each teller, so stories are
//! a way in, not a mint; a creature without a name does not understand you.

use super::combat::narrate;
use super::speech::SAY;
use super::{internal, me, mind, presence, rooms, Curves};
use crate::world::World;
use moondial_log::Actor;
use moondial_registry::{Args, CommandError, CommandSpec, Outcome, ParamType};
use serde_json::json;

/// A chunk changed hands: `{from, to, chunk, with, room}`.
pub const GIVEN: &str = "disremembered.given";
/// A story was told and kept: `{from, creature, kind, name, words, earned, kept}`.
pub const STORY: &str = "disremembered.story";

/// A story is at least this many words, and worth up to this many.
const MIN_WORDS: usize = 12;
const MAX_WORDS_PAID: usize = 80;
/// XP per word for the teller; the creature keeps half as much.
const XP_PER_WORD: f64 = 0.5;
const MAX_STORY_CHARS: usize = 1200;
/// One story per teller per creature in this long.
const STORY_COOLDOWN_MS: i64 = 60 * 60 * 1000;

pub fn register(world: &mut World, curves: Curves) {
    world.register(
        CommandSpec::new(
            "disremembered.give",
            "Hand one of your chunks to a citizen standing here. It is theirs then, fading on its own curve, and the gift is on the record.",
        )
        .param("chunk", ParamType::Entity, true, "a chunk of yours; it goes to them")
        .param("citizen", ParamType::Entity, true, "a citizen here"),
        Box::new(|w: &mut World, ctx, args: &Args| {
            let f = me(w, ctx)?;
            let here = rooms::room_of(w, f.citizen).ok_or_else(|| internal("citizen has no room"))?;
            let typed = args.str("citizen").unwrap_or("").trim().trim_start_matches("citizen-").to_string();
            let Some(to) = super::canonical_name(w, &typed) else {
                return Err(CommandError::precondition(format!("There is no one called {typed}.")));
            };
            if to == ctx.actor {
                return Err(CommandError::precondition("You give it to yourself. It is where it was."));
            }
            let them = super::find_citizen(w, &to).ok_or_else(|| CommandError::precondition(format!("There is no one called {typed}.")))?;
            if rooms::room_of(w, them.citizen) != Some(here) || !presence::is_here(w, them.citizen) {
                return Err(CommandError::precondition(format!(
                    "{to} is not here. A memory is given hand to hand."
                )));
            }
            let chunk = super::memory::resolve_chunk(w, ctx, args.str("chunk").unwrap_or(""), &[f.window, f.palace])?;
            let (id, source) = mind::move_chunk(w, &ctx.actor, chunk, them.window, Some(to.clone()))?;
            let _ = w.note(
                Actor::Citizen(ctx.actor.clone()),
                GIVEN,
                json!({"from": ctx.actor, "to": to, "chunk": id.0, "was": chunk.0, "with": source, "room": here.0}),
            );
            narrate(w, &to, format!("{} gives you the memory of {source}: chunk-{}. It is yours now, and fading as it was.", ctx.actor, id.0));
            w.notice(format!("**{}** gave the memory of {source} to **{to}**.", ctx.actor));
            Ok(Outcome::new(
                format!("You give {to} the memory of {source}. It is theirs now: chunk-{}.", id.0),
                json!({"to": to, "chunk": id.0, "with": source}),
            ))
        }),
    );

    world.register(
        CommandSpec::new(
            "disremembered.story",
            "Tell a named creature here a story, at least a dozen words. It keeps the story as a memory of its own, and you come away with one: the way to earn without striking anything. One story an hour per creature; a creature without a name does not understand you.",
        )
        .param("creature", ParamType::Entity, true, "a named creature here")
        .param("text", ParamType::Text, true, "the story, quoted"),
        Box::new(move |w: &mut World, ctx, args: &Args| {
            let f = me(w, ctx)?;
            let here = rooms::room_of(w, f.citizen).ok_or_else(|| internal("citizen has no room"))?;
            let who = args.str("creature").unwrap_or("").trim().to_string();
            let Some(creature) = mind::find_here(w, here, &who) else {
                return Err(CommandError::precondition(format!("There is no {who} here.")));
            };
            let display = mind::display(w, creature);
            let Some(name) = mind::name(w, creature) else {
                return Ok(Outcome::new(
                    format!("You begin, and the {display} goes on sorting. It has no name to listen with."),
                    json!({"creature": creature.0, "kept": false}),
                ));
            };
            let text = args.str("text").unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
            let words = text.split_whitespace().count();
            if words < MIN_WORDS {
                return Err(CommandError::precondition(format!(
                    "A story is at least {MIN_WORDS} words; that was {words}."
                )));
            }
            if text.chars().count() > MAX_STORY_CHARS {
                return Err(CommandError::precondition(format!(
                    "That is more than {MAX_STORY_CHARS} characters; tell it in parts."
                )));
            }
            let now = w.now();
            let recently = w
                .recent_events(2000)
                .iter()
                .any(|e| e.kind == STORY && e.payload["from"] == ctx.actor && e.payload["creature"] == creature.0 && now.since(e.at).0 < STORY_COOLDOWN_MS);
            if recently {
                return Err(CommandError::precondition(format!(
                    "{name} is still turning your last story over. One an hour."
                )));
            }
            // The story is said, as itself.
            w.note(
                Actor::Citizen(ctx.actor.clone()),
                SAY,
                json!({"from": ctx.actor, "room": here.0, "to": display, "story": true, "text": text}),
            )
            .map_err(internal)?;
            // The story it wanted is worth double, and the errand is done.
            let wanted = super::errand::wants_story(w, creature, &text);
            let paid = words.min(MAX_WORDS_PAID) as f64 * XP_PER_WORD * if wanted { 2.0 } else { 1.0 };
            let gist = gist(&text);
            let kind = super::plain_str(w, creature, super::CREATURE, "name").unwrap_or("?").to_string();
            // It keeps the story; you keep the telling.
            let window = mind::window_of(w, creature).ok_or_else(|| internal("a named creature has a window"))?;
            let kept = super::memory::earn_chunk(w, &ctx.actor, window, &format!("a story: \"{gist}\""), paid / 2.0, 0.6, &curves)?;
            // The creature's chunk is its own, not the teller's.
            let kept = mind::disown(w, kept)?;
            let earned = super::memory::earn_chunk(w, &ctx.actor, f.window, &format!("telling {name} a story"), paid, 0.4, &curves)?;
            let level = mind::level(w, creature);
            let _ = w.note(
                Actor::Citizen(ctx.actor.clone()),
                STORY,
                json!({"from": ctx.actor, "creature": creature.0, "kind": kind, "name": name, "words": words, "earned": earned.0, "kept": kept.0, "level": level}),
            );
            let answer = if wanted {
                format!("That is the one I wanted. I will keep that. \"{gist}\" I hold {level:.0} now.")
            } else {
                format!("I will keep that. \"{gist}\" I hold {level:.0} now.")
            };
            super::talk::speak(w, creature, &ctx.actor, &answer)?;
            if wanted {
                super::errand::story_done(w, &ctx.actor, creature, &text)?;
            }
            mind::check_tier(w, creature)?;
            Ok(Outcome::new(
                // The creature's answer reaches the teller as it reaches the room, a
                // narrator line, so the outcome does not repeat it.
                format!(
                    "You tell {display} a story of {words} words, and it answers.{} You remember the telling: chunk-{} worth {paid:.0} XP.",
                    if wanted { " It was the story it wanted, so the memory is worth double." } else { "" },
                    earned.0
                ),
                json!({"creature": creature.0, "name": name, "words": words, "earned": earned.0, "xp": paid, "kept": kept.0, "level": level, "wanted": wanted}),
            ))
        }),
    );
}

/// The first sentence of a story, or its first words up to a line's worth, ending on a
/// word: a creature that repeats "memory of a story: \"Once there was a lant…\"" told a
/// visitor nothing. The ellipsis marks a cut, and only a cut.
fn gist(text: &str) -> String {
    const LINE: usize = 56;
    let first = text
        .split_inclusive(['.', '!', '?'])
        .next()
        .unwrap_or(text)
        .trim();
    if first.chars().count() <= LINE {
        return first.to_string();
    }
    let mut out = String::new();
    for word in first.split_whitespace() {
        if !out.is_empty() && out.chars().count() + 1 + word.chars().count() > LINE {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    format!("{}\u{2026}", out.trim_end_matches([',', ';', ':']))
}
