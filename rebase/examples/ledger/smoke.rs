//! Reproducible synthetic inputs; these are not independent evolved lineages.

use super::manifest::{Artifact, write_new};
use neat_ai_rebase::corpus::corpus_info;
use neat_ai_rebase::creature::creature_checksum;
use neat_ai_rebase::enhancement::{Enhancement, Payload, ProducerContext};
use neat_ai_rebase::fixtures::linear_hidden_creature;
use neat_ai_rebase::patch::{Node, Patch, Provenance};
use neat_core::training_data::TrainingDataConfig;
use serde_json::json;
use std::path::Path;
use std::time::Instant;

pub(crate) fn prepare(output: &Path, scorer: &Path) -> Result<(), String> {
    let started = Instant::now();
    std::fs::create_dir(output)
        .map_err(|e| format!("new smoke directory {}: {e}", output.display()))?;
    let output = output.canonicalize().map_err(|e| e.to_string())?;
    let scorer = Artifact::pin(scorer)?;
    for (name, offset) in [("training", 0.0_f32), ("calibration", 100.0_f32)] {
        let dir = output.join(name);
        std::fs::create_dir(&dir).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        for index in 0..4096 {
            let x = offset + (index as f32 + 0.5) / 4096.0;
            let y = 2.0 * x + if x > 0.5 { 0.02 } else { 0.0 };
            for value in [x, 0.25, y] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        std::fs::write(dir.join("data.bin"), bytes).map_err(|e| e.to_string())?;
    }
    let training = output.join("training");
    let corpus = corpus_info(&training, &TrainingDataConfig::new(2, 1))?;
    let mut parents = Vec::new();
    for index in 0..3 {
        let weight = 1.98 + index as f64 * 0.01;
        let root = linear_hidden_creature(weight - 0.5);
        let parent = linear_hidden_creature(weight);
        let path = output.join(format!("parent-{index}.json"));
        std::fs::write(
            &path,
            neat_core::creature_to_json(&parent).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let root_path = output.join(format!("root-{index}.json"));
        std::fs::write(
            &root_path,
            neat_core::creature_to_json(&root).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let history = output.join(format!("history-{index}.json"));
        write_new(
            &history,
            &json!({ "completeToRoot": true, "establishedBeforeValidation": true,
            "source": format!("hand-built smoke fixture {index}: root weight increased by 0.5; not evolved provenance"),
            "seed": index, "ancestors": [creature_checksum(&root)?, creature_checksum(&parent)?] }),
        )?;
        parents.push(json!({"creature": path, "provenance": history}));
    }
    let context = ProducerContext {
        producer: "ledger-smoke-fixture".into(),
        base_checksum: creature_checksum(&linear_hidden_creature(1.98))?,
        base_score: 0.0,
        improved_score: 0.0,
        corpus_identity: corpus.identity,
        input_count: 2,
        output_count: 1,
    };
    let mut patches = Vec::new();
    for (name, value) in [("positive", 0.02), ("negative", -0.02)] {
        let path = output.join(format!("{name}.json"));
        let enhancement = Enhancement::new(
            Payload::ForestPatch {
                patch: Patch::new(0, Node::stump(0, 0.5, 0.0, value), Provenance::default()),
            },
            &context,
        );
        write_new(&path, &enhancement)?;
        patches.push(path);
    }
    let calibration_report = output.join("calibration-report.json");
    write_new(
        &calibration_report,
        &json!({ "sampleCount":256, "minImprovement":1e-8,
        "validationUsed":false, "seconds":0.0,
        "rationale":"fixed smoke settings; no calibration study or optimisation inference" }),
    )?;
    write_new(
        &output.join("draft.json"),
        &json!({ "version":1, "evidenceClass":"smoke",
        "training":training, "calibrationTraining":output.join("calibration"),
        "calibrationReport":calibration_report, "parents":parents,
        "patches":[patches[0], patches[1], patches[0], patches[1]], "seeds":[41,42,43,44],
        "sampleCount":256, "minImprovement":1e-8, "scorer":scorer.path,
        "scorerArgs":["--gpu=off","--cost=MSE"], "evaluationTimeoutSeconds":10,
        "budget":{"calls":32,"seconds":120.0},
        "success":{"minimumPairedRuns":4,"minimumRateGain":0.01,"maximumLostWins":0},
        "preparationSeconds":started.elapsed().as_secs_f64() }),
    )
}
