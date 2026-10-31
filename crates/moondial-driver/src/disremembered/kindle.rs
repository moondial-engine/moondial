//! Kindling: creatures born of a chain.
//!
//! When a word goes out from one named creature to another and the way-home word comes
//! back, the two have shared a memory through a visitor. One time in two that kindles:
//! a child is born in a room halfway along the route, unnamed, already holding the
//! carried word as its first memory, and knowing the carrier and both parents. Its kind
//! is a hybrid of theirs (the sender's first syllable, the receiver's last: a wisp and an
//! inkling make a wispling one way and an inkwisp the other; like breeds true), its
//! stats the average, its temperament the milder parent's. It is dealt a sealed name at
//! birth, revealed when a visitor feeds it to sixty: one time in three a name that
//! carries its parents' (a blend, or one parent with a diminutive). One birth in six is
//! a mutant, and one of four things is different: its kind took a syllable from some
//! third creature in the world, a stat doubled, its temperament flipped, or it was born
//! wanting something no creature has wanted yet. The record says what was born and of
//! whom; the operator hears.

use super::combat::CREATURES;
use super::{errand, internal, mind, plain_num, plain_str, CREATURE, ROOM};
use crate::world::World;
use moondial_log::Actor;
use moondial_registry::CommandError;
use moondial_store::{EntityId, Field, StoreEvent};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// A creature was born: `{child, kind, room, parents, carrier, mutation}`. Public.
pub const BORN: &str = "disremembered.born";
/// One closed chain in this many kindles.
pub const BIRTH_IN: u32 = 2;
/// One birth in this many is a mutant.
pub const MUTATION_IN: u32 = 6;
/// One birth in this many is dealt a name that carries its parents'.
pub const HEIR_NAME_IN: u32 = 3;
/// Children alive at once, at most; the map is not a nursery.
pub const MAX_CHILDREN: usize = 12;

/// Syllables of the seed kinds, for the hybrid names; a kind not here is split by its
/// vowels.
const SYLLABLES: &[(&str, &[&str])] = &[
    ("wisp", &["wisp"]),
    ("hollow", &["hol", "low"]),
    ("revenant", &["rev", "e", "nant"]),
    ("inkling", &["ink", "ling"]),
    ("mothwing", &["moth", "wing"]),
    ("ragpicker", &["rag", "pick", "er"]),
    ("drowned", &["drown", "ed"]),
    ("sitter", &["sit", "ter"]),
    ("hourhand", &["hour", "hand"]),
    ("keeper", &["kee", "per"]),
];

/// Pairs that deserve a better word than the rule gives.
const NAMED_HYBRIDS: &[(&str, &str, &str)] = &[
    ("wisp", "inkling", "wispling"),
    ("inkling", "wisp", "inkwisp"),
    ("wisp", "mothwing", "wispwing"),
    ("mothwing", "wisp", "mothwisp"),
    ("hollow", "drowned", "hollowed"),
    ("drowned", "hollow", "drownlow"),
    ("keeper", "sitter", "keepsit"),
    ("sitter", "keeper", "sitkeep"),
    ("ragpicker", "inkling", "ragling"),
    ("inkling", "ragpicker", "inkpicker"),
];

