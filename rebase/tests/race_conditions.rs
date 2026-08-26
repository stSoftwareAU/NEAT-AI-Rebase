//! Race-condition and interaction suite (Issue #9).
//!
//! # The regression this suite exists to prevent
//!
//! An optimiser opens on champion **A**, spends 45–60 minutes finding a real
//! improvement **Δ**, and finishes. By then the fleet has evolved to champion
//! **B**, which contains improvements the optimiser never saw. The obvious
//! thing to publish is `A + Δ` — and it is the wrong thing, because it silently
//! deletes everything that made B better than A.
//!
//! Repeat that across a fleet of optimisers and the population converges on
//! whichever process happens to finish last. Every other lineage is discarded
//! on each re-entry, the population stops carrying diverse improvements, and
//! the fleet ends up a **monoculture** descended from one search process —
//! having thrown away exactly the concurrent discoveries that made running
//! several different optimisers worthwhile in the first place.
//!
//! Rebase's answer is to publish `B + Δ`, and only when the scorer confirms it
//! beats B. So the tests below all have the same shape: build A, evolve it to
//! B *independently*, replay Δ, and check that
//!
//! * the unrelated evolution in B survives, and
//! * nothing is published that the scorer did not prefer over B.
//!
//! **If a future refactor makes one of these tests awkward, that is the alarm,
//! not the inconvenience.** The cheap way to make them pass is to republish the
//! stale descendant, which is the bug.

use std::collections::BTreeMap;
use std::path::Path;

use neat_ai_rebase::adapter::Application;
use neat_ai_rebase::creature::{creature_checksum, validate_source_creature};
use neat_ai_rebase::engine::{EnhancementOutcome, RebaseOutcome, RebaseRequest, rebase};
use neat_ai_rebase::enhancement::{
    Enhancement, EnhancementBundle, OckhamRemoval, Payload, ProducerContext, RemovalStrategy,
};
use neat_ai_rebase::fixtures::{
    creature, evolved_descendant, linear_hidden_creature, neuron, synapse,
};
use neat_ai_rebase::patch::{Node, Patch, Provenance};
use neat_ai_rebase::scorer::{
    DirectoryScorer, ScorerError, ScorerMode, ScriptedScorer, Verdict, judge,
};
use neat_core::{CreatureExport, creature_to_json};

const CORPUS: &str = "corpus-race";

// ---------------------------------------------------------------------------
// Fixtures: the A / B lineage
// ---------------------------------------------------------------------------

/// Creature **A** — the stale ancestor an optimiser opens on.
fn ancestor() -> CreatureExport {
    linear_hidden_creature(2.0)
}

/// Creature **B** — A plus an improvement the fleet found independently
/// (`h2`, reading `input-1`), while the optimiser was still working on A.
fn fleet_champion() -> CreatureExport {
    evolved_descendant(2.0, 0.5)
}

fn forest_patch(feature: usize, right: f32) -> Patch {
    Patch::new(
        0,
        Node::stump(feature, 0.5, 0.0, right),
        Provenance::default(),
    )
}

/// A Forest enhancement discovered on A, with A's checksum recorded.
fn forest_delta(feature: usize, right: f32) -> Enhancement {
    Enhancement::new(
        Payload::ForestPatch {
            patch: forest_patch(feature, right),
        },
        &ProducerContext {
            producer: "neat-ai-forests/test".into(),
            base_checksum: creature_checksum(&ancestor()).unwrap(),
            base_score: 0.5000,
            improved_score: 0.5100,
            corpus_identity: CORPUS.into(),
            input_count: 2,
            output_count: 1,
        },
    )
}

/// An Ockham removal discovered on A.
fn ockham_delta(uuid: &str) -> Enhancement {
    Enhancement::new(
        Payload::OckhamRemoval {
            removal: OckhamRemoval {
                neuron_uuid: uuid.into(),
                strategy: RemovalStrategy::MeanAblation { mean: 0.5 },
            },
        },
        &ProducerContext {
            producer: "neat-ai-ockham/test".into(),
            base_checksum: creature_checksum(&ancestor()).unwrap(),
            base_score: 0.5000,
            improved_score: 0.5050,
            corpus_identity: CORPUS.into(),
            input_count: 2,
            output_count: 1,
        },
    )
}

