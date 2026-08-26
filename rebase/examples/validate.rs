//! Check creature JSON against the shared NEAT-AI-core contract.
//!
//! ```text
//! cargo run --release --example validate -- <creature.json> [more.json …]
//! ```
//!
//! Exactly the gate a rebased candidate has to clear before it is scored, so a
//! creature that passes here is one Rebase would build on and one the fleet
//! should accept. Exits non-zero if any file fails.
//!
//! Two published fleet samples currently fail it, which is how this example
//! came to exist.

use neat_ai_rebase::creature::{creature_checksum, validate_source_creature};
use neat_ai_rebase::harvest::patch_ids;
use neat_ai_rebase::tags::CreatureMeta;
use neat_core::parse_creature_json;

fn main() -> std::process::ExitCode {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: validate <creature.json> [more.json …]");
        return std::process::ExitCode::from(2);
    }
    let mut failed = 0u8;
    for path in &paths {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                println!("FAIL {path}: {e}");
                failed = 1;
                continue;
            }
        };
        let creature = match parse_creature_json(&text) {
            Ok(c) => c,
            Err(e) => {
                println!("FAIL {path}: does not parse — {e}");
                failed = 1;
                continue;
            }
        };
        match validate_source_creature(&creature) {
            Ok(()) => {
                let meta = CreatureMeta::from_json(&text);
                println!(
                    "OK   {path}\n     {} neurons, {} synapses, {} patches, {} tags, score tag {}\n     checksum {}",
                    creature.neurons.len(),
                    creature.synapses.len(),
                    patch_ids(&creature).len(),
                    meta.tags.len(),
                    meta.score().map_or("none".into(), |s| format!("{s:.12}")),
                    creature_checksum(&creature).unwrap_or_default(),
                );
            }
            Err(e) => {
                println!("FAIL {path}: {e}");
                failed = 1;
            }
        }
    }
    std::process::ExitCode::from(failed)
}
