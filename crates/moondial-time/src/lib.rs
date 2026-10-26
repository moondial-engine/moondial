//! Time-evaluated state for the Moondial engine.
//!
//! A moondial is a sundial read by moonlight: it gives the right time only when the
//! correction for the moon's phase is applied at the moment of reading. This crate is that
//! idea as a primitive. A [`TimedValue`] stores a [`Waterline`] (a value and the moment it
//! was committed) and a [`Curve`]. Reading it applies the curve to the time that has
//! *actively* elapsed since the waterline, where active time excludes every interval in
//! which the value's [`Clock`] scope was paused. Nothing ticks. A read never moves the
//! waterline; only a mutation ([`TimedValue::commit`] or [`TimedValue::recurve`]) does.
//!
//! The rules this crate guarantees, and that its property tests check:
//!
//! - **Reads are pure.** Reading at `t` any number of times, in any order, gives the same
//!   value as reading once. Intermediate reads never change later reads.
//! - **Pauses are exact.** While a scope is paused the value is constant. Pausing for `d`
//!   and then reading at `t` equals reading at `t - d` with no pause.
//! - **Replay agrees with lazy evaluation.** The value computed from a waterline and the
//!   clock's pause intervals equals the value computed by replaying the event log that
//!   produced them.
//! - **Decay curves are monotone.** A decaying curve never increases between reads.
//!
//! Curves need not be memoryless. A power-law tail, which Entropic uses for the Memory
//! Palace, gives a different value if re-anchored halfway; that is why reads never
//! re-anchor and only mutations do. Moving a chunk between tiers is a [`TimedValue::recurve`],
//! which anchors the current value on the new curve, and that is the intended semantics.
//!
//! Timestamps are whole milliseconds since the Unix epoch, UTC. The engine stamps every
//! event with one; this crate never consults a wall clock.
//!
//! Values and curve parameters are `f64`. Any JSON path that carries them must parse
//! floats round-trip exactly (`serde_json` with the `float_roundtrip` feature); the
//! default fast path can be off by one ulp, which the property tests catch.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};
use std::fmt;

/// A moment: whole milliseconds since the Unix epoch, UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

/// A span of time in whole milliseconds. Never negative when produced by this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Duration(pub i64);

impl Timestamp {
    /// Milliseconds from `earlier` to `self`, clamped at zero.
    pub fn since(self, earlier: Timestamp) -> Duration {
        Duration((self.0 - earlier.0).max(0))
    }
    /// This moment plus a duration.
    pub fn plus(self, d: Duration) -> Timestamp {
        Timestamp(self.0 + d.0)
    }
}

impl Duration {
    /// Zero milliseconds.
    pub const ZERO: Duration = Duration(0);
    /// A duration of whole seconds.
    pub const fn seconds(s: i64) -> Duration {
        Duration(s * 1_000)
    }
    /// A duration of whole hours.
    pub const fn hours(h: i64) -> Duration {
        Duration(h * 3_600_000)
    }
    /// A duration of whole days.
    pub const fn days(d: i64) -> Duration {
        Duration(d * 86_400_000)
    }
    /// The duration as fractional milliseconds, for curve arithmetic.
    pub fn as_ms_f64(self) -> f64 {
        self.0 as f64
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "t={}", self.0)
    }
}

/// Errors from clock operations.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ClockError {
    /// `pause` was called while the scope was already paused.
    #[error("clock is already paused since {0}")]
    AlreadyPaused(Timestamp),
    /// `resume` was called while the scope was not paused.
    #[error("clock is not paused")]
    NotPaused,
    /// An event arrived with a timestamp earlier than the previous event in the scope.
    #[error("event at {at} is earlier than the last event at {last}")]
    OutOfOrder {
        /// The offending timestamp.
        at: Timestamp,
        /// The last timestamp the scope accepted.
        last: Timestamp,
    },
}

/// A closed-or-open interval during which a clock scope was paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pause {
    /// When the pause began.
    pub from: Timestamp,
    /// When it ended, or `None` if it is still in effect.
    pub until: Option<Timestamp>,
}

impl Pause {
    /// Milliseconds of this pause that fall inside `[from, to]`.
    fn overlap(&self, from: Timestamp, to: Timestamp) -> i64 {
        let start = self.from.0.max(from.0);
        let end = self.until.map_or(to.0, |u| u.0.min(to.0));
        (end - start).max(0)
    }
}

