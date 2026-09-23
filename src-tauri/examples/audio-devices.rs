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
