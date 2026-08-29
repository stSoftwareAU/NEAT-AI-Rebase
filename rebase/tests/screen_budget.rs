//! What the screen is allowed to cost, and what it is allowed to veto
//! (Issue #42).
//!
//! A live GRQ run screened three enhancements into a cohort of six, invoked
//! with `--max-candidates 8`, and published nothing: the whole cohort already
//! fitted the authoritative budget, so the screen could not save a corpus pass
//! — it could only discard information, and it discarded all of it.
//!
//! Two rules come out of that, one test each:
//!
//! * the screen does not engage when the cohort already fits
//!   `--max-candidates`; and
//! * a candidate the stratum cannot resolve is **undecided**, carried to the
//!   authoritative pass rather than vetoed by a test weaker than the one that
//!   admitted it.
//!
//! The scorer here answers a stratum and the corpus differently, which is the
//! whole point: a graft fires on a subset of records, so a 5% stratum that
//! contains none of them reports the baseline exactly while the full corpus
//! sees the gain.
//!
//! That difference is also why a phase has to journal what it *measured*
//! (Issue #43): the two shapes above are the same survivor count and opposite
//! diagnoses, and the staging directory is deleted on the way out. The last two
//! tests here pin the numbers a phase leaves behind.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use neat_ai_rebase::cli::{Cli, EXIT_IMPROVED, RebaseSummary, run_with};
use neat_ai_rebase::corpus::corpus_info;
use neat_ai_rebase::enhancement::{Enhancement, EnhancementBundle, Payload, ProducerContext};
use neat_ai_rebase::fixtures::evolved_descendant;
use neat_ai_rebase::journal::SCREEN_RECORD;
use neat_ai_rebase::patch::{Node, Patch, Provenance};
use neat_ai_rebase::report::read_one;
use neat_ai_rebase::scorer::{
    DirectoryScorer, ScoreResult, ScorerError, ScorerMode, ScriptedScorer,
};
use neat_core::creature_to_json;
use neat_core::training_data::TrainingDataConfig;

const PRODUCER: &str = "neat-ai-forests/test";
/// What every candidate and the champion score on the stratum used below.
const STRATUM_BASELINE: f64 = 0.50;

// ---------------------------------------------------------------------------
// A scorer that answers the stratum and the corpus differently
// ---------------------------------------------------------------------------

/// Two scripted scorers behind one trait: one for the screen's sampled calls,
/// one for the authoritative pass.
struct ByMode {
    sample: ScriptedScorer,
    full: ScriptedScorer,
}

impl DirectoryScorer for ByMode {
    fn score_directory(
        &self,
        creature_dir: &Path,
        training_dir: &Path,
        mode: ScorerMode,
    ) -> Result<BTreeMap<String, ScoreResult>, ScorerError> {
        match mode {
            ScorerMode::Full => self.full.score_directory(creature_dir, training_dir, mode),
            ScorerMode::Sample { .. } => {
                self.sample
                    .score_directory(creature_dir, training_dir, mode)
            }
        }
    }

    fn identity(&self) -> String {
        "by-mode-scorer".into()
    }
}