/// A clock scope: the record of when time was paused for everything in the scope.
///
/// A scope is an append-only list of pauses in time order. Entropic's PTO is a pause on an
/// account's scope; a world-wide maintenance freeze is a pause on the world's scope. A
/// value may belong to several scopes at once (see [`Clock::elapsed`] on a slice).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clock {
    pauses: Vec<Pause>,
    last_event: Option<Timestamp>,
}

impl Clock {
    /// A scope that has never been paused.
    pub fn new() -> Self {
        Self::default()
    }

    /// The recorded pauses, oldest first.
    pub fn pauses(&self) -> &[Pause] {
        &self.pauses
    }

    /// Whether the scope is paused at `at`.
    pub fn is_paused_at(&self, at: Timestamp) -> bool {
        self.pauses
            .iter()
            .any(|p| p.from <= at && p.until.map_or(true, |u| at < u))
    }

    fn accept(&mut self, at: Timestamp) -> Result<(), ClockError> {
        if let Some(last) = self.last_event {
            if at < last {
                return Err(ClockError::OutOfOrder { at, last });
            }
        }
        self.last_event = Some(at);
        Ok(())
    }

    /// Record that the scope's time stopped at `at`.
    pub fn pause(&mut self, at: Timestamp) -> Result<(), ClockError> {
        if let Some(open) = self.pauses.last().filter(|p| p.until.is_none()) {
            return Err(ClockError::AlreadyPaused(open.from));
        }
        self.accept(at)?;
        self.pauses.push(Pause {
            from: at,
            until: None,
        });
        Ok(())
    }

    /// Record that the scope's time started again at `at`.
    pub fn resume(&mut self, at: Timestamp) -> Result<(), ClockError> {
        match self.pauses.last_mut() {
            Some(open) if open.until.is_none() => {
                if at < open.from {
                    return Err(ClockError::OutOfOrder {
                        at,
                        last: open.from,
                    });
                }
                self.last_event = Some(at);
                open.until = Some(at);
                Ok(())
            }
            _ => Err(ClockError::NotPaused),
        }
    }

    /// Active milliseconds between `from` and `to` in this scope alone.
    pub fn elapsed_in(&self, from: Timestamp, to: Timestamp) -> Duration {
        Clock::elapsed(std::slice::from_ref(self), from, to)
    }

    /// Active milliseconds between `from` and `to` across several scopes.
    ///
    /// Time counts only when *no* scope is paused, so overlapping pauses are not double
    /// counted: the union of all pause intervals is subtracted once.
    pub fn elapsed(scopes: &[Clock], from: Timestamp, to: Timestamp) -> Duration {
        if to <= from {
            return Duration::ZERO;
        }
        let mut intervals: Vec<(i64, i64)> = scopes
            .iter()
            .flat_map(|c| c.pauses.iter())
            .filter_map(|p| {
                let o = p.overlap(from, to);
                (o > 0).then(|| {
                    let s = p.from.0.max(from.0);
                    (s, s + o)
                })
            })
            .collect();
        intervals.sort_unstable();
        let mut paused = 0i64;
        let mut cur: Option<(i64, i64)> = None;
        for (s, e) in intervals {
            match cur {
                Some((cs, ce)) if s <= ce => cur = Some((cs, ce.max(e))),
                Some((cs, ce)) => {
                    paused += ce - cs;
                    cur = Some((s, e));
                }
                None => cur = Some((s, e)),
            }
        }
        if let Some((cs, ce)) = cur {
            paused += ce - cs;
        }
        Duration((to.0 - from.0) - paused)
    }
}

/// How a value changes with active elapsed time since its waterline.
///
/// Every curve maps `(value_at_waterline, active_ms)` to a value. Bounds clamp the result.
/// Modules may register their own curves through [`CurveFn`]; the families here cover
/// what Entropic needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Curve {
    /// The value never changes with time.
    Constant,
    /// Adds `rate_per_ms` per active millisecond (negative to decay).
    Linear {
        /// Change per active millisecond.
        rate_per_ms: f64,
    },
    /// Halves every `half_life` of active time. Memoryless: re-anchoring changes nothing.
    Exponential {
        /// Active time for the value to halve. Must be positive.
        half_life: Duration,
    },
    /// `v / (1 + t/scale)^exponent`: strong early fade, long tail. Not memoryless.
    PowerLaw {
        /// Active time at which the denominator is 2. Must be positive.
        scale: Duration,
        /// Steepness; 1.0 is a plain hyperbola. Must be positive.
        exponent: f64,
    },
    /// Adds `delta` once per completed `interval` of active time.
    Step {
        /// Active time per step. Must be positive.
        interval: Duration,
        /// Change per step (negative to decay).
        delta: f64,
    },
}

