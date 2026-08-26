//! Print the documented example bundle, so `docs/enhancement-format.md` shows
//! real bytes rather than bytes someone believed were real.
//!
//! `cargo run --example print_bundle`

use neat_ai_rebase::enhancement::{
    Enhancement, EnhancementBundle, OckhamRemoval, Payload, ProducerContext, RemovalStrategy,
};
use neat_ai_rebase::patch::{Node, Patch, Provenance};

fn main() {
    let context = ProducerContext {
        producer: "neat-ai-forests/0.1.17".into(),
        base_checksum: "9a2f7c4e18b0d35a6f1c9e2b7d4a8305c6e1f0b9d2a7c4e18b0d35a6f1c9e2b7".into(),
        base_score: 0.812_340,
        improved_score: 0.812_905,
        corpus_identity: "3f2a1b0c9d8e7f65".into(),
        input_count: 42,
        output_count: 1,
    };
    let forest = Enhancement::new(
        Payload::ForestPatch {
            patch: Patch::new(
                0,
                Node::stump(17, 0.25, 0.0, 0.011),
                Provenance {
                    strategy: "histogram-stump".into(),
                    backend: "cpu".into(),
                    predicted_gain: 4.271,
                    affected_records: 18_204,
                    search_records: 250_000,
                    incumbent_checksum: context.base_checksum.clone(),
                    seed: None,
                    notes: vec![],
                },
            ),
        },
        &context,
    );
    let ockham = Enhancement::new(
        Payload::OckhamRemoval {
            removal: OckhamRemoval {
                neuron_uuid: "b7c1f0d2-3e4a-4b5c-8d9e-0f1a2b3c4d5e".into(),
                strategy: RemovalStrategy::MeanAblation { mean: 0.031_25 },
            },
        },
        &ProducerContext {
            producer: "neat-ai-ockham/0.1.12".into(),
            improved_score: 0.812_601,
            ..context
        },
    );

    println!("--- single forest enhancement ---");
    println!("{}", serde_json::to_string_pretty(&forest).unwrap());
    println!("--- single ockham enhancement ---");
    println!("{}", serde_json::to_string_pretty(&ockham).unwrap());
    println!("--- bundle ---");
    println!(
        "{}",
        serde_json::to_string_pretty(&EnhancementBundle::from_enhancements(vec![forest, ockham]))
            .unwrap()
    );
}
