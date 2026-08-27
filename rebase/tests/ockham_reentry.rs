//! Ockham's re-entry, end to end (Issue #8).
//!
//! `race_conditions.rs` proves the mechanism on hand-built enhancements. This
//! suite proves the **producer path**: an Ockham run opens on ancestor `A`,
//! files each accepted prune through [`PruneLog`], and at re-entry hands the
//! bundle to the ordinary Rebase CLI together with a **freshly fetched**
//! champion `B` — never its own local incumbent.
//!
//! The four things that must hold, one test each:
//!
//! * unrelated evolution that happened during the run survives, and so does the
//!   compatible prune;
//! * a UUID the fleet has already pruned is already incorporated — no failure,
//!   and no corpus pass spent on it;
//! * a prune that cannot be reproduced on the fresh champion fails closed and
//!   leaves that champion standing;
//! * every one of those outcomes is journalled with provenance a human can
//!   follow days later.

use std::path::{Path, PathBuf};

use neat_ai_rebase::cli::{
    Cli, EXIT_IMPROVED, EXIT_INCOMPATIBLE, EXIT_NO_IMPROVEMENT, RebaseSummary, run_with,
};
use neat_ai_rebase::corpus::corpus_info;
use neat_ai_rebase::creature::{creature_checksum, validate_source_creature};
use neat_ai_rebase::enhancement::{EnhancementBundle, RemovalStrategy};
use neat_ai_rebase::fixtures::{creature, evolved_descendant, neuron, synapse};
use neat_ai_rebase::prune_log::PruneLog;
use neat_ai_rebase::scorer::{ScorerError, ScriptedScorer};
use neat_core::training_data::TrainingDataConfig;
use neat_core::{CreatureExport, creature_to_json, parse_creature_json};

const PRODUCER: &str = "neat-ai-ockham/test";

// ---------------------------------------------------------------------------
// The lineage: A is what Ockham opened on, B is where the fleet got to
// ---------------------------------------------------------------------------

/// Creature **A** — the ancestor the Ockham run opens on: `h1` (the neuron it
/// will prove is not earning its keep) and `h2`.
fn ancestor() -> CreatureExport {
    evolved_descendant(2.0, 0.5)
}

/// Creature **B** — A plus `h3`, an improvement the fleet found independently
/// while Ockham was working. This is the unrelated evolution that a stale
/// `A - h1` republish would delete.
fn fleet_champion() -> CreatureExport {
    with_hidden(&ancestor(), "h3", 0.75)
}

/// `source` plus one more hidden IDENTITY neuron reading `input-1`.
fn with_hidden(source: &CreatureExport, uuid: &str, weight: f64) -> CreatureExport {
    let mut neurons: Vec<_> = source
        .neurons
        .iter()
        .filter(|n| n.neuron_type != "output")
        .cloned()
        .collect();
    neurons.push(neuron("hidden", uuid, 0.0, Some("IDENTITY")));
    neurons.extend(
        source
            .neurons
            .iter()
            .filter(|n| n.neuron_type == "output")
            .cloned(),
    );
    let mut synapses = source.synapses.clone();
    synapses.push(synapse("input-1", uuid, weight));
    synapses.push(synapse(uuid, "output-0", 1.0));
    creature(source.input, source.output, neurons, synapses)
}

fn carries(creature: &CreatureExport, uuid: &str) -> bool {
    creature.neurons.iter().any(|n| n.uuid == uuid)
}

// ---------------------------------------------------------------------------
// Harness: a real corpus, real files, real CLI — only the scorer is scripted
// ---------------------------------------------------------------------------

struct Harness {
    _tmp: tempfile::TempDir,
    cli: Cli,
    corpus_identity: String,
    bundle_path: PathBuf,
}