/// Lower and upper clamps applied after the curve.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    /// The value never goes below this.
    pub floor: f64,
    /// The value never goes above this, when set. `None` means no ceiling; infinity is
    /// avoided because JSON cannot carry it.
    pub ceiling: Option<f64>,
}

impl Default for Bounds {
    fn default() -> Self {
        Bounds {
            floor: 0.0,
            ceiling: None,
        }
    }
}

impl Bounds {
    /// Clamp a value into these bounds.
    pub fn clamp(&self, v: f64) -> f64 {
        let v = v.max(self.floor);
        match self.ceiling {
            Some(c) => v.min(c),
            None => v,
        }
    }
}

/// A curve supplied by a module: a pure function of the waterline value and active time.
pub trait CurveFn: fmt::Debug {
    /// The value after `active` milliseconds since a waterline of `at_waterline`.
    fn value(&self, at_waterline: f64, active: Duration) -> f64;
    /// Whether the curve never increases with time. Enables the monotonicity check.
    fn is_decaying(&self) -> bool {
        false
    }
}

impl CurveFn for Curve {
    fn value(&self, v0: f64, active: Duration) -> f64 {
        let t = active.as_ms_f64();
        match *self {
            Curve::Constant => v0,
            Curve::Linear { rate_per_ms } => v0 + rate_per_ms * t,
            Curve::Exponential { half_life } => {
                let hl = half_life.as_ms_f64();
                if hl <= 0.0 {
                    return v0;
                }
                v0 * 0.5f64.powf(t / hl)
            }
            Curve::PowerLaw { scale, exponent } => {
                let s = scale.as_ms_f64();
                if s <= 0.0 || exponent <= 0.0 {
                    return v0;
                }
                v0 / (1.0 + t / s).powf(exponent)
            }
            Curve::Step { interval, delta } => {
                if interval.0 <= 0 {
                    return v0;
                }
                let steps = (active.0 / interval.0) as f64;
                v0 + delta * steps
            }
        }
    }

    fn is_decaying(&self) -> bool {
        match *self {
            Curve::Constant => true,
            Curve::Linear { rate_per_ms } => rate_per_ms <= 0.0,
            Curve::Exponential { half_life } => half_life.0 > 0,
            Curve::PowerLaw { scale, exponent } => scale.0 > 0 && exponent > 0.0,
            Curve::Step { interval, delta } => interval.0 > 0 && delta <= 0.0,
        }
    }
}

impl Curve {
    /// Active time until the value falls from `v0` to `target`, for curves that can be
    /// inverted. `None` if the curve never reaches it or cannot be inverted. Jobs use this
    /// to schedule "when does this bottom out" without polling.
    pub fn time_to_reach(&self, v0: f64, target: f64) -> Option<Duration> {
        if target >= v0 {
            return Some(Duration::ZERO);
        }
        if target <= 0.0 && !matches!(self, Curve::Linear { .. } | Curve::Step { .. }) {
            return None; // asymptotic curves never reach zero
        }
        let ms = match *self {
            Curve::Constant => return None,
            Curve::Linear { rate_per_ms } => {
                if rate_per_ms >= 0.0 {
                    return None;
                }
                (target - v0) / rate_per_ms
            }
            Curve::Exponential { half_life } => {
                if half_life.0 <= 0 || v0 <= 0.0 || target <= 0.0 {
                    return None;
                }
                half_life.as_ms_f64() * (v0 / target).log2()
            }
            Curve::PowerLaw { scale, exponent } => {
                if scale.0 <= 0 || exponent <= 0.0 || v0 <= 0.0 || target <= 0.0 {
                    return None;
                }
                scale.as_ms_f64() * ((v0 / target).powf(1.0 / exponent) - 1.0)
            }
            Curve::Step { interval, delta } => {
                if interval.0 <= 0 || delta >= 0.0 {
                    return None;
                }
                ((v0 - target) / -delta).ceil() * interval.as_ms_f64()
            }
        };
        Some(Duration(ms.ceil() as i64))
    }
}