fn cohort(champion: &CreatureExport, enhancements: &[Enhancement]) -> RebaseOutcome {
    rebase(&RebaseRequest {
        champion,
        enhancements,
        corpus_identity: CORPUS,
        max_candidates: 0,
    })
    .expect("the engine builds a cohort")
}

fn decide(scorer: &dyn DirectoryScorer, outcome: &RebaseOutcome) -> Result<Verdict, ScorerError> {
    let tmp = tempfile::tempdir().unwrap();
    judge(
        scorer,
        outcome,
        tmp.path(),
        &tmp.path().join("staging"),
        1e-9,
        ScorerMode::Full,
    )
}

fn winner_creature<'a>(
    verdict: &Verdict,
    outcome: &'a RebaseOutcome,
) -> Option<&'a CreatureExport> {
    let winner = verdict.winner.as_ref()?;
    outcome
        .cohort
        .iter()
        .find(|c| c.label == winner.label)
        .map(|c| &c.creature)
}

/// The unrelated fleet improvement, as it appears in B.
fn carries_fleet_improvement(creature: &CreatureExport) -> bool {
    creature.neurons.iter().any(|n| n.uuid == "h2")
        && creature
            .synapses
            .iter()
            .any(|s| s.from_uuid == "h2" && s.to_uuid == "output-0")
}

// ---------------------------------------------------------------------------
// Forest races
// ---------------------------------------------------------------------------

#[test]
fn patch_improves_a_and_also_b_so_the_winner_is_b_plus_patch() {
    let champion = fleet_champion();
    let delta = forest_delta(1, 0.25);
    let outcome = cohort(&champion, &[delta]);
    assert_eq!(outcome.reports[0].outcome, EnhancementOutcome::Applied);

    let scorer = ScriptedScorer::flat(0.80).with("single-00", 0.85);
    let verdict = decide(&scorer, &outcome).unwrap();
    assert!(verdict.improved());

    let published = winner_creature(&verdict, &outcome).unwrap();
    validate_source_creature(published).unwrap();
    assert!(
        carries_fleet_improvement(published),
        "publishing must not discard the improvement the fleet made while the optimiser ran"
    );
    // And it is genuinely B + Δ, not A + Δ.
    assert!(
        published
            .neurons
            .iter()
            .any(|n| n.uuid.starts_with("forest-")),
        "the discovery must survive too"
    );
}

#[test]
fn patch_improves_a_but_hurts_b_so_b_remains_champion() {
    let champion = fleet_champion();
    let delta = forest_delta(1, 0.25);
    // The producer measured a real gain on A…
    assert!(delta.meta.claimed_gain() > 0.0);
    let outcome = cohort(&champion, &[delta]);

    // …and on B the same change is worse.
    let scorer = ScriptedScorer::flat(0.80).with("single-00", 0.75);
    let verdict = decide(&scorer, &outcome).unwrap();
    assert!(
        !verdict.improved(),
        "a gain proven on an ancestor is evidence, never permission"
    );
    assert!(verdict.candidates[0].delta < 0.0);
}

#[test]
fn a_champion_that_already_includes_the_patch_gets_no_duplicate_structure() {
    let champion = fleet_champion();
    let delta = forest_delta(1, 0.25);

    // Someone — this host, another host, or the fleet — already published it.
    let first = cohort(&champion, std::slice::from_ref(&delta));
    let already = first.cohort[1].creature.clone();
    let neurons_before = already.neurons.len();
    let synapses_before = already.synapses.len();

    let second = cohort(&already, &[delta]);
    assert_eq!(
        second.reports[0].outcome,
        EnhancementOutcome::AlreadyPresent
    );
    assert!(second.is_empty(), "nothing left to score");
    assert_eq!(already.neurons.len(), neurons_before);
    assert_eq!(already.synapses.len(), synapses_before);
}

// ---------------------------------------------------------------------------
// Ockham races
// ---------------------------------------------------------------------------