impl Harness {
    /// Stage a run whose `--champion` is `champion`, freshly fetched.
    fn new(champion: &CreatureExport) -> Self {
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
        std::fs::write(&champion_path, creature_to_json(champion).unwrap()).unwrap();
        let bundle_path = tmp.path().join("ockham").join("bundle.json");
        let output_dir = tmp.path().join("out");

        Self {
            cli: Cli {
                champion: champion_path,
                enhancements: Some(bundle_path.clone()),
                harvest_from: None,
                screen_sample_rate: None,
                screen_held_out: true,
                training_data: training,
                scorer: None,
                output_dir,
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

    /// The log an Ockham run opens against `A`, on the corpus this run will be
    /// judged on.
    fn prune_log(&self) -> PruneLog {
        PruneLog::opening(PRODUCER, &ancestor(), 0.5000, &self.corpus_identity).unwrap()
    }

    fn file(&self, log: &PruneLog) {
        assert!(
            log.write_bundle(&self.bundle_path).unwrap(),
            "the run accepted prunes, so a bundle must be written"
        );
    }

    fn summary(&self) -> RebaseSummary {
        serde_json::from_str(
            &std::fs::read_to_string(self.cli.output_dir.join("rebase.json")).unwrap(),
        )
        .unwrap()
    }

    fn journal(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.cli.output_dir.join("experiments.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn published(&self) -> Option<CreatureExport> {
        let path = self.cli.output_dir.join("population-candidate.json");
        let text = std::fs::read_to_string(path).ok()?;
        Some(parse_creature_json(&text).unwrap())
    }

    fn champion_bytes(&self) -> String {
        std::fs::read_to_string(&self.cli.champion).unwrap()
    }
}

/// A scorer that fails if it is asked anything at all — the way to assert that
/// a run spent no corpus pass.
fn never_called() -> ScriptedScorer {
    ScriptedScorer::flat(0.5).failing(ScorerError::Spawn(
        "the scorer must not be invoked when there is nothing to score".into(),
    ))
}

// ---------------------------------------------------------------------------
// 1. The race: unrelated evolution and the compatible prune both survive
// ---------------------------------------------------------------------------

#[test]
fn a_prune_filed_on_a_replays_onto_the_fresh_champion_and_keeps_the_fleets_work() {
    let champion = fleet_champion();
    let h = Harness::new(&champion);
    let mut log = h.prune_log();
    log.accept("h1", RemovalStrategy::MeanAblation { mean: 0.25 }, 0.5100)
        .unwrap();
    h.file(&log);
    let champion_before = h.champion_bytes();

    let scorer = ScriptedScorer::flat(0.80).with("single-00", 0.85);
    assert_eq!(run_with(&h.cli, Some(&scorer)).unwrap(), EXIT_IMPROVED);

    let published = h.published().expect("an improvement was emitted");
    validate_source_creature(&published).unwrap();
    assert!(!carries(&published, "h1"), "the prune replayed");
    assert!(
        carries(&published, "h3"),
        "the fleet's unrelated improvement must survive the prune"
    );
    assert!(carries(&published, "h2"), "and so must the rest of B");

    // Re-entry used the fresh champion, not Ockham's local incumbent: the
    // summary's opening ancestor and the champion it built on are different
    // creatures, and the ancestor is the one the log recorded at open.
    let summary = h.summary();
    assert_eq!(summary.status, "improved");
    assert_eq!(summary.opening_checksum, log.base_checksum());
    assert_eq!(
        summary.opening_checksum,
        creature_checksum(&ancestor()).unwrap()
    );
    assert_eq!(
        summary.champion_checksum,
        creature_checksum(&champion).unwrap()
    );
    assert_ne!(summary.opening_checksum, summary.champion_checksum);
    assert_eq!(summary.producer, PRODUCER);
    assert_eq!(h.champion_bytes(), champion_before, "inputs are read-only");
}

// ---------------------------------------------------------------------------
// 2. A UUID the fleet has already pruned
// ---------------------------------------------------------------------------

#[test]
fn a_uuid_the_fleet_already_pruned_is_already_incorporated_and_costs_nothing() {
    // The fleet pruned `h1` itself while Ockham was proving the same thing.
    let champion = creature(
        2,
        1,
        vec![
            neuron("hidden", "h2", 0.0, Some("IDENTITY")),
            neuron("hidden", "h3", 0.0, Some("IDENTITY")),
            neuron("output", "output-0", 0.0, Some("IDENTITY")),
        ],
        vec![
            synapse("input-1", "h2", 0.5),
            synapse("input-1", "h3", 0.75),
            synapse("h2", "output-0", 1.0),
            synapse("h3", "output-0", 1.0),
        ],
    );
    let h = Harness::new(&champion);
    let mut log = h.prune_log();
    log.accept("h1", RemovalStrategy::MeanAblation { mean: 0.25 }, 0.5100)
        .unwrap();
    h.file(&log);

    // No candidate is built, so the scorer is never asked — an already-absent
    // UUID must cost neither a failure nor a corpus pass.
    assert_eq!(
        run_with(&h.cli, Some(&never_called())).unwrap(),
        EXIT_NO_IMPROVEMENT
    );
    let summary = h.summary();
    assert_eq!(summary.status, "nothingToDo");
    assert_eq!(summary.enhancements[0].outcome, "alreadyPresent");
    assert!(summary.enhancements[0].reason.is_none());
    assert_eq!(
        summary.candidates.len(),
        1,
        "only the baseline: no duplicate work"
    );
    assert!(summary.verdict.is_none());
    assert!(h.published().is_none());
}

// ---------------------------------------------------------------------------
// 3. A prune that cannot be reproduced on the fresh champion
// ---------------------------------------------------------------------------

#[test]
fn a_conflicting_prune_fails_closed_and_the_fresh_champion_stands() {
    // Ockham accepted an exact IDENTITY collapse of `h1`. While it ran, the
    // fleet retrained `h1` to TANH, so the recorded transformation cannot be
    // reproduced — and substituting the approximate one is not on offer.
    let mut champion = fleet_champion();
    champion
        .neurons
        .iter_mut()
        .find(|n| n.uuid == "h1")
        .unwrap()
        .squash = Some("TANH".into());
    let h = Harness::new(&champion);
    let mut log = h.prune_log();
    log.accept("h1", RemovalStrategy::IdentityCollapse, 0.5100)
        .unwrap();
    h.file(&log);
    let champion_before = h.champion_bytes();

    assert_eq!(
        run_with(&h.cli, Some(&never_called())).unwrap(),
        EXIT_INCOMPATIBLE
    );
    let summary = h.summary();
    assert_eq!(summary.status, "incompatible");
    assert_eq!(summary.enhancements[0].outcome, "incompatible");
    assert!(
        summary.enhancements[0]
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("not IDENTITY"),
        "the refusal must name what it could not reproduce: {summary:?}"
    );
    assert!(
        h.published().is_none(),
        "a prune that cannot be replayed must never replace the champion"
    );
    assert_eq!(h.champion_bytes(), champion_before);
}

#[test]
fn a_conflicting_prune_does_not_take_the_compatible_one_with_it() {
    let mut champion = fleet_champion();
    champion
        .neurons
        .iter_mut()
        .find(|n| n.uuid == "h1")
        .unwrap()
        .squash = Some("TANH".into());
    let h = Harness::new(&champion);
    let mut log = h.prune_log();
    log.accept("h1", RemovalStrategy::IdentityCollapse, 0.5100)
        .unwrap();
    log.accept("h2", RemovalStrategy::MeanAblation { mean: 0.125 }, 0.5150)
        .unwrap();
    h.file(&log);

    let scorer = ScriptedScorer::flat(0.80).with("single-00", 0.84);
    assert_eq!(run_with(&h.cli, Some(&scorer)).unwrap(), EXIT_IMPROVED);

    let summary = h.summary();
    assert_eq!(summary.enhancements[0].outcome, "incompatible");
    assert_eq!(summary.enhancements[1].outcome, "applied");
    let published = h.published().expect("the compatible prune still applies");
    validate_source_creature(&published).unwrap();
    assert!(!carries(&published, "h2"), "the compatible prune replayed");
    assert!(
        carries(&published, "h1"),
        "the refused prune must leave its target alone"
    );
    assert!(carries(&published, "h3"), "and the fleet's work stands");
}

// ---------------------------------------------------------------------------
// 4. Provenance
// ---------------------------------------------------------------------------

#[test]
fn the_filed_bundle_carries_the_opening_checksum_score_and_corpus_identity() {
    let h = Harness::new(&fleet_champion());
    let mut log = h.prune_log();
    log.accept("h1", RemovalStrategy::MeanAblation { mean: 0.25 }, 0.5100)
        .unwrap();
    log.accept("h2", RemovalStrategy::IdentityCollapse, 0.5150)
        .unwrap();
    h.file(&log);

    let text = std::fs::read_to_string(&h.bundle_path).unwrap();
    let bundle = EnhancementBundle::parse_json(&text).unwrap();
    assert_eq!(bundle.producer, PRODUCER);
    assert_eq!(
        bundle.base_checksum,
        creature_checksum(&ancestor()).unwrap()
    );
    assert!((bundle.base_score - 0.5000).abs() < 1e-12);
    assert_eq!(bundle.corpus_identity, h.corpus_identity);
    assert_eq!(bundle.enhancements.len(), 2);
    for e in &bundle.enhancements {
        assert_eq!(e.payload.kind(), "ockhamRemoval");
        assert!(e.id_is_consistent());
        assert_eq!(e.meta.corpus_identity, h.corpus_identity);
        assert_eq!(e.meta.base_checksum, bundle.base_checksum);
    }
}

#[test]
fn every_outcome_is_journalled_with_provenance() {
    let champion = fleet_champion();
    let h = Harness::new(&champion);
    let mut log = h.prune_log();
    log.accept("h1", RemovalStrategy::MeanAblation { mean: 0.25 }, 0.5100)
        .unwrap();
    h.file(&log);
    let filed_id = log.enhancements()[0].meta.id.clone();

    let scorer = ScriptedScorer::flat(0.80).with("single-00", 0.85);
    assert_eq!(run_with(&h.cli, Some(&scorer)).unwrap(), EXIT_IMPROVED);

    let journal = h.journal();
    let record = |name: &str| -> serde_json::Value {
        journal
            .iter()
            .find(|v| v["record"] == name)
            .unwrap_or_else(|| panic!("no `{name}` record in {journal:?}"))
            .clone()
    };

    // The four facts an unattended host has to be able to grep for: the opening
    // ancestor, the fresh champion, what happened to the prune, and the verdict.
    let opening = record("opening");
    assert_eq!(opening["producer"], PRODUCER);
    assert_eq!(opening["openingChecksum"], log.base_checksum());
    assert_eq!(
        opening["championChecksum"],
        creature_checksum(&champion).unwrap()
    );
    assert_eq!(opening["corpusIdentity"], h.corpus_identity);

    let enhancement = record("enhancement");
    assert_eq!(enhancement["id"], filed_id);
    assert_eq!(enhancement["kind"], "ockhamRemoval");
    assert_eq!(enhancement["producer"], PRODUCER);
    assert_eq!(enhancement["outcome"], "applied");
    assert!((enhancement["claimedGain"].as_f64().unwrap() - 0.01).abs() < 1e-9);

    let verdict = record("verdict");
    assert_eq!(verdict["championChecksum"], opening["championChecksum"]);
    assert_eq!(verdict["mode"], "full");

    let result = record("result");
    assert_eq!(result["status"], "improved");
    let emitted = result["emittedChecksum"].as_str().unwrap();
    assert_eq!(
        emitted,
        neat_ai_rebase::creature::sha256_hex(
            std::fs::read_to_string(h.cli.output_dir.join("population-candidate.json"))
                .unwrap()
                .as_bytes()
        ),
        "the journal names the bytes that were actually published"
    );
}

/// The scoring inputs are kept for diagnosis, and the champion is among them:
/// a verdict without an authoritatively scored baseline is not a verdict.
#[test]
fn the_champion_is_scored_authoritatively_alongside_the_rebased_candidates() {
    let h = Harness::new(&fleet_champion());
    let mut log = h.prune_log();
    log.accept("h1", RemovalStrategy::MeanAblation { mean: 0.25 }, 0.5100)
        .unwrap();
    log.accept("h2", RemovalStrategy::IdentityCollapse, 0.5150)
        .unwrap();
    h.file(&log);

    let scorer = ScriptedScorer::flat(0.80)
        .with("single-00", 0.83)
        .with("single-01", 0.81)
        .with("bundle", 0.90);
    assert_eq!(run_with(&h.cli, Some(&scorer)).unwrap(), EXIT_IMPROVED);

    let verdict = h.summary().verdict.expect("scoring ran");
    assert!((verdict.baseline.score - 0.80).abs() < 1e-12);
    assert_eq!(verdict.champion_checksum, h.summary().champion_checksum);
    assert!(verdict.improved());
    let winner = verdict.winner.unwrap();
    assert_eq!(winner.label, "bundle");
    assert_eq!(winner.applied_ids.len(), 2);

    let staged: Vec<String> = std::fs::read_dir(h.cli.output_dir.join("scoring"))
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|e| {
            Path::new(&e.file_name())
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .collect();
    assert!(staged.contains(&"baseline".to_string()), "{staged:?}");
    assert!(staged.contains(&"bundle".to_string()), "{staged:?}");
}
