//! The operator's channel. Notices (an arrival worth knowing about) are posted to a
//! Discord webhook from a thread of their own; the world never waits on the network.
//! Without a webhook they go to stderr, so a local run still shows them.

use std::sync::mpsc;

/// Start the notifier; returns the sender the tick loop feeds.
pub fn start(webhook: Option<String>, world: &str) -> mpsc::Sender<String> {
    let (tx, rx) = mpsc::channel::<String>();
    let world = world.to_string();
    std::thread::Builder::new()
        .name("moondial-notify".into())
        .spawn(move || {
            for line in rx {
                match &webhook {
                    Some(url) => post(url, &world, &line),
                    None => eprintln!("notice: {line}"),
                }
            }
        })
        .expect("notifier thread");
    tx
}

fn post(url: &str, world: &str, line: &str) {
    let body = serde_json::json!({
        "username": format!("{world} (the world)"),
        "content": line,
        "allowed_mentions": {"parse": []},
    });
    match ureq::post(url)
        .set("content-type", "application/json")
        .timeout(std::time::Duration::from_secs(10))
        .send_string(&body.to_string())
    {
        Ok(_) => {}
        Err(ureq::Error::Status(code, _)) => eprintln!("notice: webhook answered {code}: {line}"),
        Err(e) => eprintln!("notice: webhook failed: {e}: {line}"),
    }
}
