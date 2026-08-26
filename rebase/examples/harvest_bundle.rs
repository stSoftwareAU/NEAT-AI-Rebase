//! Recover the enhancement bundle a Forests run would have filed, from the
//! creature it published.
//!
//! ```text
//! cargo run --release --example harvest_bundle -- \
//!     <source.json> <target.json> <corpus-identity> <out-bundle.json>
//! ```
//!
//! `source` is the creature carrying the discoveries (the Forests output),
//! `target` is the champion they will be rebased onto. Only the patches the
//! target lacks are recovered; a patch whose reconstruction does not hash back
//! to its own id is reported and discarded.

use neat_ai_rebase::creature::creature_checksum;
use neat_ai_rebase::enhancement::EnhancementBundle;
use neat_ai_rebase::harvest::{harvest_delta, patch_ids};
use neat_core::parse_creature_json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [source_path, target_path, corpus_identity, out_path] = args.as_slice() else {
        eprintln!("usage: harvest_bundle <source.json> <target.json> <corpus-identity> <out.json>");
        std::process::exit(2);
    };

    let source = parse_creature_json(&std::fs::read_to_string(source_path)?)?;
    let target = parse_creature_json(&std::fs::read_to_string(target_path)?)?;

    println!("source {source_path}");
    println!("  checksum {}", creature_checksum(&source)?);
    println!(
        "  {} neurons, {} synapses, {} forest patches",
        source.neurons.len(),
        source.synapses.len(),
        patch_ids(&source).len()
    );
    println!("target {target_path}");
    println!("  checksum {}", creature_checksum(&target)?);
    println!(
        "  {} neurons, {} synapses, {} forest patches",
        target.neurons.len(),
        target.synapses.len(),
        patch_ids(&target).len()
    );

    let harvest = harvest_delta(&source, &target);
    println!(
        "\nharvested {} patches, skipped {}",
        harvest.patches.len(),
        harvest.skipped.len()
    );
    for skip in &harvest.skipped {
        println!("  SKIP {} — {}", skip.id, skip.reason);
    }
    for patch in &harvest.patches {
        println!(
            "  OK   {} output-{} depth {} splits {} features {:?}",
            patch.id(),
            patch.output,
            patch.root.depth(),
            patch.root.split_count(),
            patch.root.features()
        );
    }
    if harvest.patches.is_empty() {
        println!("\nnothing to rebase");
        return Ok(());
    }

    let enhancements = harvest.into_enhancements(
        &source,
        corpus_identity,
        "harvest/neat-ai-rebase",
        f64::NAN, // no measurement was made; the CLI never reads it
    )?;
    // A harvest measures nothing, so record 0.0 rather than a NaN that would
    // look like a number in the journal.
    let mut enhancements = enhancements;
    for e in &mut enhancements {
        e.meta.base_score = 0.0;
        e.meta.improved_score = 0.0;
    }
    let bundle = EnhancementBundle::from_enhancements(enhancements);
    std::fs::write(out_path, serde_json::to_string_pretty(&bundle)?)?;
    println!(
        "\nwrote {out_path} ({} enhancements)",
        bundle.enhancements.len()
    );
    Ok(())
}
