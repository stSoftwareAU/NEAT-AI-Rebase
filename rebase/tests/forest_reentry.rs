//! Forests' re-entry, end to end (Issue #36).
//!
//! `ockham_reentry.rs` proves the producer path for removals. This suite proves
//! it for **grafts**: a Forests run opens on ancestor `A`, files each locally
//! accepted patch through [`PatchLog`] at the moment it is accepted, and at
//! re-entry hands the bundle to the ordinary Rebase CLI together with a
//! **freshly fetched** champion `B`.
//!
//! It exists because the alternative — harvesting the patches back out of the
//! published creature — can only see what survived, cannot recover the
//! producer's own scores, and infers an order rather than recording one. What
//! is filed here has nothing to reconstruct.
//!
//! The acceptance criteria, one test each:
//!
//! * every locally accepted patch is in the bundle, in acceptance order, with
//!   the exact payload bytes that were accepted;
//! * a filed patch's id is the id the graft uses, so a champion that already
//!   carries it is still recognised as carrying it;
//! * the bundle's `corpusIdentity` is the identity Rebase computes from the
//!   same corpus;
//! * a run that accepted nothing writes no bundle, and the caller can tell.

use std::path::PathBuf;

use neat_ai_rebase::cli::{Cli, EXIT_IMPROVED, EXIT_NO_IMPROVEMENT, RebaseSummary, run_with};
use neat_ai_rebase::corpus::corpus_info;
use neat_ai_rebase::creature::{creature_checksum, validate_source_creature};
use neat_ai_rebase::enhancement::{EnhancementBundle, Payload};
use neat_ai_rebase::fixtures::{creature, evolved_descendant, neuron, synapse};
use neat_ai_rebase::patch::{Node, Patch, Provenance};
use neat_ai_rebase::patch_log::PatchLog;
use neat_ai_rebase::scorer::{ScorerError, ScriptedScorer};
use neat_core::training_data::TrainingDataConfig;
use neat_core::{CreatureExport, creature_to_json, parse_creature_json};

const PRODUCER: &str = "neat-ai-forests/test";

// ---------------------------------------------------------------------------
// The lineage: A is what Forests opened on, B is where the fleet got to
// ---------------------------------------------------------------------------

/// Creature **A** — the ancestor the Forests run opens on.
fn ancestor() -> CreatureExport {
    evolved_descendant(2.0, 0.5)
}

/// Creature **B** — A plus `h3`, an improvement the fleet found independently
/// while Forests was searching. A stale `A + Δ` republish would delete it.
fn fleet_champion() -> CreatureExport {
    let source = ancestor();
    let mut neurons: Vec<_> = source
        .neurons
        .iter()
        .filter(|n| n.neuron_type != "output")
        .cloned()
        .collect();
    neurons.push(neuron("hidden", "h3", 0.0, Some("IDENTITY")));
    neurons.extend(
        source
            .neurons
            .iter()
            .filter(|n| n.neuron_type == "output")
            .cloned(),
    );
    let mut synapses = source.synapses.clone();
    synapses.push(synapse("input-1", "h3", 0.75));
    synapses.push(synapse("h3", "output-0", 1.0));
    creature(source.input, source.output, neurons, synapses)
}

/// The first patch a run accepts: a stump on `input-0`.
fn first_patch() -> Patch {
    Patch::new(
        0,
        Node::stump(0, 0.25, 0.0, 0.01),
        Provenance {
            strategy: "histogram-stump".into(),
            backend: "cpu".into(),
            predicted_gain: 1.5,
            affected_records: 100,
            search_records: 1000,
            incumbent_checksum: "abc".into(),
            seed: Some(7),
            notes: vec!["sampled".into()],
        },
    )
}

/// The second: a stump on `input-1`, so the two are distinct enhancements.
fn second_patch() -> Patch {
    Patch::new(0, Node::stump(1, 0.5, -0.02, 0.0), Provenance::default())
}

fn carries(creature: &CreatureExport, uuid: &str) -> bool {
    creature.neurons.iter().any(|n| n.uuid == uuid)
}

