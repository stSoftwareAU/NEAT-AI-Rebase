//! The generic rebase engine and candidate cohort (Issue #4).
//!
//! Input: the **latest** champion, an ordered enhancement bundle, the corpus
//! identity the decision will be made on, and a cap on how many candidates are
//! worth constructing. Output: a scorer-ready cohort and an explicit outcome
//! for every enhancement.
//!
//! No acceptance happens here. The engine's job ends at "here are creatures
//! worth asking the scorer about"; [`crate::scorer`] decides.
//!
//! ## What the cohort contains
//!
//! In this order, which is fixed so a run is reproducible and so a tight
//! `max_candidates` drops the least useful members rather than an arbitrary
//! slice:
//!
//! 1. `bundle` — every applicable enhancement, in the producer's acceptance
//!    order (omitted when there is only one, since it would equal `single-00`);
//! 2. `single-NN` — each applicable enhancement on its own;
//! 3. `prefix-NN` — the cumulative prefixes in between.
//!
//! The champion itself is always present as `baseline`, and it is never
//! counted against the cap: a verdict without an explicit baseline is not a
//! verdict.
//!
//! ## Why prefixes and singles both
//!
//! Two changes that each helped separately may interact badly — the module
//! makes no additivity assumption. Scoring the singles and the combination
//! together lets the scorer pick the best *verified* subset instead of
//! Rebase guessing which members are carrying the improvement.
//!
//! ## De-duplication
//!
//! Candidates are keyed by the checksum of the resulting creature. A prefix
//! that happens to build the same creature as a single, or a combination whose
//! members were all already present, collapses to one scored candidate — and a
//! candidate identical to the champion is dropped entirely, because asking the
//! scorer whether the champion beats itself wastes a corpus pass.

use std::collections::HashSet;
use std::fmt;

use neat_core::CreatureExport;

use crate::adapter::{Application, apply, is_present};
use crate::compat::{Target, check_common};
use crate::creature::{creature_checksum, validate_source_creature};
use crate::enhancement::Enhancement;

/// What Rebase is being asked to do.
#[derive(Debug, Clone, Copy)]
pub struct RebaseRequest<'a> {
    /// The latest global champion. Fetch it immediately before calling —
    /// a champion that is minutes old is the problem Rebase exists to solve.
    pub champion: &'a CreatureExport,
    /// The enhancement bundle, in the producer's acceptance order.
    pub enhancements: &'a [Enhancement],
    /// Identity of the corpus the scorer will judge on.
    pub corpus_identity: &'a str,
    /// Maximum candidates to construct, excluding the baseline. `0` means no
    /// cap.
    pub max_candidates: usize,
}

/// What happened to one enhancement.
#[derive(Debug, Clone, PartialEq)]
pub enum EnhancementOutcome {
    /// Applied to the champion; it contributed at least one candidate.
    Applied,
    /// The champion already carries it. A clean no-op.
    AlreadyPresent,
    /// It could not be attempted, or could not be constructed, for this reason.
    Incompatible(String),
}

impl EnhancementOutcome {
    /// Stable label for journals.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::AlreadyPresent => "alreadyPresent",
            Self::Incompatible(_) => "incompatible",
        }
    }
}

/// The engine's verdict on one enhancement, ready to journal.
#[derive(Debug, Clone, PartialEq)]
pub struct EnhancementReport {
    /// Stable enhancement id.
    pub id: String,
    /// Payload kind (`forestPatch` / `ockhamRemoval`).
    pub kind: &'static str,
    /// Who produced it.
    pub producer: String,
    /// The improvement the producer measured on its own opening creature.
    /// Evidence only.
    pub claimed_gain: f64,
    /// What happened.
    pub outcome: EnhancementOutcome,
}

/// One creature the scorer will be asked about.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Stable label, also the scorer's file stem: `baseline`, `bundle`,
    /// `single-00`, `prefix-02`, …
    pub label: String,
    /// The creature.
    pub creature: CreatureExport,
    /// Ids of the enhancements this candidate actually applied, in order.
    pub applied_ids: Vec<String>,
    /// Checksum of the creature's canonical JSON.
    pub checksum: String,
}

