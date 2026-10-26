//! Moondial driver. Pre-alpha: the primitives arrive one crate at a time, and this crate
//! assembles them. For now it re-exports the time primitive so `moondial::time` is a stable path.

pub use moondial_time as time;