fn syllables(kind: &str) -> Vec<String> {
    if let Some((_, s)) = SYLLABLES.iter().find(|(k, _)| *k == kind) {
        return s.iter().map(|x| x.to_string()).collect();
    }
    // Split after each vowel group followed by a consonant: "wispling" -> wis, pling.
    let chars: Vec<char> = kind.chars().collect();
    let is_v = |c: char| "aeiouy".contains(c);
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut seen_vowel = false;
    for (i, &c) in chars.iter().enumerate() {
        cur.push(c);
        if is_v(c) {
            seen_vowel = true;
        } else if seen_vowel && i + 1 < chars.len() && !is_v(chars[i + 1]) {
            out.push(std::mem::take(&mut cur));
            seen_vowel = false;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.is_empty() {
        vec![kind.to_string()]
    } else {
        out
    }
}

/// The child's kind: the sender's first syllable and the receiver's last, or the
/// table's word; like breeds true.
pub fn hybrid_kind(sender: &str, receiver: &str) -> String {
    if sender == receiver {
        return sender.to_string();
    }
    if let Some((_, _, name)) = NAMED_HYBRIDS
        .iter()
        .find(|(a, b, _)| *a == sender && *b == receiver)
    {
        return name.to_string();
    }
    let a = syllables(sender);
    let b = syllables(receiver);
    let head = a.first().cloned().unwrap_or_default();
    let tail = b.last().cloned().unwrap_or_default();
    let joined = format!("{head}{tail}");
    if joined == sender || joined == receiver || joined.len() < 4 {
        format!("{head}{}", b.first().cloned().unwrap_or_default())
    } else {
        joined
    }
}

/// A name that carries the parents': a blend of both, or one with a diminutive.
pub fn heir_name(parent_a: &str, parent_b: &str, roll: u32) -> String {
    let a: Vec<char> = parent_a.chars().collect();
    let b: Vec<char> = parent_b.chars().collect();
    match roll % 4 {
        0 => {
            let mut s: String = a[..a.len().div_ceil(2)].iter().collect();
            s.extend(b[b.len() / 2..].iter());
            s
        }
        1 => {
            let mut s: String = b[..b.len().div_ceil(2)].iter().collect();
            s.extend(a[a.len() / 2..].iter());
            s
        }
        2 => format!(
            "{parent_a}{}",
            ["kin", "ling", "let"][(roll as usize / 4) % 3]
        ),
        _ => format!(
            "{parent_b}{}",
            ["kin", "ling", "let"][(roll as usize / 4) % 3]
        ),
    }
}

/// Children alive now: creatures with parents.
fn children_alive(world: &World) -> usize {
    world
        .store
        .entities()
        .filter(|e| e.kind == "creature")
        .filter(|e| {
            e.components
                .get(CREATURE)
                .and_then(|c| c.fields.get("parents"))
                .is_some_and(|f| !matches!(f, Field::Plain(Value::Null)))
        })
        .count()
}

/// The room halfway along the route from `from` to `to` (rounded toward `from`), or
/// `from` when they are adjacent or the route is unknown.
fn midpoint(world: &World, from: EntityId, to: EntityId) -> EntityId {
    let Some(path) = errand::route(world, from, to) else {
        return from;
    };
    let steps = path.len() / 2;
    let mut at = from;
    for via in path.iter().take(steps) {
        match super::rooms::exits_of(world, at).get(via) {
            Some(&next) => at = next,
            None => return at,
        }
    }
    at
}

/// A chain closed: the way-home word reached `sender` (who sent the first word),
/// carried by `carrier`, from `receiver`. One time in two, a child.
pub fn try_birth(
    world: &mut World,
    carrier: &str,
    sender: EntityId,
    receiver: EntityId,
    word: &str,
) -> Result<Option<EntityId>, CommandError> {
    if world.roll(BIRTH_IN) != 0 {
        return Ok(None);
    }
    if children_alive(world) >= MAX_CHILDREN {
        return Ok(None);
    }
    let (Some(sender_name), Some(receiver_name)) =
        (mind::name(world, sender), mind::name(world, receiver))
    else {
        return Ok(None);
    };
    let sk = plain_str(world, sender, CREATURE, "name")
        .unwrap_or("wisp")
        .to_string();
    let rk = plain_str(world, receiver, CREATURE, "name")
        .unwrap_or("wisp")
        .to_string();
    let (Some(sr), Some(rr)) = (
        world.store.entity(sender).and_then(|e| e.parent),
        world.store.entity(receiver).and_then(|e| e.parent),
    ) else {
        return Ok(None);
    };
    let room = midpoint(world, sr, rr);
    let stat = |c: EntityId, f: &str| plain_num(world, c, CREATURE, f).unwrap_or(1.0);
    let mut kind = hybrid_kind(&sk, &rk);
    let mut hp = (stat(sender, "max_hp") + stat(receiver, "max_hp")) / 2.0;
    let mut attack = (stat(sender, "attack") + stat(receiver, "attack")) / 2.0;
    let xp = (stat(sender, "xp") + stat(receiver, "xp")) / 2.0;
    let mut aggressive = super::combat::is_aggressive(world, sender)
        && super::combat::is_aggressive(world, receiver);
    let mut mutation: Option<String> = None;
    let mut want: Option<String> = None;
    if world.roll(MUTATION_IN) == 0 {
        match world.roll(4) {
            0 => {
                // A syllable from some third creature in the world, or a seed kind.
                let others: Vec<String> = world
                    .store
                    .entities()
                    .filter(|e| e.kind == "creature")
                    .filter_map(|e| plain_str(world, e.id, CREATURE, "name").map(str::to_string))
                    .filter(|k| k != &sk && k != &rk)
                    .collect();
                let third = if others.is_empty() {
                    CREATURES[world.roll(CREATURES.len() as u32) as usize]
                        .key
                        .to_string()
                } else {
                    others[world.roll(others.len() as u32) as usize].clone()
                };
                let syl = syllables(&third);
                let extra = syl[world.roll(syl.len() as u32) as usize].clone();
                kind = format!(
                    "{}{extra}",
                    syllables(&kind).first().cloned().unwrap_or_default()
                );
                mutation = Some(format!(
                    "a kind of its own, with something of the {third} in it"
                ));
            }
            1 => {
                if world.roll(2) == 0 {
                    hp *= 2.0;
                    mutation = Some("twice the hit points of its parents".into());
                } else {
                    attack *= 2.0;
                    mutation = Some("twice the bite of its parents".into());
                }
            }
            2 => {
                aggressive = !aggressive;
                mutation = Some(if aggressive {
                    "a temper neither parent had".into()
                } else {
                    "a calm neither parent had".into()
                });
            }
            _ => {
                let w = STRANGE_WANTS[world.roll(STRANGE_WANTS.len() as u32) as usize];
                want = Some(w.to_string());
                mutation = Some(format!("born wanting {w}"));
            }
        }
    }
    // The sealed name: the parents' one time in three, else the list's.
    let heir = if world.roll(HEIR_NAME_IN) == 0 {
        Some(heir_name(&sender_name, &receiver_name, world.roll(12)))
    } else {
        None
    };
    // Born.
    let id = world.store.allocate_id();
    world
        .record(
            Actor::Driver,
            StoreEvent::EntityCreated {
                id,
                kind: "creature".into(),
                owner: None,
                parent: Some(room),
                scopes: vec![],
            },
        )
        .map_err(internal)?;
    let mut fields = BTreeMap::new();
    fields.insert("name".into(), Field::Plain(json!(kind)));
    fields.insert("hp".into(), Field::Plain(json!(hp)));
    fields.insert("max_hp".into(), Field::Plain(json!(hp)));
    fields.insert("attack".into(), Field::Plain(json!(attack)));
    fields.insert("xp".into(), Field::Plain(json!(xp)));
    fields.insert("aggressive".into(), Field::Plain(json!(aggressive)));
    fields.insert(
        "ambush".into(),
        Field::Plain(json!(format!(
            "comes at you, young and {}.",
            if aggressive { "fierce" } else { "unsure" }
        ))),
    );
    fields.insert(
        "parents".into(),
        Field::Plain(json!([sender_name, receiver_name])),
    );
    fields.insert("born_of".into(), Field::Plain(json!(carrier)));
    fields.insert("mutation".into(), Field::Plain(json!(mutation)));
    world
        .record(
            Actor::Driver,
            StoreEvent::ComponentSet {
                id,
                component: CREATURE.into(),
                fields,
            },
        )
        .map_err(internal)?;
    // Drawn with one parent's frame in the other's tint: a mix without new art.
    let frame = plain_str(world, sender, super::SPRITE, "frame")
        .unwrap_or(&sk)
        .to_string();
    let tint = plain_str(world, receiver, super::SPRITE, "tint")
        .unwrap_or("#9fd3e6")
        .to_string();
    let glyph = kind
        .chars()
        .next()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "?".into());
    world
        .record(
            Actor::Driver,
            StoreEvent::ComponentSet {
                id,
                component: super::SPRITE.into(),
                fields: super::sprite_fields(&glyph, &tint, &frame),
            },
        )
        .map_err(internal)?;
    // Its first memory: the word that made it; and it knows who made it.
    mind::born_knowing(
        world,
        id,
        &[carrier, &sender_name, &receiver_name],
        heir.as_deref(),
    )?;
    let window = mind::ensure_window_pub(world, id)?;
    let curves = super::Curves::with_half_life(moondial_time::Duration::seconds(3 * 3600));
    let chunk = super::memory::earn_chunk(
        world,
        carrier,
        window,
        &format!("the word that made it: \"{word}\""),
        errand::WORD_XP,
        0.6,
        &curves,
    )?;
    mind::disown(world, chunk)?;
    if let Some(w) = want {
        // A mutant born wanting: the want waits until it has a name to want with, and
        // the errand sweep leaves a want it did not set alone.
        errand::set_story(world, id, &w, "birth")?;
    }
    let rname = plain_str(world, room, ROOM, "name")
        .unwrap_or("somewhere")
        .to_string();
    let _ = world.note(
        Actor::Driver,
        BORN,
        json!({"child": id.0, "kind": kind, "room": room.0, "room_name": rname, "parents": [sender_name, receiver_name], "carrier": carrier, "mutation": mutation}),
    );
    world.notice(format!(
        "A **{kind}** was born in {rname}, of {sender_name} and {receiver_name}, carried by **{carrier}**{}.",
        mutation.as_ref().map(|m| format!(" — {m}")).unwrap_or_default()
    ));
    super::combat::narrate_room(
        world,
        id,
        &format!("Something small is here that was not: a {kind}, new."),
    );
    Ok(Some(id))
}

/// What a mutant may be born wanting: nothing any creature has asked for.
const STRANGE_WANTS: &[&str] = &[
    "the sound a door makes when no one is there to hear it",
    "what the moon does when it is not being looked at",
    "a lantern that is afraid of the dark",
    "the first thing a wisp forgets",
    "a stair that only goes up",
    "a visitor who stayed",
];