impl Candidate {
    /// `true` for the champion baseline.
    pub fn is_baseline(&self) -> bool {
        self.label == BASELINE_LABEL
    }
}

/// Scorer file stem reserved for the champion.
pub const BASELINE_LABEL: &str = "baseline";

/// Scorer file stem reserved for the producer's own descendant.
///
/// It is scored beside the cohort so one authoritative call answers the
/// question the producer actually has — "was publishing my own creature worth
/// more than rebasing my discoveries?" — but it is never a candidate, because
/// it descends from an ancestor the champion has already moved past.
pub const REFERENCE_LABEL: &str = "source";

/// Everything the engine produced.
#[derive(Debug, Clone, PartialEq)]
pub struct RebaseOutcome {
    /// Checksum of the champion the cohort was built from.
    pub champion_checksum: String,
    /// One report per enhancement, in bundle order.
    pub reports: Vec<EnhancementReport>,
    /// The cohort. `cohort[0]` is always the baseline.
    pub cohort: Vec<Candidate>,
    /// Candidates constructed and then dropped to honour `max_candidates`.
    /// Never silent: the CLI journals it and the caller can raise the cap.
    pub dropped_for_cap: Vec<String>,
    /// Combinations that could not be constructed, with the reason. A single
    /// that applied on its own can still fail inside a combination, and that
    /// must not corrupt the shorter prefixes.
    pub combination_failures: Vec<String>,
    /// The producer's own descendant, scored for comparison only. Set by the
    /// caller after [`rebase`]; it takes no part in building the cohort and
    /// can never win.
    pub reference: Option<Candidate>,
}

impl RebaseOutcome {
    /// Candidates other than the baseline.
    pub fn candidates(&self) -> impl Iterator<Item = &Candidate> {
        self.cohort.iter().filter(|c| !c.is_baseline())
    }

    /// `true` when there is nothing for the scorer to compare against the
    /// champion — every enhancement was already present or incompatible.
    pub fn is_empty(&self) -> bool {
        self.candidates().next().is_none()
    }
}

/// Why the engine could not run at all.
#[derive(Debug, Clone, PartialEq)]
pub enum RebaseError {
    /// The supplied champion is not a creature Rebase will build on.
    Champion(String),
    /// A creature could not be serialised for checksumming.
    Serialise(String),
}

impl fmt::Display for RebaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Champion(m) => write!(f, "champion refused: {m}"),
            Self::Serialise(m) => write!(f, "cannot serialise creature: {m}"),
        }
    }
}

impl std::error::Error for RebaseError {}