/// `true` when `creature` carries the structure the graft appends for `patch`.
fn carries_graft(creature: &CreatureExport, patch: &Patch) -> bool {
    let prefix = format!("{}-", patch.uuid_prefix());
    creature.neurons.iter().any(|n| n.uuid.starts_with(&prefix))
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
        // Beside `best.json`, which is where a Forests run keeps its outputs.
        let bundle_path = tmp.path().join("forests").join("enhancements.json");
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

    /// The log a Forests run opens against `A`, on the corpus this run will be
    /// judged on.
    fn patch_log(&self) -> PatchLog {
        PatchLog::opening(PRODUCER, &ancestor(), 0.5000, &self.corpus_identity).unwrap()
    }

    fn file(&self, log: &PatchLog) {
        assert!(
            log.write_bundle(&self.bundle_path).unwrap(),
            "the run accepted patches, so a bundle must be written"
        );
    }

    fn summary(&self) -> RebaseSummary {
        serde_json::from_str(
            &std::fs::read_to_string(self.cli.output_dir.join("rebase.json")).unwrap(),
        )
        .unwrap()
    }

    fn published(&self) -> Option<CreatureExport> {
        let path = self.cli.output_dir.join("population-candidate.json");
        let text = std::fs::read_to_string(path).ok()?;
        Some(parse_creature_json(&text).unwrap())
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
// 1. The race: the filed patches replay onto the fresh champion
// ---------------------------------------------------------------------------

#[test]
fn patches_filed_on_a_replay_onto_the_fresh_champion_and_keep_the_fleets_work() {
    let champion = fleet_champion();
    let h = Harness::new(&champion);
    let mut log = h.patch_log();
    log.accept(&first_patch(), 0.5100).unwrap();
    log.accept(&second_patch(), 0.5150).unwrap();
    h.file(&log);

    let scorer = ScriptedScorer::flat(0.80).with("bundle", 0.90);
    assert_eq!(run_with(&h.cli, Some(&scorer)).unwrap(), EXIT_IMPROVED);

    let published = h.published().expect("an improvement was emitted");
    validate_source_creature(&published).unwrap();
    assert!(carries_graft(&published, &first_patch()), "patch 1 grafted");
    assert!(
        carries_graft(&published, &second_patch()),
        "patch 2 grafted"
    );
    assert!(
        carries(&published, "h3"),
        "the fleet's unrelated improvement must survive the graft"
    );

    // Re-entry used the fresh champion, not the run's own local descendant.
    let summary = h.summary();
    assert_eq!(summary.status, "improved");
    assert_eq!(summary.opening_checksum, log.base_checksum());
    assert_eq!(
        summary.opening_checksum,
        creature_checksum(&ancestor()).unwrap()
    );
    assert_ne!(summary.opening_checksum, summary.champion_checksum);
    assert_eq!(summary.producer, PRODUCER);
}

// ---------------------------------------------------------------------------
// 2. The filed id is the id the graft uses
// ---------------------------------------------------------------------------

#[test]
fn a_patch_the_champion_already_carries_is_recognised_and_costs_nothing() {
    // Another host rebased the same discovery onto the champion first, so B
    // already carries the graft. The filed enhancement must be recognised as
    // present — which only works because the filed id is the graft's id.
    let patch = first_patch();
    let application = neat_ai_rebase::forest::apply(&patch, &fleet_champion()).unwrap();
    let champion = application
        .creature()
        .expect("the fixture champion must accept the graft")
        .clone();
    let h = Harness::new(&champion);
    let mut log = h.patch_log();
    let filed = log.accept(&patch, 0.5100).unwrap().clone();
    h.file(&log);

    assert_eq!(filed.meta.id, patch.id(), "the filed id is the patch id");
    assert!(
        carries_graft(&champion, &patch),
        "the champion carries `forest-<id>-…` for that very id"
    );

    // Nothing is built, so the scorer is never asked.
    assert_eq!(
        run_with(&h.cli, Some(&never_called())).unwrap(),
        EXIT_NO_IMPROVEMENT
    );
    let summary = h.summary();
    assert_eq!(summary.status, "nothingToDo");
    assert_eq!(summary.enhancements[0].outcome, "alreadyPresent");
    assert_eq!(
        summary.candidates.len(),
        1,
        "only the baseline: no duplicate work"
    );
    assert!(h.published().is_none());
}

// ---------------------------------------------------------------------------
// 3. Provenance: the opening facts and the corpus Rebase itself computes
// ---------------------------------------------------------------------------

#[test]
fn the_filed_bundle_carries_the_opening_facts_the_order_and_the_exact_payloads() {
    let h = Harness::new(&fleet_champion());
    let mut log = h.patch_log();
    // Accepted second-then-first, to prove the order recorded is acceptance
    // order rather than anything derived from the patches themselves.
    log.accept(&second_patch(), 0.5100).unwrap();
    log.accept(&first_patch(), 0.5150).unwrap();
    h.file(&log);

    let text = std::fs::read_to_string(&h.bundle_path).unwrap();
    let bundle = EnhancementBundle::parse_json(&text).unwrap();
    assert_eq!(bundle.producer, PRODUCER);
    assert_eq!(
        bundle.base_checksum,
        creature_checksum(&ancestor()).unwrap()
    );
    assert!((bundle.base_score - 0.5000).abs() < 1e-12);
    assert_eq!(
        bundle.corpus_identity, h.corpus_identity,
        "the bundle names the corpus identity Rebase computes from the same corpus"
    );

    let filed: Vec<&Patch> = bundle
        .enhancements
        .iter()
        .map(|e| match &e.payload {
            Payload::ForestPatch { patch } => patch,
            Payload::OckhamRemoval { .. } => panic!("a Forests run files patches"),
        })
        .collect();
    assert_eq!(
        filed,
        vec![&second_patch(), &first_patch()],
        "acceptance order, and the exact accepted payload — provenance included"
    );
    for e in &bundle.enhancements {
        assert!(e.id_is_consistent());
        assert_eq!(e.meta.corpus_identity, h.corpus_identity);
        assert_eq!(e.meta.base_checksum, bundle.base_checksum);
    }
    assert!(
        (bundle.enhancements[1].meta.improved_score - 0.5150).abs() < 1e-12,
        "the producer's own two scores bracket the change"
    );
}

// ---------------------------------------------------------------------------
// 4. A run that accepted nothing
// ---------------------------------------------------------------------------

#[test]
fn a_run_that_accepted_nothing_writes_no_bundle_and_says_so() {
    let h = Harness::new(&fleet_champion());
    let log = h.patch_log();
    assert!(log.is_empty());
    assert!(
        !log.write_bundle(&h.bundle_path).unwrap(),
        "an empty run must return the signal not to invoke Rebase"
    );
    assert!(
        !h.bundle_path.exists(),
        "and must leave no bundle behind for a later run to pick up"
    );
}
