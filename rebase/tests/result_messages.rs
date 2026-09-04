//! The result vocabulary a rebase reports in (Issue #80).
//!
//! Three scores are in play and each is measured against a different baseline:
//! what the producer **claimed** on its own opening creature, what the
//! authoritative scorer **validated** for the champion this run was judged
//! against, and what the **rebased** candidate finally scored. A message that
//! reports two of those deltas without naming their baselines reads as a
//! contradiction — "declined" and "improved" in the same sentence — so every
//! delta here names, or is bracketed by, the number it was taken from.

use std::path::{Path, PathBuf};

use neat_ai_rebase::cli::{Cli, EXIT_IMPROVED, EXIT_NO_IMPROVEMENT, run_with};
use neat_ai_rebase::corpus::corpus_info;
use neat_ai_rebase::enhancement::{Enhancement, EnhancementBundle, Payload, ProducerContext};
use neat_ai_rebase::fixtures::evolved_descendant;
use neat_ai_rebase::message::{
    NoImprovement, RebaseStamp, SourceScore, no_improvement_message, rebase_message,
};
use neat_ai_rebase::patch::{Node, Patch, Provenance};
use neat_ai_rebase::scorer::ScriptedScorer;
use neat_ai_rebase::tags::CreatureMeta;
use neat_core::creature_to_json;
use neat_core::training_data::TrainingDataConfig;

/// What the producer claimed for the creature it filed the enhancements from.
const CLAIMED: f64 = 0.6;
/// Longest a result message may be and still sit in a commit subject.
const SUBJECT_BUDGET: usize = 180;

/// Wording no result message may ever use for a claim/validation mismatch: it
/// says the creature got worse, when what actually happened is that two
/// different baselines were compared.
const BANNED: [&str; 2] = ["declined", "decline"];

fn assert_reads_cleanly(message: &str) {
    for word in BANNED {
        assert!(
            !message.to_lowercase().contains(word),
            "`{word}` describes a creature getting worse, not a claim delta: {message}"
        );
    }
    assert!(
        message.chars().count() <= SUBJECT_BUDGET,
        "{} chars is too long for a commit subject: {message}",
        message.chars().count()
    );
}

/// The downstream case from Issue #80: the champion validated 0.0015 below what
/// the producer claimed, and the replay still added 0.000344 on top of it.
#[test]
fn a_validation_below_the_claim_is_a_claim_delta_not_a_decline() {
    let message = rebase_message(&RebaseStamp {
        score: 0.419751,
        error: 0.580249,
        champion_score: 0.419407,
        source_score: SourceScore::Claimed(0.421251),
        applied: 2,
        label: "bundle",
        source: "neat-ai-forests",
    });

    assert!(
        message.contains("champion 0.419407 → rebased 0.419751"),
        "the rebase delta names both ends: {message}"
    );
    assert!(message.contains("(+3.44e-4)"), "{message}");
    assert!(
        message.contains("claim delta -1.50e-3 vs claimed 0.421251"),
        "the mismatch against the claim is named as such: {message}"
    );
    assert_reads_cleanly(&message);
}

/// The other direction: the authoritative pass measured *more* than the
/// producer claimed. Same wording, opposite sign — the reader is never left
/// guessing which way a bare number went.
#[test]
fn a_validation_above_the_claim_is_reported_against_the_claim_too() {
    let message = rebase_message(&RebaseStamp {
        score: 0.5,
        error: 0.5,
        champion_score: 0.4,
        source_score: SourceScore::Claimed(0.45),
        applied: 1,
        label: "single-00",
        source: "neat-ai-forests",
    });

    assert!(
        message.contains("1 enhancement from neat-ai-forests"),
        "{message}"
    );
    assert!(
        message.contains("claim delta +5.00e-2 vs claimed 0.450000"),
        "{message}"
    );
    assert_reads_cleanly(&message);
}

/// The gain the rebase itself produced is always shown as champion → rebased,
/// so it can never be mistaken for the claim delta beside it.
#[test]
fn a_positive_rebase_delta_is_shown_as_champion_to_rebased() {
    let message = rebase_message(&RebaseStamp {
        score: 0.5,
        error: 0.5,
        champion_score: 0.4,
        source_score: SourceScore::Claimed(0.45),
        applied: 3,
        label: "bundle",
        source: "harvest",
    });

    assert!(
        message.starts_with("🪢 Rebase applied · 3 enhancements from harvest"),
        "{message}"
    );
    assert!(
        message.contains("champion 0.400000 → rebased 0.500000 (+1.00e-1)"),
        "{message}"
    );
    assert_reads_cleanly(&message);
}

/// An attempted rebase that wins nothing says so, and says which champion
/// stood — a run that reports only a number cannot be told from one that
/// promoted something.
#[test]
fn an_attempted_rebase_that_wins_nothing_names_the_champion_that_held() {
    let message = no_improvement_message(&NoImprovement {
        champion_score: 0.5,
        best_score: Some(0.49),
        source_score: SourceScore::Claimed(0.6),
        attempted: 2,
        source: "neat-ai-forests",
    });

    assert!(
        message.starts_with("🪢 Rebase not applied · 2 enhancements from neat-ai-forests"),
        "{message}"
    );
    assert!(message.contains("champion 0.500000 held"), "{message}");
    assert!(
        message.contains("best candidate 0.490000 (-1.00e-2)"),
        "{message}"
    );
    assert!(
        message.contains("claim delta -1.10e-1 vs claimed 0.600000"),
        "{message}"
    );
    assert_reads_cleanly(&message);
}