/// Build the candidate cohort.
///
/// The champion is read, never written: every candidate is built on a clone,
/// and `champion_bytes_unchanged` pins that.
///
/// # Errors
///
/// [`RebaseError::Champion`] when the supplied champion fails validation —
/// there is no safe way to build on a creature the scorer would refuse — and
/// [`RebaseError::Serialise`] when a creature cannot be checksummed.
pub fn rebase(request: &RebaseRequest<'_>) -> Result<RebaseOutcome, RebaseError> {
    validate_source_creature(request.champion).map_err(|e| RebaseError::Champion(e.to_string()))?;
    let champion_checksum = creature_checksum(request.champion).map_err(RebaseError::Serialise)?;
    let target = Target::new(request.champion, request.corpus_identity);

    let mut reports = Vec::with_capacity(request.enhancements.len());
    // Enhancements that both clear the gate and construct on their own. Only
    // these take part in the combinations.
    let mut usable: Vec<&Enhancement> = Vec::new();
    let mut singles: Vec<(String, CreatureExport, Vec<String>)> = Vec::new();

    for enhancement in request.enhancements {
        let meta = &enhancement.meta;
        let mut report = EnhancementReport {
            id: meta.id.clone(),
            kind: enhancement.payload.kind(),
            producer: meta.producer.clone(),
            claimed_gain: meta.claimed_gain(),
            outcome: EnhancementOutcome::Applied,
        };
        if let Err(reason) = check_common(enhancement, &target) {
            report.outcome = EnhancementOutcome::Incompatible(reason.to_string());
            reports.push(report);
            continue;
        }
        if is_present(enhancement, request.champion) {
            report.outcome = EnhancementOutcome::AlreadyPresent;
            reports.push(report);
            continue;
        }
        match apply(enhancement, &target) {
            Ok(Application::Applied { creature, .. }) => {
                let label = format!("single-{:02}", usable.len());
                singles.push((label, *creature, vec![meta.id.clone()]));
                usable.push(enhancement);
            }
            // `is_present` said no and the adapter says yes: the adapter is the
            // authority on its own idempotence, so take its word for it.
            Ok(Application::AlreadyPresent) => {
                report.outcome = EnhancementOutcome::AlreadyPresent;
            }
            Err(reason) => {
                report.outcome = EnhancementOutcome::Incompatible(reason.to_string());
            }
        }
        reports.push(report);
    }

    let mut combination_failures = Vec::new();
    let prefixes = build_prefixes(
        request.champion,
        request.corpus_identity,
        &usable,
        &mut combination_failures,
    );

    // Ordering is fixed: the full bundle, then the singles, then the shorter
    // prefixes. A tight cap therefore drops the least informative members.
    let mut ordered: Vec<(String, CreatureExport, Vec<String>)> = Vec::new();
    if usable.len() > 1
        && let Some(full) = prefixes.iter().find(|(k, _, _)| *k == usable.len())
    {
        ordered.push(("bundle".to_string(), full.1.clone(), full.2.clone()));
    }
    ordered.extend(singles);
    for (k, creature, ids) in &prefixes {
        if *k > 1 && *k < usable.len() {
            ordered.push((format!("prefix-{k:02}"), creature.clone(), ids.clone()));
        }
    }

    let mut cohort = vec![Candidate {
        label: BASELINE_LABEL.to_string(),
        creature: request.champion.clone(),
        applied_ids: Vec::new(),
        checksum: champion_checksum.clone(),
    }];
    let mut seen: HashSet<String> = HashSet::from([champion_checksum.clone()]);
    let mut dropped_for_cap = Vec::new();
    for (label, creature, applied_ids) in ordered {
        let checksum = creature_checksum(&creature).map_err(RebaseError::Serialise)?;
        // A candidate identical to the champion, or to one already in the
        // cohort, is not worth a corpus pass.
        if !seen.insert(checksum.clone()) {
            continue;
        }
        if request.max_candidates > 0 && cohort.len() > request.max_candidates {
            dropped_for_cap.push(label);
            continue;
        }
        cohort.push(Candidate {
            label,
            creature,
            applied_ids,
            checksum,
        });
    }

    Ok(RebaseOutcome {
        champion_checksum,
        reports,
        cohort,
        dropped_for_cap,
        combination_failures,
        reference: None,
    })
}