// ---------------------------------------------------------------------------
// Harness: real corpus, real files, real CLI
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
                screen_sample_rate: Some(0.05),
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

    /// One graft, filed against the corpus this run is judged on.
    fn patch(&self, feature: usize, right: f32) -> Enhancement {
        Enhancement::new(
            Payload::ForestPatch {
                patch: Patch::new(
                    0,
                    Node::stump(feature, 0.5, 0.0, right),
                    Provenance::default(),
                ),
            },
            &ProducerContext {
                producer: PRODUCER.into(),
                base_checksum: "opening-checksum".into(),
                base_score: 0.5,
                improved_score: 0.6,
                corpus_identity: self.corpus_identity.clone(),
                input_count: 2,
                output_count: 1,
            },
        )
    }

    fn file(&self, enhancements: Vec<Enhancement>) {
        std::fs::create_dir_all(self.bundle_path.parent().unwrap()).unwrap();
        let bundle = EnhancementBundle::from_enhancements(enhancements);
        std::fs::write(
            &self.bundle_path,
            serde_json::to_string_pretty(&bundle).unwrap(),
        )
        .unwrap();
    }

    fn summary(&self) -> RebaseSummary {
        serde_json::from_str(&std::fs::read_to_string(self.out().join("rebase.json")).unwrap())
            .unwrap()
    }

    /// Every line the run journalled, so a test can ask whether it screened.
    fn journal_lines(&self) -> Vec<String> {
        std::fs::read_to_string(self.out().join("experiments.jsonl"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The screen records the run journalled, one per phase, in order.
    fn screen_records(&self) -> Vec<serde_json::Value> {
        self.journal_lines()
            .iter()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["record"] == SCREEN_RECORD)
            .collect()
    }

    fn labels_scored(&self) -> Vec<String> {
        self.summary()
            .candidates
            .iter()
            .map(|c| c.label.clone())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// 1. A cohort that fits the budget is handed straight to the corpus
// ---------------------------------------------------------------------------

/// The observed run: everything ties the baseline on the stratum, so the old
/// screen kept nothing and the run published nothing — while the corpus pass it
/// was going to pay for anyway would have found the winner.
#[test]
fn a_cohort_that_fits_the_budget_is_never_screened() {
    let h = Harness::new();
    let good = h.patch(0, 0.25);
    let dud = h.patch(1, -0.10);
    h.file(vec![good.clone(), dud]);

    // Two enhancements build baseline + bundle + two singles: three candidates
    // against a budget of eight. Nothing to save.
    let scorer = ByMode {
        // The stratum resolves nothing at all.
        sample: ScriptedScorer::flat(STRATUM_BASELINE),
        full: ScriptedScorer::flat(STRATUM_BASELINE)
            .with("single-00", 0.60)
            .with("single-01", 0.30)
            .with("bundle", 0.45),
    };
    assert_eq!(run_with(&h.cli, Some(&scorer)).unwrap(), EXIT_IMPROVED);

    assert!(
        h.screen_records().is_empty(),
        "no screen phase may be journalled: {:?}",
        h.journal_lines()
    );
    assert_eq!(
        read_one(&h.out().join("experiments.jsonl"))
            .unwrap()
            .screen
            .screened_runs,
        0,
        "a skipped screen is not a screened run"
    );
    assert_eq!(
        h.labels_scored(),
        vec!["baseline", "bundle", "single-00", "single-01"],
        "the whole cohort reaches the authoritative pass"
    );
    assert!(
        h.out().join("population-candidate.json").exists(),
        "the corpus found the winner the stratum could not see"
    );
    let winner = h.summary().verdict.unwrap().winner.unwrap();
    assert_eq!(winner.applied_ids, vec![good.meta.id]);
}

// ---------------------------------------------------------------------------
// 2. A cohort that overflows the budget is screened — one-sidedly
// ---------------------------------------------------------------------------

/// The perverse case: a graft whose firing records miss the stratum scores the
/// baseline *exactly*, so a strict `>` vetoed it. It is undecided, not failed,
/// and the authoritative pass is what decides.
#[test]
fn a_candidate_the_stratum_cannot_resolve_survives_the_screen() {
    let h = Harness::new();
    let mut cli = h.cli.clone();
    // A budget the cohort cannot fit, so the screen genuinely saves a pass.
    cli.max_candidates = 2;
    let invisible = h.patch(0, 0.25);
    let loser = h.patch(1, -0.10);
    h.file(vec![invisible.clone(), loser.clone()]);

    let scorer = ByMode {
        // `single-00` fires on records the stratum does not hold: it reports the
        // baseline exactly. `single-01` is a loss the stratum can see.
        sample: ScriptedScorer::flat(STRATUM_BASELINE).with("single-01", 0.40),
        full: ScriptedScorer::flat(STRATUM_BASELINE)
            .with("single-00", 0.60)
            .with("single-01", 0.30),
    };
    assert_eq!(run_with(&cli, Some(&scorer)).unwrap(), EXIT_IMPROVED);

    let summary: RebaseSummary =
        serde_json::from_str(&std::fs::read_to_string(h.out().join("rebase.json")).unwrap())
            .unwrap();
    let scored: Vec<&str> = summary
        .candidates
        .iter()
        .map(|c| c.label.as_str())
        .collect();
    assert_eq!(
        scored,
        vec!["baseline", "single-00"],
        "the undecided candidate is carried forward and the visible loser is not: {scored:?}"
    );
    let winner = summary.verdict.unwrap().winner.unwrap();
    assert_eq!(
        winner.applied_ids,
        vec![invisible.meta.id],
        "the corpus promoted what the stratum could not resolve"
    );
    assert_ne!(winner.applied_ids, vec![loser.meta.id]);
}

/// The screen still earns its keep: what the stratum can see losing never
/// reaches the corpus, and a screen that sees every candidate lose still
/// publishes nothing.
#[test]
fn a_loss_the_stratum_can_see_is_still_screened_out() {
    let h = Harness::new();
    let mut cli = h.cli.clone();
    cli.max_candidates = 2;
    h.file(vec![h.patch(0, 0.25), h.patch(1, -0.10)]);

    let scorer = ByMode {
        sample: ScriptedScorer::flat(STRATUM_BASELINE)
            .with("single-00", 0.30)
            .with("single-01", 0.30),
        // The corpus would have promoted `single-00` — it is never asked.
        full: ScriptedScorer::flat(STRATUM_BASELINE).with("single-00", 0.90),
    };
    assert_eq!(
        run_with(&cli, Some(&scorer)).unwrap(),
        neat_ai_rebase::cli::EXIT_NO_IMPROVEMENT
    );
    assert!(!h.out().join("population-candidate.json").exists());
    assert_eq!(h.summary().status, "nothingToDo");
    assert!(
        !h.screen_records().is_empty(),
        "the screen ran, and said so"
    );
}

// ---------------------------------------------------------------------------
// 3. What a phase leaves behind (Issue #43)
// ---------------------------------------------------------------------------

/// The failure the issue opens with: three patches, `kept 0 of 3`, and no way
/// to tell a working screen from a blind one. A stratum that resolves nothing
/// has to say so in the journal — every delta exactly zero, every verdict
/// `indistinguishable` — and the stratum's own size has to be there too, so its
/// power is checkable once the staging directory is gone.
#[test]
fn a_stratum_that_resolved_nothing_journals_zero_deltas_not_a_bare_count() {
    let h = Harness::new();
    let mut cli = h.cli.clone();
    cli.max_candidates = 2;
    let patches: Vec<Enhancement> = [(0, 0.25), (1, -0.10), (0, 0.40)]
        .iter()
        .map(|(feature, right)| h.patch(*feature, *right))
        .collect();
    let ids: Vec<String> = patches.iter().map(|e| e.meta.id.clone()).collect();
    h.file(patches);

    let scorer = ByMode {
        // The stratum holds none of the records these grafts fire on.
        sample: ScriptedScorer::flat(STRATUM_BASELINE),
        full: ScriptedScorer::flat(STRATUM_BASELINE).with("single-00", 0.60),
    };
    assert_eq!(run_with(&cli, Some(&scorer)).unwrap(), EXIT_IMPROVED);

    let records = h.screen_records();
    assert_eq!(records.len(), 2, "both strata journalled: {records:?}");
    for record in &records {
        assert_eq!(record["sampleRate"], 0.05);
        assert_eq!(record["baselineScore"], STRATUM_BASELINE);
        assert_eq!(
            record["recordCount"], 1000,
            "the stratum's own size, so its power is checkable: {record}"
        );
        assert_eq!(record["kept"], 3, "nothing visible, so nothing eliminated");
        let measured = record["enhancements"].as_array().unwrap();
        assert_eq!(measured.len(), 3);
        for entry in measured {
            assert_eq!(entry["delta"], 0.0);
            assert_eq!(
                entry["verdict"], "indistinguishable",
                "a stratum that saw nothing is not a stratum that saw a loss"
            );
            assert_eq!(entry["kept"], true);
            assert_eq!(entry["producer"], PRODUCER);
        }
        let journalled: Vec<&str> = measured.iter().map(|e| e["id"].as_str().unwrap()).collect();
        for id in &ids {
            assert!(journalled.contains(&id.as_str()), "{id} missing: {record}");
        }
    }
}

/// The other explanation of the same survivor count: the stratum *did* see the
/// losses. Each delta is signed and negative, and each verdict is `worse` — the
/// one verdict that eliminates.
#[test]
fn a_loss_the_stratum_could_see_journals_the_signed_delta_that_killed_it() {
    let h = Harness::new();
    let mut cli = h.cli.clone();
    cli.max_candidates = 2;
    h.file(
        [(0, 0.25), (1, -0.10), (0, 0.40)]
            .iter()
            .map(|(feature, right)| h.patch(*feature, *right))
            .collect(),
    );

    let scorer = ByMode {
        // A loss of 3e-4 — the shape a 5% stratum is powered for.
        sample: ScriptedScorer::flat(STRATUM_BASELINE - 3e-4).with("baseline", STRATUM_BASELINE),
        // Never asked: the screen kept nothing.
        full: ScriptedScorer::flat(STRATUM_BASELINE).with("single-00", 0.90),
    };
    assert_eq!(
        run_with(&cli, Some(&scorer)).unwrap(),
        neat_ai_rebase::cli::EXIT_NO_IMPROVEMENT
    );

    let records = h.screen_records();
    assert_eq!(records.len(), 1, "phase 0 killed everything: {records:?}");
    assert_eq!(records[0]["kept"], 0);
    let measured = records[0]["enhancements"].as_array().unwrap();
    assert_eq!(measured.len(), 3);
    for entry in measured {
        let delta = entry["delta"].as_f64().unwrap();
        assert!((delta + 3e-4).abs() < 1e-12, "{entry}");
        assert_eq!(entry["verdict"], "worse");
        assert_eq!(entry["kept"], false);
    }
}