#[test]
fn prune_proven_on_a_replays_onto_b_and_keeps_the_unrelated_improvement() {
    // B still carries `h1`, the neuron Ockham proved was not earning its keep,
    // and it also carries `h2`, which the fleet added meanwhile.
    let champion = fleet_champion();
    assert!(champion.neurons.iter().any(|n| n.uuid == "h1"));

    let outcome = cohort(&champion, &[ockham_delta("h1")]);
    assert_eq!(outcome.reports[0].outcome, EnhancementOutcome::Applied);

    let scorer = ScriptedScorer::flat(0.80).with("single-00", 0.83);
    let verdict = decide(&scorer, &outcome).unwrap();
    assert!(verdict.improved());

    let published = winner_creature(&verdict, &outcome).unwrap();
    validate_source_creature(published).unwrap();
    assert!(!published.neurons.iter().any(|n| n.uuid == "h1"));
    assert!(
        carries_fleet_improvement(published),
        "the prune must not take the fleet's unrelated improvement with it"
    );
}

#[test]
fn a_uuid_b_has_already_removed_is_a_no_op() {
    // The fleet pruned `h1` on its own while Ockham was working.
    let champion = creature(
        2,
        1,
        vec![
            neuron("hidden", "h2", 0.0, Some("IDENTITY")),
            neuron("output", "output-0", 0.0, Some("IDENTITY")),
        ],
        vec![
            synapse("input-1", "h2", 0.5),
            synapse("h2", "output-0", 1.0),
        ],
    );
    let outcome = cohort(&champion, &[ockham_delta("h1")]);
    assert_eq!(
        outcome.reports[0].outcome,
        EnhancementOutcome::AlreadyPresent
    );
    assert!(outcome.is_empty());
    assert_eq!(outcome.cohort.len(), 1, "only the baseline");
}

// ---------------------------------------------------------------------------
// Interactions between enhancements
// ---------------------------------------------------------------------------

#[test]
fn two_individually_good_enhancements_that_conflict_lose_to_the_best_verified_subset() {
    let champion = fleet_champion();
    let a = forest_delta(0, 0.25);
    let b = forest_delta(1, -0.10);
    let outcome = cohort(&champion, &[a, b]);
    assert_eq!(outcome.candidates().count(), 3, "bundle + two singles");

    // Each helps alone; together they overcorrect and land below the champion.
    let scorer = ScriptedScorer::flat(0.80)
        .with("single-00", 0.84)
        .with("single-01", 0.82)
        .with("bundle", 0.78);
    let verdict = decide(&scorer, &outcome).unwrap();
    assert!(verdict.improved());
    let winner = verdict.winner.clone().unwrap();
    assert_eq!(
        winner.label, "single-00",
        "the scorer picks the best verified subset, not the biggest bundle"
    );
    // The losing combination is still recorded, so the interaction is visible.
    assert!(
        verdict
            .candidates
            .iter()
            .any(|c| c.label == "bundle" && c.delta < 0.0)
    );
}

#[test]
fn multiple_compatible_enhancements_compound() {
    let champion = fleet_champion();
    let a = forest_delta(0, 0.25);
    let b = forest_delta(1, -0.10);
    let c = ockham_delta("h1");
    let outcome = cohort(&champion, &[a, b, c]);

    let scorer = ScriptedScorer::flat(0.80)
        .with("single-00", 0.81)
        .with("single-01", 0.81)
        .with("single-02", 0.81)
        .with("prefix-02", 0.83)
        .with("bundle", 0.90);
    let verdict = decide(&scorer, &outcome).unwrap();
    let winner = verdict.winner.clone().unwrap();
    assert_eq!(winner.label, "bundle");
    assert_eq!(winner.applied_ids.len(), 3);

    let published = winner_creature(&verdict, &outcome).unwrap();
    validate_source_creature(published).unwrap();
    assert!(carries_fleet_improvement(published));
}

// ---------------------------------------------------------------------------
// Fail-closed
// ---------------------------------------------------------------------------

#[test]
fn corpus_identity_mismatch_fails_closed() {
    let champion = fleet_champion();
    let outcome = rebase(&RebaseRequest {
        champion: &champion,
        enhancements: &[forest_delta(1, 0.25)],
        corpus_identity: "a-completely-different-corpus",
        max_candidates: 0,
    })
    .unwrap();
    assert!(matches!(
        outcome.reports[0].outcome,
        EnhancementOutcome::Incompatible(_)
    ));
    assert!(
        outcome.is_empty(),
        "nothing may be built on a foreign corpus"
    );
}