/// The stored value and the moment it was committed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Waterline {
    /// The value at `at`.
    pub value: f64,
    /// When `value` was committed.
    pub at: Timestamp,
}

/// A value that is a curve over a waterline: computed when read, never ticked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimedValue {
    /// The last committed value and its moment.
    pub waterline: Waterline,
    /// How the value changes with active time since the waterline.
    pub curve: Curve,
    /// Clamps applied after the curve.
    pub bounds: Bounds,
}

impl TimedValue {
    /// A value committed at `at` on `curve`, with default bounds (floor 0, no ceiling).
    pub fn new(value: f64, at: Timestamp, curve: Curve) -> Self {
        TimedValue {
            waterline: Waterline { value, at },
            curve,
            bounds: Bounds::default(),
        }
    }

    /// The same value with explicit bounds.
    pub fn with_bounds(mut self, bounds: Bounds) -> Self {
        self.bounds = bounds;
        self
    }

    /// Read the value at `now`, with time paused in any of `scopes` excluded.
    /// Pure: reading never changes the waterline.
    pub fn read_at(&self, now: Timestamp, scopes: &[Clock]) -> f64 {
        let active = Clock::elapsed(scopes, self.waterline.at, now);
        self.bounds
            .clamp(self.curve.value(self.waterline.value, active))
    }

    /// Read with a module-supplied curve instead of the stored one. Pure.
    pub fn read_with(&self, curve: &dyn CurveFn, now: Timestamp, scopes: &[Clock]) -> f64 {
        let active = Clock::elapsed(scopes, self.waterline.at, now);
        self.bounds.clamp(curve.value(self.waterline.value, active))
    }

    /// Commit a new value at `now`. This is the only way the waterline moves.
    pub fn commit(&mut self, value: f64, now: Timestamp) {
        self.waterline = Waterline {
            value: self.bounds.clamp(value),
            at: now,
        };
    }

    /// Change the curve at `now`, anchoring the current value on the new curve. Entropic's
    /// shelving and reminiscence are recurves between the active and palace curves.
    pub fn recurve(&mut self, curve: Curve, now: Timestamp, scopes: &[Clock]) {
        let current = self.read_at(now, scopes);
        self.waterline = Waterline {
            value: current,
            at: now,
        };
        self.curve = curve;
    }

    /// Active time, from `now`, until the value reaches `target`, if the curve can say.
    /// Wall-clock time will be longer if the scope pauses.
    pub fn active_time_to_reach(
        &self,
        target: f64,
        now: Timestamp,
        scopes: &[Clock],
    ) -> Option<Duration> {
        let current = self.read_at(now, scopes);
        self.curve.time_to_reach(current, self.bounds.clamp(target))
    }
}

/// An event in the log that produces a [`TimedValue`] and its [`Clock`]. Replaying a
/// sequence of these is the oracle the property tests compare lazy evaluation against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Event {
    /// The value was committed.
    Commit {
        /// When.
        at: Timestamp,
        /// To what.
        value: f64,
    },
    /// The curve changed, anchoring the current value.
    Recurve {
        /// When.
        at: Timestamp,
        /// To which curve.
        curve: Curve,
    },
    /// The scope paused.
    Pause {
        /// When.
        at: Timestamp,
    },
    /// The scope resumed.
    Resume {
        /// When.
        at: Timestamp,
    },
}

impl Event {
    /// The event's timestamp.
    pub fn at(&self) -> Timestamp {
        match self {
            Event::Commit { at, .. }
            | Event::Recurve { at, .. }
            | Event::Pause { at }
            | Event::Resume { at } => *at,
        }
    }
}

