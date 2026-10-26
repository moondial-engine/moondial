//! Moondial driver. Pre-alpha: the primitives arrive one crate at a time, and this crate
//! assembles them. For now it re-exports them so `moondial::time`, `moondial::log` and
//! `moondial::store` are stable paths.

pub use moondial_log as log;
pub use moondial_store as store;
pub use moondial_time as time;
