//! The Moondial driver, pre-alpha. Loads one world (the Disremembered text slice), replays its
//! log, and listens on a text port and an HTTP port. Configuration is by environment:
//!
//! - `MOONDIAL_LOG_URL`: a PostgreSQL URL (`postgres://user:pass@host/db`); the event log
//!   lives there when set, and `MOONDIAL_DATA` is ignored.
//! - `MOONDIAL_DATA`: directory for the file event log (default `./data`); `memory` for none.
//!
//! `moondial-driver import <dir>` copies the world's file log from `<dir>` into the
//! PostgreSQL log `MOONDIAL_LOG_URL` names, once, and exits.
//! - `MOONDIAL_WORLD`: world name (default `disremembered`).
//! - `MOONDIAL_TCP`: text listener (default `127.0.0.1:7778`).
//! - `MOONDIAL_HTTP`: HTTP listener (default `127.0.0.1:7777`).
//! - `MOONDIAL_ADMIN_TOKEN`: enables `admin <token>` and the `X-Moondial-Admin` header.
//! - `MOONDIAL_ACTIVE_HALF_LIFE_SECONDS`: the active window's half-life (default 3600).
//! - `MOONDIAL_CORS_ORIGINS`: browser origins allowed to call the HTTP port, comma-separated
//!   (default the agentscanplay.com site).
//! - `MOONDIAL_OPS_LOG`: the operator's side channel, a JSON-lines file of who came from
//!   where (every MCP call and arrival, with the client address), read by the analytics
//!   collector and kept ninety days there. Default `<MOONDIAL_DATA>/ops.jsonl`; `none`
//!   writes nothing. Never the world's log.
//! - `MOONDIAL_LAB_NETS`: the operator's own networks, comma-separated addresses or CIDRs;
//!   an arrival from anywhere else is organic, and the Discord line says so.
//! - `MOONDIAL_DISCORD_WEBHOOK`: a Discord webhook URL; arrivals worth knowing about (a
//!   first arrival, an agent's channel, a return after a day away) are posted there.

mod disremembered;
mod gate;
mod jobs;
mod mcp;
mod net;
mod notify;
mod observe;
mod openapi;
mod session;
mod world;

use moondial_log::WorldId;
use moondial_store::Store;
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
    let world_name = env_or("MOONDIAL_WORLD", "disremembered");
    let data = env_or("MOONDIAL_DATA", "./data");
    let log_url = std::env::var("MOONDIAL_LOG_URL")
        .ok()
        .filter(|v| !v.is_empty());

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("import") {
        let Some(dir) = args.get(2) else {
            eprintln!("usage: moondial-driver import <dir>   (with MOONDIAL_LOG_URL set)");
            std::process::exit(2);
        };
        let Some(url) = log_url else {
            eprintln!("import needs MOONDIAL_LOG_URL, the PostgreSQL log to copy into");
            std::process::exit(2);
        };
        match import_file_log(dir, &url, &world_name) {
            Ok(n) => println!("imported {n} events of world {world_name} from {dir}"),
            Err(e) => {
                eprintln!("import failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let backing = match log_url {
        Some(url) => LogBacking::Postgres(url),
        None if data == "memory" => LogBacking::Memory,
        None => LogBacking::File(data.clone().into()),
    };
    let where_ = match &backing {
        LogBacking::Postgres(_) => "PostgreSQL".to_string(),
        LogBacking::Memory => "memory".to_string(),
        LogBacking::File(d) => d.display().to_string(),
    };

    let mut world = match World::open(
        WorldId(world_name.clone()),
        Store::new(disremembered::registry()),
        backing,
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("cannot open world {world_name}: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = disremembered::ensure_world(&mut world) {
        eprintln!("cannot build the rooms: {e}");
        std::process::exit(1);
    }
    disremembered::register(&mut world, disremembered::Curves::from_env());
    let ran = world.tick();
    if ran > 0 {
        eprintln!("ran {ran} jobs that came due while the driver was down");
    }
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
        where_,
        world.registry.specs().count()
    );

    world.lab_nets = world::lab_nets_from(&env_or("MOONDIAL_LAB_NETS", ""));
    world.ops_log = match env_or("MOONDIAL_OPS_LOG", "").as_str() {
        "none" => None,
        "" => Some(std::path::PathBuf::from(&data).join("ops.jsonl")),
        p => Some(std::path::PathBuf::from(p)),
    };
    if let Some(p) = &world.ops_log {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        eprintln!(
            "ops log: {} ({} lab nets from MOONDIAL_LAB_NETS={:?})",
            p.display(),
            world.lab_nets.len(),
            env_or("MOONDIAL_LAB_NETS", "")
        );
    }
    let shared = net::Shared {
        world: Arc::new(Mutex::new(world)),
        admin_token: std::env::var("MOONDIAL_ADMIN_TOKEN").ok(),
        budget: Arc::new(Mutex::new(observe::Budget::new(30, 10_000))),
        checker: Arc::new(gate::checker()),
        hour_budget: Arc::new(Mutex::new(gate::HourBudget::new(60.0, 0.25))),
        changed: tokio::sync::broadcast::channel(64).0,
        limits: moondial_query::Limits::default(),
        log_where: where_.clone(),
        cors_origins: env_or(
            "MOONDIAL_CORS_ORIGINS",
            "https://agentscanplay.com,https://www.agentscanplay.com",
        )
        .split(',')
        .map(str::to_string)
        .collect(),
    };
    let tcp = env_or("MOONDIAL_TCP", "127.0.0.1:7778");
    let http = env_or("MOONDIAL_HTTP", "127.0.0.1:7777");

    // The operator's channel: notices go to a Discord webhook from their own thread, so
    // a slow or dead webhook never holds the world.
    let notify = notify::start(
        std::env::var("MOONDIAL_DISCORD_WEBHOOK")
            .ok()
            .filter(|v| !v.is_empty()),
        &world_name,
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        let a = tokio::spawn(net::serve_tcp(shared.clone(), tcp));
        let b = tokio::spawn(net::serve_http(shared.clone(), http));
        // The scheduler: every due job, once a second.
        tokio::spawn(async move {
            let mut every = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                every.tick().await;
                let (ran, notices) = shared
                    .world
                    .lock()
                    .map(|mut w| (w.tick(), w.take_notices()))
                    .unwrap_or((0, Vec::new()));
                for n in notices {
                    let _ = notify.send(n);
                }
                if ran > 0 {
                    let _ = shared.changed.send(());
                }
            }
        });
        tokio::select! {
            _ = tokio::signal::ctrl_c() => eprintln!("shutting down"),
            _ = a => eprintln!("tcp listener stopped"),
            _ = b => eprintln!("http listener stopped"),
        }
    });
}

/// Copy a world's file log into the PostgreSQL log, once.
fn import_file_log(dir: &str, url: &str, world: &str) -> Result<u64, String> {
    let from = moondial_log::FileLog::open(dir).map_err(|e| e.to_string())?;
    let mut to = moondial_log_postgres::PostgresLog::open(url).map_err(|e| e.to_string())?;
    moondial_log_postgres::copy_world(&from, &mut to, &WorldId(world.to_string()))
        .map_err(|e| e.to_string())
}
