// The diagnostic tool reports the same message names as the app itself.
// It takes both modules whole and uses a few functions of each.
#![allow(dead_code)]
#[path = "../src/message.rs"]
mod message;
#[path = "../src/audio.rs"]
mod audio;
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = if args.is_empty() {
        audio::devices().and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string()))
    } else if (args.len() == 3 || args.len() == 4) && args[0] == "volume" {
        let level = args.get(3).map(|v| v.parse::<u8>()).transpose();
        level
            .map_err(|e| e.to_string())
            .and_then(|level| audio::volume(&args[1], &args[2], level, None))
            .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string()))
    } else if args.len() == 3 && args[0] == "peak" {
        audio::peak(&args[1], &args[2])
            .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string()))
    } else if args.len() == 3 && args[0] == "resolve" {
        // The endpoint a choice stands for now, by the engine's rule.
        audio::resolve(&args[1], &args[2])
            .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string()))
    } else if args.len() == 2 && args[0] == "watch" {
        // The active endpoints looked at every 50 ms for the seconds given:
        // a line, with the time, whenever one comes or goes. For measuring
        // how long after a device is plugged in Windows lists it, and how
        // long after that the engine takes it (an ad-hoc measurement, not a
        // test).
        watch(args[1].parse().unwrap_or(60))
    } else if (args.len() == 3 || args.len() == 4) && args[0] == "calibrate" {
        audio::calibrate_aec(
            &args[1],
            &args[2],
            args.get(3).is_some_and(|v| v == "careful"),
        )
        .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string()))
    } else {
        Err("Invalid device command".into())
    };
    match result {
        Ok(value) => println!("{value}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
fn watch(seconds: u64) -> Result<String, String> {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    let now_ms = || SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let listing = || -> BTreeMap<(String, String), String> {
        audio::devices().unwrap_or_default().into_iter().map(|d| ((d.kind, d.id), d.name)).collect()
    };
    let entry = |((kind, id), name): (&(String, String), &String)| serde_json::json!({"kind": kind, "id": id, "name": name});
    let mut known = listing();
    println!("{}", serde_json::json!({"at_ms": now_ms(), "listed": known.iter().map(entry).collect::<Vec<_>>()}));
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
        let current = listing();
        let added: Vec<_> = current.iter().filter(|(key, _)| !known.contains_key(*key)).map(entry).collect();
        let removed: Vec<_> = known.iter().filter(|(key, _)| !current.contains_key(*key)).map(entry).collect();
        if !added.is_empty() || !removed.is_empty() {
            println!("{}", serde_json::json!({"at_ms": now_ms(), "added": added, "removed": removed}));
            known = current;
        }
    }
    Ok(serde_json::json!({"at_ms": now_ms(), "done": true}).to_string())
}