/// Absent is not zero: a verdict that scored no candidate at all says that,
/// rather than reporting a `0.000000` best candidate nobody measured.
#[test]
fn a_verdict_with_no_candidate_scored_says_so_rather_than_inventing_one() {
    let message = no_improvement_message(&NoImprovement {
        champion_score: 0.5,
        best_score: None,
        source_score: SourceScore::Claimed(0.6),
        attempted: 1,
        source: "harvest",
    });

    assert!(message.contains("no candidate scored"), "{message}");
    assert!(!message.contains("0.000000"), "{message}");
    assert_reads_cleanly(&message);
}

// ---------------------------------------------------------------------------
// End to end: the message the CLI actually records
// ---------------------------------------------------------------------------

struct Harness {
    _tmp: tempfile::TempDir,
    cli: Cli,
    corpus_identity: String,
    bundle_path: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let training = tmp.path().join("training");
        std::fs::create_dir_all(&training).unwrap();
        let mut bytes = Vec::new();
        for record in 0..32u32 {
            for slot in 0..3 {
                bytes.extend_from_slice(&((record as f32) * 0.05 + slot as f32).to_le_bytes());
            }
        }
        std::fs::write(training.join("corpus.bin"), bytes).unwrap();
        let corpus = corpus_info(&training, &TrainingDataConfig::new(2, 1)).unwrap();

        let champion_path = tmp.path().join("champion.json");
        std::fs::write(
            &champion_path,
            creature_to_json(&evolved_descendant(2.0, 0.5)).unwrap(),
        )
        .unwrap();
        let bundle_path = tmp.path().join("forests").join("enhancements.json");

        Self {
            cli: Cli {
                command: None,
                champion: Some(champion_path),
                enhancements: Some(bundle_path.clone()),
                harvest_from: None,
                screen_sample_rate: None,
                screen_held_out: true,
                training_data: Some(training),
                scorer: None,
                output_dir: Some(tmp.path().join("out")),
                scorer_args: Vec::new(),
                min_improvement: 1e-9,
                max_candidates: 8,
                dry_run: false,
            },
            corpus_identity: corpus.identity,
            bundle_path,
            _tmp: tmp,
        }
    }

    fn out(&self) -> &Path {
        self.cli
            .output_dir
            .as_deref()
            .expect("the harness always sets an output directory")
    }

    fn file_one_patch(&self) {
        let enhancement = Enhancement::new(
            Payload::ForestPatch {
                patch: Patch::new(0, Node::stump(1, 0.5, 0.0, 0.25), Provenance::default()),
            },
            &ProducerContext {
                producer: "neat-ai-forests/test".into(),
                base_checksum: "opening-checksum".into(),
                base_score: 0.5,
                improved_score: CLAIMED,
                corpus_identity: self.corpus_identity.clone(),
                input_count: 2,
                output_count: 1,
            },
        );
        std::fs::create_dir_all(self.bundle_path.parent().unwrap()).unwrap();
        let bundle = EnhancementBundle::from_enhancements(vec![enhancement]);
        std::fs::write(
            &self.bundle_path,
            serde_json::to_string_pretty(&bundle).unwrap(),
        )
        .unwrap();
    }

    /// The `detail` the run's `result` record carried, which is where an
    /// unattended reader finds what the run decided and why.
    fn result_detail(&self) -> String {
        std::fs::read_to_string(self.out().join("experiments.jsonl"))
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| v["record"] == "result")
            .expect("every run journals a result")["detail"]
            .as_str()
            .expect("the result record carries the message a human reads")
            .to_string()
    }
}

#[test]
fn a_promoted_candidate_is_journalled_and_tagged_with_the_same_message() {
    let h = Harness::new();
    h.file_one_patch();

    let scorer = ScriptedScorer::flat(0.50).with("single-00", 0.60);
    assert_eq!(run_with(&h.cli, Some(&scorer)).unwrap(), EXIT_IMPROVED);

    let detail = h.result_detail();
    assert!(
        detail.starts_with("🪢 Rebase applied · 1 enhancement"),
        "{detail}"
    );
    assert!(
        detail.contains("champion 0.500000 → rebased 0.600000 (+1.00e-1)"),
        "{detail}"
    );
    // The producer claimed 0.6 and the rebase matched it exactly, so the claim
    // delta is zero — and is still reported, because a silently absent delta
    // reads as "not measured".
    assert!(detail.contains("vs claimed 0.600000"), "{detail}");
    assert_reads_cleanly(&detail);

    let emitted = std::fs::read_to_string(h.out().join("population-candidate.json")).unwrap();
    let tagged = CreatureMeta::from_json(&emitted);
    assert_eq!(
        tagged.get("rebase"),
        Some(detail.as_str()),
        "the tag and the journal tell the same story"
    );
}

#[test]
fn an_unpromoted_run_journals_why_the_champion_held() {
    let h = Harness::new();
    h.file_one_patch();

    let scorer = ScriptedScorer::flat(0.80).with("single-00", 0.70);
    assert_eq!(
        run_with(&h.cli, Some(&scorer)).unwrap(),
        EXIT_NO_IMPROVEMENT
    );

    let detail = h.result_detail();
    assert!(
        detail.starts_with("🪢 Rebase not applied · 1 enhancement"),
        "{detail}"
    );
    assert!(detail.contains("champion 0.800000 held"), "{detail}");
    assert!(
        detail.contains("best candidate 0.700000 (-1.00e-1)"),
        "{detail}"
    );
    assert_reads_cleanly(&detail);
}