/// Replay an event log from an initial value into a value and its clock. Events must be in
/// time order; clock errors (double pause, resume without pause) are returned.
pub fn replay(initial: TimedValue, events: &[Event]) -> Result<(TimedValue, Clock), ClockError> {
    let mut value = initial;
    let mut clock = Clock::new();
    for ev in events {
        match ev {
            Event::Commit { at, value: v } => value.commit(*v, *at),
            Event::Recurve { at, curve } => {
                let scopes = std::slice::from_ref(&clock);
                value.recurve(curve.clone(), *at, scopes);
            }
            Event::Pause { at } => clock.pause(*at)?,
            Event::Resume { at } => clock.resume(*at)?,
        }
    }
    Ok((value, clock))
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: Timestamp = Timestamp(1_700_000_000_000);
    fn at(ms: i64) -> Timestamp {
        Timestamp(T0.0 + ms)
    }
    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
    }

    #[test]
    fn exponential_halves_each_half_life() {
        let v = TimedValue::new(
            100.0,
            T0,
            Curve::Exponential {
                half_life: Duration::days(1),
            },
        );
        assert!(approx(v.read_at(T0, &[]), 100.0));
        assert!(approx(v.read_at(at(Duration::days(1).0), &[]), 50.0));
        assert!(approx(v.read_at(at(Duration::days(3).0), &[]), 12.5));
    }

    #[test]
    fn pause_freezes_the_value_exactly() {
        let v = TimedValue::new(
            100.0,
            T0,
            Curve::Exponential {
                half_life: Duration::days(1),
            },
        );
        let mut clock = Clock::new();
        clock.pause(at(Duration::hours(6).0)).unwrap();
        clock.resume(at(Duration::hours(30).0)).unwrap();
        // Paused for 24 h: reading at 48 h equals an unpaused read at 24 h.
        let paused = v.read_at(at(Duration::hours(48).0), &[clock]);
        let unpaused = v.read_at(at(Duration::hours(24).0), &[]);
        assert!(approx(paused, unpaused));
        // Inside the pause the value does not move.
        let mut c2 = Clock::new();
        c2.pause(at(Duration::hours(6).0)).unwrap();
        let a = v.read_at(at(Duration::hours(7).0), std::slice::from_ref(&c2));
        let b = v.read_at(at(Duration::hours(900).0), std::slice::from_ref(&c2));
        assert!(approx(a, b));
    }

    #[test]
    fn overlapping_pauses_across_scopes_are_not_double_counted() {
        let mut account = Clock::new();
        account.pause(at(100)).unwrap();
        account.resume(at(300)).unwrap();
        let mut world = Clock::new();
        world.pause(at(200)).unwrap();
        world.resume(at(400)).unwrap();
        // The union of the pauses is [100, 400): 300 ms paused out of 1000.
        assert_eq!(
            Clock::elapsed(&[account.clone(), world.clone()], T0, at(1000)),
            Duration(700)
        );
        let v = TimedValue::new(1000.0, T0, Curve::Linear { rate_per_ms: -1.0 });
        assert!(approx(v.read_at(at(1000), &[account, world]), 300.0));
        assert_eq!(
            Clock::elapsed(&[Clock::new()], T0, at(1000)),
            Duration(1000)
        );
    }

    #[test]
    fn clock_rejects_double_pause_and_stray_resume() {
        let mut c = Clock::new();
        assert_eq!(c.resume(at(1)), Err(ClockError::NotPaused));
        c.pause(at(1)).unwrap();
        assert_eq!(c.pause(at(2)), Err(ClockError::AlreadyPaused(at(1))));
        assert_eq!(
            c.resume(at(0)),
            Err(ClockError::OutOfOrder {
                at: at(0),
                last: at(1)
            })
        );
        c.resume(at(5)).unwrap();
        assert_eq!(
            c.pause(at(4)),
            Err(ClockError::OutOfOrder {
                at: at(4),
                last: at(5)
            })
        );
    }

    #[test]
    fn recurve_anchors_current_value_on_new_curve() {
        let mut v = TimedValue::new(
            100.0,
            T0,
            Curve::Exponential {
                half_life: Duration::days(1),
            },
        );
        let shelf = at(Duration::days(1).0);
        v.recurve(
            Curve::Exponential {
                half_life: Duration::days(5),
            },
            shelf,
            &[],
        );
        assert!(approx(v.waterline.value, 50.0));
        assert_eq!(v.waterline.at, shelf);
        assert!(approx(v.read_at(at(Duration::days(6).0), &[]), 25.0));
    }

    #[test]
    fn bounds_clamp_and_floor_is_zero_by_default() {
        let v = TimedValue::new(10.0, T0, Curve::Linear { rate_per_ms: -1.0 });
        assert_eq!(v.read_at(at(50), &[]), 0.0);
        let g = TimedValue::new(10.0, T0, Curve::Linear { rate_per_ms: 1.0 }).with_bounds(Bounds {
            floor: 0.0,
            ceiling: Some(15.0),
        });
        assert_eq!(g.read_at(at(50), &[]), 15.0);
    }

    #[test]
    fn time_to_reach_inverts_the_curves() {
        let hl = Duration::days(1);
        let e = Curve::Exponential { half_life: hl };
        assert_eq!(e.time_to_reach(100.0, 25.0), Some(Duration::days(2)));
        assert_eq!(e.time_to_reach(100.0, 0.0), None);
        let l = Curve::Linear { rate_per_ms: -2.0 };
        assert_eq!(l.time_to_reach(100.0, 0.0), Some(Duration(50)));
        let p = Curve::PowerLaw {
            scale: Duration(1000),
            exponent: 2.0,
        };
        let t = p.time_to_reach(100.0, 25.0).unwrap();
        assert_eq!(t, Duration(1000)); // (1 + 1)^2 = 4
        let s = Curve::Step {
            interval: Duration(10),
            delta: -3.0,
        };
        assert_eq!(s.time_to_reach(10.0, 0.0), Some(Duration(40)));
        assert_eq!(Curve::Constant.time_to_reach(1.0, 0.5), None);
    }

    #[test]
    fn replay_matches_direct_construction() {
        let init = TimedValue::new(
            100.0,
            T0,
            Curve::Exponential {
                half_life: Duration::days(1),
            },
        );
        let events = vec![
            Event::Pause {
                at: at(Duration::hours(1).0),
            },
            Event::Resume {
                at: at(Duration::hours(13).0),
            },
            Event::Recurve {
                at: at(Duration::hours(24).0),
                curve: Curve::PowerLaw {
                    scale: Duration::days(2),
                    exponent: 1.5,
                },
            },
            Event::Commit {
                at: at(Duration::hours(30).0),
                value: 80.0,
            },
        ];
        let (v, clock) = replay(init, &events).unwrap();
        assert!(approx(v.waterline.value, 80.0));
        let now = at(Duration::days(4).0);
        let expect = Curve::PowerLaw {
            scale: Duration::days(2),
            exponent: 1.5,
        }
        .value(80.0, Duration(now.0 - at(Duration::hours(30).0).0));
        assert!(approx(v.read_at(now, std::slice::from_ref(&clock)), expect));
    }
}