#[test]
fn a_scorer_failure_never_produces_a_population_candidate() {
    let champion = fleet_champion();
    let outcome = cohort(&champion, &[forest_delta(1, 0.25)]);
    for failure in [
        ScorerError::Spawn("no such binary".into()),
        ScorerError::Failed {
            status: "exit status: 137".into(),
            stderr: "killed".into(),
        },
        ScorerError::Malformed("not json".into()),
        ScorerError::MissingBaseline,
    ] {
        let scorer = ScriptedScorer::flat(0.5).failing(failure);
        assert!(
            decide(&scorer, &outcome).is_err(),
            "every scorer failure must fail closed"
        );
    }
}

#[test]
fn the_champion_and_the_enhancement_artefacts_are_byte_for_byte_unchanged() {
    let champion = fleet_champion();
    let champion_before = creature_to_json(&champion).unwrap();
    let enhancements = vec![
        forest_delta(0, 0.25),
        forest_delta(1, -0.10),
        ockham_delta("h1"),
    ];
    let bundle_before =
        serde_json::to_string(&EnhancementBundle::from_enhancements(enhancements.clone())).unwrap();

    let outcome = cohort(&champion, &enhancements);
    let scorer = ScriptedScorer::flat(0.80).with("bundle", 0.90);
    decide(&scorer, &outcome).unwrap();

    assert_eq!(creature_to_json(&champion).unwrap(), champion_before);
    assert_eq!(
        serde_json::to_string(&EnhancementBundle::from_enhancements(enhancements)).unwrap(),
        bundle_before
    );
}

// ---------------------------------------------------------------------------
// The full path, on real files
// ---------------------------------------------------------------------------

