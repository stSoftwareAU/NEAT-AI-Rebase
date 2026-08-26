//! Write a champion and an enhancement bundle for a manual end-to-end run.
//!
//! `cargo run --example make_fixture -- <dir>`

use neat_ai_rebase::corpus::corpus_info;
use neat_ai_rebase::creature::creature_checksum;
use neat_ai_rebase::enhancement::{Enhancement, EnhancementBundle, Payload, ProducerContext};
use neat_ai_rebase::fixtures::evolved_descendant;
use neat_ai_rebase::patch::{Node, Patch, Provenance};
use neat_core::training_data::TrainingDataConfig;

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("usage: <dir>"));
    let champion = evolved_descendant(2.0, 0.5);
    std::fs::write(
        dir.join("champion.json"),
        neat_core::creature_to_json_pretty(&champion).unwrap(),
    )
    .unwrap();

    let corpus = corpus_info(&dir.join("training"), &TrainingDataConfig::new(2, 1)).unwrap();
    let context = ProducerContext {
        producer: "neat-ai-forests/0.1.17".into(),
        base_checksum: creature_checksum(&champion).unwrap(),
        base_score: 0.800_000,
        improved_score: 0.801_000,
        corpus_identity: corpus.identity.clone(),
        input_count: 2,
        output_count: 1,
    };
    let bundle = EnhancementBundle::from_enhancements(vec![
        Enhancement::new(
            Payload::ForestPatch {
                patch: Patch::new(0, Node::stump(0, 0.5, 0.0, 0.02), Provenance::default()),
            },
            &context,
        ),
        Enhancement::new(
            Payload::ForestPatch {
                patch: Patch::new(0, Node::stump(1, 0.25, 0.0, -0.01), Provenance::default()),
            },
            &context,
        ),
    ]);
    std::fs::write(
        dir.join("bundle.json"),
        serde_json::to_string_pretty(&bundle).unwrap(),
    )
    .unwrap();
    println!("corpus identity {}", corpus.identity);
}