#[cfg(test)]
mod properties {
    use super::*;
    use proptest::prelude::*;

    const T0: Timestamp = Timestamp(1_700_000_000_000);

    fn curve_strategy() -> impl Strategy<Value = Curve> {
        prop_oneof![
            Just(Curve::Constant),
            (-0.01f64..0.0).prop_map(|r| Curve::Linear { rate_per_ms: r }),
            (1_000i64..10_000_000).prop_map(|h| Curve::Exponential {
                half_life: Duration(h)
            }),
            ((1_000i64..10_000_000), (0.2f64..4.0)).prop_map(|(s, e)| Curve::PowerLaw {
                scale: Duration(s),
                exponent: e
            }),
            ((100i64..100_000), (-5.0f64..0.0)).prop_map(|(i, d)| Curve::Step {
                interval: Duration(i),
                delta: d
            }),
        ]
    }

    /// A time-ordered event log with alternating pause/resume, random commits and recurves.
    fn log_strategy() -> impl Strategy<Value = Vec<Event>> {
        prop::collection::vec(
            (1i64..1_000_000, 0u8..4, 0.0f64..1000.0, curve_strategy()),
            0..12,
        )
        .prop_map(|steps| {
            let mut t = T0.0;
            let mut paused = false;
            let mut out = Vec::new();
            for (gap, kind, v, c) in steps {
                t += gap;
                let at = Timestamp(t);
                match kind {
                    0 => out.push(Event::Commit { at, value: v }),
                    1 => out.push(Event::Recurve { at, curve: c }),
                    _ => {
                        if paused {
                            out.push(Event::Resume { at });
                        } else {
                            out.push(Event::Pause { at });
                        }
                        paused = !paused;
                    }
                }
            }
            out
        })
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1.0)
    }

    proptest! {
        #[test]
        fn reads_are_pure(log in log_strategy(), v0 in 0.0f64..1000.0, c in curve_strategy(), g1 in 0i64..10_000_000, g2 in 0i64..10_000_000) {
            let (v, clock) = replay(TimedValue::new(v0, T0, c), &log).unwrap();
            let end = log.last().map_or(T0, |e| e.at());
            let t1 = Timestamp(end.0 + g1);
            let t2 = Timestamp(end.0 + g1 + g2);
            let scopes = std::slice::from_ref(&clock);
            let direct = v.read_at(t2, scopes);
            let _ = v.read_at(t1, scopes); // an intermediate read
            let again = v.read_at(t2, scopes);
            prop_assert!(close(direct, again));
            prop_assert!(v.read_at(t1, scopes) == v.read_at(t1, scopes));
        }

        #[test]
        fn decay_is_monotone_over_time(log in log_strategy(), v0 in 0.0f64..1000.0, c in curve_strategy(), gaps in prop::collection::vec(0i64..1_000_000, 1..8)) {
            let (v, clock) = replay(TimedValue::new(v0, T0, c), &log).unwrap();
            prop_assume!(v.curve.is_decaying());
            let scopes = std::slice::from_ref(&clock);
            let mut t = log.last().map_or(T0, |e| e.at());
            let mut last = v.read_at(t, scopes);
            for g in gaps {
                t = Timestamp(t.0 + g);
                let now = v.read_at(t, scopes);
                prop_assert!(now <= last + 1e-9, "value rose from {last} to {now}");
                last = now;
            }
        }

        #[test]
        fn a_pause_shifts_time_exactly(v0 in 0.0f64..1000.0, c in curve_strategy(), before in 0i64..1_000_000, pause_len in 0i64..1_000_000, after in 0i64..1_000_000) {
            let v = TimedValue::new(v0, T0, c);
            let mut clock = Clock::new();
            clock.pause(Timestamp(T0.0 + before)).unwrap();
            clock.resume(Timestamp(T0.0 + before + pause_len)).unwrap();
            let with_pause = v.read_at(Timestamp(T0.0 + before + pause_len + after), std::slice::from_ref(&clock));
            let without = v.read_at(Timestamp(T0.0 + before + after), &[]);
            prop_assert!(close(with_pause, without), "{with_pause} vs {without}");
        }

        #[test]
        fn elapsed_never_exceeds_wall_clock_and_union_is_order_independent(log in log_strategy(), extra in 0i64..1_000_000) {
            let (_, clock) = replay(TimedValue::new(1.0, T0, Curve::Constant), &log).unwrap();
            let end = Timestamp(log.last().map_or(T0, |e| e.at()).0 + extra);
            let e = clock.elapsed_in(T0, end);
            prop_assert!(e.0 >= 0 && e.0 <= end.0 - T0.0);
            // Two copies of the same scope pause the same intervals: no double counting.
            let twice = Clock::elapsed(&[clock.clone(), clock.clone()], T0, end);
            prop_assert_eq!(twice, e);
        }

        #[test]
        fn time_to_reach_is_consistent_with_reading(v0 in 1.0f64..1000.0, c in curve_strategy(), frac in 0.05f64..0.95) {
            let v = TimedValue::new(v0, T0, c.clone());
            let target = v0 * frac;
            if let Some(d) = c.time_to_reach(v0, target) {
                let reached = v.read_at(T0.plus(d), &[]);
                prop_assert!(reached <= target + 1e-6 * v0, "after {d:?}: {reached} > {target}");
                if d.0 > 1 && !matches!(c, Curve::Step { .. }) {
                    let before = v.read_at(T0.plus(Duration(d.0 - 1)), &[]);
                    prop_assert!(before >= target - 1e-3 * v0, "one ms early: {before} < {target}");
                }
            }
        }

        #[test]
        fn serde_round_trip(log in log_strategy(), v0 in 0.0f64..1000.0, c in curve_strategy()) {
            let (v, clock) = replay(TimedValue::new(v0, T0, c), &log).unwrap();
            let v2: TimedValue = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
            let c2: Clock = serde_json::from_str(&serde_json::to_string(&clock).unwrap()).unwrap();
            prop_assert_eq!(&v2, &v);
            prop_assert_eq!(&c2, &clock);
        }
    }
}
