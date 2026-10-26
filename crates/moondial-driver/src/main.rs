//! The Moondial driver, pre-alpha. Loads one world (the Entropic text slice), replays its
//! log, and listens on a text port and an HTTP port. Configuration is by environment:
//!
//! - `MOONDIAL_DATA`: directory for the event log (default `./data`); `memory` for none.
//! - `MOONDIAL_WORLD`: world name (default `entropic`).
//! - `MOONDIAL_TCP`: text listener (default `127.0.0.1:7778`).
//! - `MOONDIAL_HTTP`: HTTP listener (default `127.0.0.1:7777`).
//! - `MOONDIAL_ADMIN_TOKEN`: enables `admin <token>` and the `X-Moondial-Admin` header.
//! - `MOONDIAL_ACTIVE_HALF_LIFE_SECONDS`: the active window's half-life (default 3600).

mod entropic;
mod net;
mod world;

use moondial_log::WorldId;
use moondial_store::{Registry, Store};
use std::sync::{Arc, Mutex};
use world::{LogBacking, World};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("moondial-driver {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let world_name = env_or("MOONDIAL_WORLD", "entropic");
    let data = env_or("MOONDIAL_DATA", "./data");
    let backing = if data == "memory" {
        LogBacking::Memory
    } else {
        LogBacking::File(data.clone().into())
    };

    let mut registry = Registry::new();
    for s in entropic::schemas() {
        registry.register(s);
    }
    let mut world = match World::open(WorldId(world_name.clone()), Store::new(registry), backing) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("cannot open world {world_name}: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = entropic::ensure_room(&mut world) {
        eprintln!("cannot create the room: {e}");
        std::process::exit(1);
    }
    entropic::register(&mut world, entropic::Curves::from_env());
    let events = world
        .log
        .head(&world.id)
        .ok()
        .flatten()
        .map(|s| s.0)
        .unwrap_or(0);
    eprintln!(
        "moondial-driver {} · world {} · {} events replayed from {} · {} commands",
        env!("CARGO_PKG_VERSION"),
        world_name,
        events,
        if data == "memory" {
            "memory".to_string()
        } else {
            data.clone()
        },
        world.registry.specs().count()
    );

    let shared = net::Shared {
        world: Arc::new(Mutex::new(world)),
        admin_token: std::env::var("MOONDIAL_ADMIN_TOKEN").ok(),
    };
    let tcp = env_or("MOONDIAL_TCP", "127.0.0.1:7778");
    let http = env_or("MOONDIAL_HTTP", "127.0.0.1:7777");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        let a = tokio::spawn(net::serve_tcp(shared.clone(), tcp));
        let b = tokio::spawn(net::serve_http(shared, http));
        tokio::select! {
            _ = tokio::signal::ctrl_c() => eprintln!("shutting down"),
            _ = a => eprintln!("tcp listener stopped"),
            _ = b => eprintln!("http listener stopped"),
        }
    });
}