/// Cumulative prefixes over `usable`, as `(length, creature, applied ids)`.
///
/// A member that fails inside the combination stops that chain and is recorded
/// — the shorter prefixes already built stay valid, which is what "one
/// incompatible enhancement does not corrupt subsequent candidates" means in
/// practice.
fn build_prefixes(
    champion: &CreatureExport,
    corpus_identity: &str,
    usable: &[&Enhancement],
    failures: &mut Vec<String>,
) -> Vec<(usize, CreatureExport, Vec<String>)> {
    let mut out = Vec::with_capacity(usable.len());
    let mut current = champion.clone();
    let mut applied_ids = Vec::new();
    for (k, enhancement) in usable.iter().enumerate() {
        let target = Target::new(&current, corpus_identity);
        match apply(enhancement, &target) {
            Ok(Application::Applied { creature, .. }) => {
                current = *creature;
                applied_ids.push(enhancement.meta.id.clone());
            }
            // An earlier member of the combination already brought it in.
            Ok(Application::AlreadyPresent) => {}
            Err(reason) => {
                failures.push(format!(
                    "combination stopped at member {} (`{}`): {reason}",
                    k + 1,
                    enhancement.meta.id
                ));
                break;
            }
        }
        out.push((k + 1, current.clone(), applied_ids.clone()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enhancement::{OckhamRemoval, Payload, ProducerContext, RemovalStrategy};
    use crate::fixtures::{evolved_descendant, linear_hidden_creature};
    use crate::patch::{Node, Patch, Provenance};
    use neat_core::creature_to_json;

    const CORPUS: &str = "corpus-1";

    fn forest_enhancement(feature: usize, right: f32) -> Enhancement {
        Enhancement::new(
            Payload::ForestPatch {
                patch: Patch::new(
                    0,
                    Node::stump(feature, 0.5, 0.0, right),
                    Provenance::default(),
                ),
            },
            &ProducerContext {
                producer: "neat-ai-forests/test".into(),
                base_checksum: "base".into(),
                base_score: 0.5,
                improved_score: 0.6,
                corpus_identity: CORPUS.into(),
                input_count: 2,
                output_count: 1,
            },
        )
    }

    fn removal_enhancement(uuid: &str) -> Enhancement {
        Enhancement::new(
            Payload::OckhamRemoval {
                removal: OckhamRemoval {
                    neuron_uuid: uuid.into(),
                    strategy: RemovalStrategy::MeanAblation { mean: 0.5 },
                },
            },
            &ProducerContext {
                producer: "neat-ai-ockham/test".into(),
                base_checksum: "base".into(),
                base_score: 0.5,
                improved_score: 0.6,
                corpus_identity: CORPUS.into(),
                input_count: 2,
                output_count: 1,
            },
        )
    }

    fn run(champion: &CreatureExport, enhancements: &[Enhancement]) -> RebaseOutcome {
        rebase(&RebaseRequest {
            champion,
            enhancements,
            corpus_identity: CORPUS,
            max_candidates: 0,
        })
        .unwrap()
    }

    #[test]
    fn one_enhancement_gives_a_baseline_and_one_candidate() {
        let champion = linear_hidden_creature(2.0);
        let out = run(&champion, &[forest_enhancement(1, 0.25)]);
        assert_eq!(out.cohort.len(), 2);
        assert!(out.cohort[0].is_baseline());
        assert_eq!(out.cohort[1].label, "single-00");
        assert_eq!(out.reports[0].outcome, EnhancementOutcome::Applied);
        // No `bundle` when there is only one member: it would be a duplicate.
        assert!(!out.cohort.iter().any(|c| c.label == "bundle"));
    }

    #[test]
    fn three_enhancements_give_singles_prefixes_and_the_full_bundle() {
        let champion = evolved_descendant(2.0, 0.5);
        let out = run(
            &champion,
            &[
                forest_enhancement(0, 0.25),
                forest_enhancement(1, -0.1),
                removal_enhancement("h2"),
            ],
        );
        let labels: Vec<&str> = out.cohort.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "baseline",
                "bundle",
                "single-00",
                "single-01",
                "single-02",
                "prefix-02"
            ]
        );
        assert_eq!(
            out.cohort
                .iter()
                .find(|c| c.label == "bundle")
                .unwrap()
                .applied_ids
                .len(),
            3
        );
        assert!(out.combination_failures.is_empty());
    }

    #[test]
    fn an_already_present_enhancement_is_reported_and_contributes_nothing() {
        let champion = linear_hidden_creature(2.0);
        let e = forest_enhancement(1, 0.25);
        let once = run(&champion, std::slice::from_ref(&e));
        let grafted = once.cohort[1].creature.clone();

        let out = run(&grafted, &[e]);
        assert_eq!(out.reports[0].outcome, EnhancementOutcome::AlreadyPresent);
        assert!(out.is_empty(), "nothing left to score");
        assert_eq!(out.cohort.len(), 1);
    }

    #[test]
    fn an_incompatible_enhancement_does_not_corrupt_the_others() {
        let champion = linear_hidden_creature(2.0);
        // Feature 9 does not exist on a 2-input champion.
        let bad = forest_enhancement(9, 0.25);
        let good_a = forest_enhancement(0, 0.25);
        let good_b = forest_enhancement(1, -0.1);
        let out = run(&champion, &[good_a, bad, good_b]);

        assert_eq!(out.reports[0].outcome, EnhancementOutcome::Applied);
        assert!(matches!(
            out.reports[1].outcome,
            EnhancementOutcome::Incompatible(_)
        ));
        assert_eq!(out.reports[2].outcome, EnhancementOutcome::Applied);
        // Both good enhancements still combine.
        let bundle = out.cohort.iter().find(|c| c.label == "bundle").unwrap();
        assert_eq!(bundle.applied_ids.len(), 2);
    }

    #[test]
    fn corpus_drift_makes_every_enhancement_incompatible() {
        let champion = linear_hidden_creature(2.0);
        let out = rebase(&RebaseRequest {
            champion: &champion,
            enhancements: &[forest_enhancement(0, 0.25)],
            corpus_identity: "a-different-corpus",
            max_candidates: 0,
        })
        .unwrap();
        assert!(matches!(
            out.reports[0].outcome,
            EnhancementOutcome::Incompatible(_)
        ));
        assert!(out.is_empty());
    }

    #[test]
    fn duplicate_candidates_are_removed() {
        let champion = linear_hidden_creature(2.0);
        // The same enhancement twice: the second is already present by the
        // time the combination reaches it, so the bundle equals the single and
        // collapses to one candidate.
        let e = forest_enhancement(1, 0.25);
        let out = run(&champion, &[e.clone(), e]);
        let checksums: HashSet<&String> = out.cohort.iter().map(|c| &c.checksum).collect();
        assert_eq!(checksums.len(), out.cohort.len(), "no duplicate creatures");
        // Both apply on their own — they are the same patch — so the singles,
        // the prefix and the bundle all build one creature, and one candidate
        // survives de-duplication.
        assert_eq!(out.cohort.len(), 2);
        // The combination applied the patch once and recognised the repeat.
        let bundle = out
            .cohort
            .iter()
            .find(|c| !c.is_baseline())
            .expect("one candidate");
        assert_eq!(bundle.applied_ids.len(), 1);
    }

    #[test]
    fn a_candidate_identical_to_the_champion_is_dropped() {
        // A removal of a neuron that is not there is already present, so no
        // candidate is built at all.
        let champion = linear_hidden_creature(2.0);
        let out = run(&champion, &[removal_enhancement("not-here")]);
        assert_eq!(out.cohort.len(), 1);
        assert!(out.cohort[0].is_baseline());
    }

    #[test]
    fn the_cap_truncates_deterministically_and_reports_what_it_dropped() {
        let champion = evolved_descendant(2.0, 0.5);
        let enhancements = [
            forest_enhancement(0, 0.25),
            forest_enhancement(1, -0.1),
            removal_enhancement("h2"),
        ];
        let out = rebase(&RebaseRequest {
            champion: &champion,
            enhancements: &enhancements,
            corpus_identity: CORPUS,
            max_candidates: 2,
        })
        .unwrap();
        let labels: Vec<&str> = out.cohort.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["baseline", "bundle", "single-00"]);
        assert_eq!(
            out.dropped_for_cap,
            vec!["single-01", "single-02", "prefix-02"]
        );
    }

    #[test]
    fn champion_bytes_unchanged() {
        let champion = evolved_descendant(2.0, 0.5);
        let before = creature_to_json(&champion).unwrap();
        let _ = run(
            &champion,
            &[
                forest_enhancement(0, 0.25),
                forest_enhancement(1, -0.1),
                removal_enhancement("h2"),
            ],
        );
        assert_eq!(creature_to_json(&champion).unwrap(), before);
    }

    #[test]
    fn every_candidate_carries_the_ids_it_actually_applied() {
        let champion = evolved_descendant(2.0, 0.5);
        let a = forest_enhancement(0, 0.25);
        let b = removal_enhancement("h2");
        let out = run(&champion, &[a.clone(), b.clone()]);
        let bundle = out.cohort.iter().find(|c| c.label == "bundle").unwrap();
        assert_eq!(
            bundle.applied_ids,
            vec![a.meta.id.clone(), b.meta.id.clone()]
        );
        let single = out.cohort.iter().find(|c| c.label == "single-01").unwrap();
        assert_eq!(single.applied_ids, vec![b.meta.id]);
    }

    #[test]
    fn a_refused_champion_is_an_error_not_a_silent_empty_cohort() {
        let mut broken = linear_hidden_creature(2.0);
        broken.neurons[0].bias = f64::NAN;
        let err = rebase(&RebaseRequest {
            champion: &broken,
            enhancements: &[],
            corpus_identity: CORPUS,
            max_candidates: 0,
        })
        .unwrap_err();
        assert!(matches!(err, RebaseError::Champion(_)), "{err}");
    }
}