/// End to end over the real CLI: real creature JSON on disk, a real `.bin`
/// corpus and its computed identity, the real engine and the real staging /
/// emission path — only the scorer is scripted.
#[test]
fn the_cli_publishes_b_plus_delta_and_leaves_its_inputs_alone() {
    use neat_ai_rebase::cli::{Cli, EXIT_IMPROVED, run_with};
    use neat_ai_rebase::corpus::corpus_info;
    use neat_core::training_data::TrainingDataConfig;

    let tmp = tempfile::tempdir().unwrap();
    let champion = fleet_champion();

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
    std::fs::write(&champion_path, creature_to_json(&champion).unwrap()).unwrap();

    // Re-file the enhancement against the corpus the CLI will compute.
    let mut delta = forest_delta(1, 0.25);
    delta.meta.corpus_identity = corpus.identity.clone();
    let bundle_path = tmp.path().join("bundle.json");
    let bundle_text =
        serde_json::to_string_pretty(&EnhancementBundle::from_enhancements(vec![delta])).unwrap();
    std::fs::write(&bundle_path, &bundle_text).unwrap();

    let champion_text_before = std::fs::read_to_string(&champion_path).unwrap();
    let output_dir = tmp.path().join("out");
    let cli = Cli {
        champion: champion_path.clone(),
        enhancements: Some(bundle_path.clone()),
        harvest_from: None,
        screen_sample_rate: None,
        screen_held_out: true,
        training_data: training,
        scorer: None,
        output_dir: output_dir.clone(),
        scorer_args: Vec::new(),
        min_improvement: 1e-9,
        max_candidates: 8,
        dry_run: false,
    };
    let scorer = ScriptedScorer::flat(0.80).with("single-00", 0.85);
    assert_eq!(run_with(&cli, Some(&scorer)).unwrap(), EXIT_IMPROVED);

    let emitted = output_dir.join("population-candidate.json");
    let published: CreatureExport =
        neat_core::parse_creature_json(&std::fs::read_to_string(&emitted).unwrap()).unwrap();
    validate_source_creature(&published).unwrap();
    assert!(carries_fleet_improvement(&published));
    assert!(
        published
            .neurons
            .iter()
            .any(|n| n.uuid.starts_with("forest-"))
    );

    // The inputs are untouched.
    assert_eq!(
        std::fs::read_to_string(&champion_path).unwrap(),
        champion_text_before
    );
    assert_eq!(std::fs::read_to_string(&bundle_path).unwrap(), bundle_text);

    // The journal names the ancestor, the fresh champion and the verdict.
    let journal = std::fs::read_to_string(output_dir.join("experiments.jsonl")).unwrap();
    assert!(journal.contains(r#""record":"opening""#), "{journal}");
    assert!(journal.contains(r#""record":"verdict""#), "{journal}");
    assert!(journal.contains(r#""status":"improved""#), "{journal}");
}

/// The adapters really are no-ops on an already-incorporated enhancement, at
/// the level below the engine — so a producer can ask the question cheaply
/// before deciding to rebase at all.
#[test]
fn adapters_report_already_present_without_constructing_anything() {
    let champion = fleet_champion();
    let delta = forest_delta(1, 0.25);
    let outcome = cohort(&champion, std::slice::from_ref(&delta));
    let grafted = outcome.cohort[1].creature.clone();

    assert!(neat_ai_rebase::adapter::is_present(&delta, &grafted));
    let target = neat_ai_rebase::compat::Target::new(&grafted, CORPUS);
    assert_eq!(
        neat_ai_rebase::adapter::apply(&delta, &target).unwrap(),
        Application::AlreadyPresent
    );
}

/// A cohort that is entirely already-present still produces a readable
/// verdict-free outcome rather than an error — an unattended host must be able
/// to tell "nothing to do" from "something broke".
#[test]
fn nothing_to_do_is_distinguishable_from_a_failure() {
    let champion = fleet_champion();
    let outcome = cohort(&champion, &[ockham_delta("never-existed")]);
    assert!(outcome.is_empty());
    assert!(outcome.combination_failures.is_empty());
    assert_eq!(
        outcome.reports[0].outcome,
        EnhancementOutcome::AlreadyPresent
    );
}

/// A scripted scorer that scores a directory it was never given still has to
/// name the baseline — the guard that keeps a stale result set from being read
/// as a verdict.
#[test]
fn a_verdict_always_carries_the_baseline_explicitly() {
    let champion = fleet_champion();
    let outcome = cohort(&champion, &[forest_delta(1, 0.25)]);
    let scorer = ScriptedScorer::flat(0.80).with("single-00", 0.85);
    let verdict = decide(&scorer, &outcome).unwrap();
    assert_eq!(verdict.champion_checksum, outcome.champion_checksum);
    assert!((verdict.baseline.score - 0.80).abs() < 1e-12);
    assert_eq!(verdict.mode, "full");

    let losing = ScriptedScorer::flat(0.80).with("single-00", 0.10);
    let verdict = decide(&losing, &outcome).unwrap();
    assert!(!verdict.improved());
    assert!(
        (verdict.baseline.score - 0.80).abs() < 1e-12,
        "the baseline is recorded whatever the outcome"
    );
}

/// The scorer sees exactly the creatures the cohort holds — no extra file, no
/// missing one — because a directory pass scores whatever is in the directory.
#[test]
fn the_staging_directory_holds_exactly_the_cohort() {
    struct Counting;
    impl DirectoryScorer for Counting {
        fn score_directory(
            &self,
            creature_dir: &Path,
            _training_dir: &Path,
            _mode: ScorerMode,
        ) -> Result<BTreeMap<String, neat_ai_rebase::scorer::ScoreResult>, ScorerError> {
            ScriptedScorer::flat(0.5).score_directory(
                creature_dir,
                Path::new("."),
                ScorerMode::Full,
            )
        }
        fn identity(&self) -> String {
            "counting".into()
        }
    }

    let champion = fleet_champion();
    let outcome = cohort(&champion, &[forest_delta(0, 0.25), forest_delta(1, -0.1)]);
    let tmp = tempfile::tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let verdict = judge(
        &Counting,
        &outcome,
        tmp.path(),
        &staging,
        1e-9,
        ScorerMode::Full,
    )
    .unwrap();
    assert_eq!(verdict.candidates.len(), outcome.candidates().count());
    let staged: Vec<String> = std::fs::read_dir(&staging)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|e| {
            e.path()
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .collect();
    assert_eq!(staged.len(), outcome.cohort.len());
    for candidate in &outcome.cohort {
        assert!(staged.contains(&candidate.label), "{staged:?}");
    }
}
