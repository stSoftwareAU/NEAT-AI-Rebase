//! Filing accepted NEAT-AI-Ockham prunes as v1 enhancements (Issue #8).
//!
//! [`crate::ockham`] is the *consumer* side: it replays a removal onto whatever
//! champion the fleet has reached. This is the **producer** side — what an
//! Ockham run calls at the moment it accepts a prune, so that the prune becomes
//! a portable artefact instead of a fact buried inside a local descendant.
//!
//! ## Why a log rather than a helper function
//!
//! The run-level facts — the opening creature's checksum and authoritative
//! score, the corpus identity, the widths — are recorded **once, at open**, and
//! stamped on every prune filed afterwards. A producer that rebuilds them per
//! prune eventually rebuilds them from a creature it has already pruned, and
//! files a bundle whose `baseChecksum` names a creature nobody else has. The
//! log holds them so that cannot happen.
//!
//! ```no_run
//! use neat_ai_rebase::enhancement::RemovalStrategy;
//! use neat_ai_rebase::prune_log::PruneLog;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let opening = neat_ai_rebase::fixtures::evolved_descendant(2.0, 0.5);
//! // At open: the ancestor, its authoritative score, and the corpus.
//! let mut log = PruneLog::opening("neat-ai-ockham/0.4.2", &opening, 0.8123, "3f2a1b0c9d8e7f65")?;
//!
//! // At each acceptance: the exact transformation the scorer approved.
//! log.accept("h1", RemovalStrategy::MeanAblation { mean: 0.03125 }, 0.8130)?;
//!
//! // At re-entry: file the bundle, then hand it to Rebase with a *freshly
//! // fetched* champion — never the local incumbent.
//! if log.write_bundle("run/ockham-bundle.json".as_ref())? {
//!     // neat_ai_rebase --champion <fresh> --enhancements run/ockham-bundle.json …
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## What is refused at filing time
//!
//! Filing is the cheapest place to catch a mis-filed prune, so the log fails
//! closed on the mistakes that would otherwise surface an hour later as an
//! un-replayable bundle: a non-finite score or mean, a UUID the opening
//! creature never had, a neuron that was not hidden, and the same removal filed
//! twice.
//!
//! It deliberately does **not** replay the removal to check it. Replay happens
//! against the fresh champion, which is a different creature by definition, and
//! a producer that has already accepted a prune must not lose it because the
//! ancestor happens to refuse a re-derivation.

use std::collections::BTreeMap;
use std::path::Path;

use neat_core::CreatureExport;

use crate::creature::creature_checksum;
use crate::enhancement::{
    Enhancement, EnhancementBundle, OckhamRemoval, Payload, ProducerContext, RemovalStrategy,
};

/// Why a prune could not be filed.
#[derive(Debug, Clone, PartialEq)]
pub enum PruneLogError {
    /// The opening creature could not be serialised, so it has no checksum.
    Checksum(String),
    /// A score that is not a number. `field` names which one.
    NonFiniteScore {
        /// `baseScore` or `improvedScore`.
        field: &'static str,
        /// The value supplied.
        value: f64,
    },
    /// A mean-ablation measurement that is not finite; the replay would refuse
    /// it, so it is refused here.
    NonFiniteMean(f64),
    /// The opening creature has no such neuron. A removal is identified by
    /// UUID, so filing one the ancestor never carried files nothing replayable.
    UnknownNeuron(String),
    /// The named neuron is not hidden. Only hidden neurons are removal targets.
    NotHidden {
        /// The neuron UUID.
        uuid: String,
        /// What it actually is.
        neuron_type: String,
    },
    /// This exact removal has already been filed by this run.
    Duplicate {
        /// The stable enhancement id both share.
        id: String,
        /// The neuron UUID.
        neuron_uuid: String,
    },
    /// The bundle could not be written.
    Write(String),
}

impl std::fmt::Display for PruneLogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Checksum(m) => write!(f, "cannot checksum the opening creature: {m}"),
            Self::NonFiniteScore { field, value } => {
                write!(f, "{field} {value} is not finite")
            }
            Self::NonFiniteMean(mean) => write!(f, "measured mean {mean} is not finite"),
            Self::UnknownNeuron(uuid) => write!(
                f,
                "the opening creature has no neuron `{uuid}`; a prune of a neuron it never \
                 carried cannot be replayed"
            ),
            Self::NotHidden { uuid, neuron_type } => write!(
                f,
                "`{uuid}` is {neuron_type}, not hidden; only hidden neurons are removal targets"
            ),
            Self::Duplicate { id, neuron_uuid } => write!(
                f,
                "`{neuron_uuid}` has already been filed as `{id}` by this run"
            ),
            Self::Write(m) => write!(f, "cannot write the bundle: {m}"),
        }
    }
}

impl std::error::Error for PruneLogError {}

/// Accepted prunes from one Ockham run, in acceptance order.
///
/// Order matters: Rebase's cumulative prefixes only mean something when "the
/// first two" means the same thing to producer and consumer.
#[derive(Debug, Clone, PartialEq)]
pub struct PruneLog {
    producer: String,
    base_checksum: String,
    base_score: f64,
    corpus_identity: String,
    input_count: usize,
    output_count: usize,
    /// Neuron UUID → type, as the opening creature had them.
    opening_neurons: BTreeMap<String, String>,
    accepted: Vec<Enhancement>,
}

impl PruneLog {
    /// Open a log against the creature the run starts from.
    ///
    /// `base_score` is that creature's **authoritative** score on the corpus
    /// named by `corpus_identity` — the same corpus the rebase verdict will be
    /// measured on. Every prune filed afterwards carries these facts.
    ///
    /// # Errors
    ///
    /// [`PruneLogError::Checksum`] when the opening creature cannot be
    /// serialised, and [`PruneLogError::NonFiniteScore`] when `base_score` is
    /// not a number — a bundle whose provenance is `NaN` explains nothing.
    pub fn opening(
        producer: &str,
        opening: &CreatureExport,
        base_score: f64,
        corpus_identity: &str,
    ) -> Result<Self, PruneLogError> {
        if !base_score.is_finite() {
            return Err(PruneLogError::NonFiniteScore {
                field: "baseScore",
                value: base_score,
            });
        }
        Ok(Self {
            producer: producer.to_string(),
            base_checksum: creature_checksum(opening).map_err(PruneLogError::Checksum)?,
            base_score,
            corpus_identity: corpus_identity.to_string(),
            input_count: opening.input,
            output_count: opening.output,
            opening_neurons: opening
                .neurons
                .iter()
                .map(|n| (n.uuid.clone(), n.neuron_type.clone()))
                .collect(),
            accepted: Vec::new(),
        })
    }

    /// File one authoritative accepted prune.
    ///
    /// `improved_score` is what the producer's own scorer measured after the
    /// removal. It is evidence and never permission: the rebase verdict is
    /// re-measured against the fresh champion.
    ///
    /// # Errors
    ///
    /// Every fail-closed case in [`PruneLogError`] except
    /// [`PruneLogError::Checksum`] and [`PruneLogError::Write`]: a non-finite
    /// score or mean, a UUID the opening creature never carried, a neuron that
    /// is not hidden, and a removal this run has already filed.
    pub fn accept(
        &mut self,
        neuron_uuid: &str,
        strategy: RemovalStrategy,
        improved_score: f64,
    ) -> Result<&Enhancement, PruneLogError> {
        if !improved_score.is_finite() {
            return Err(PruneLogError::NonFiniteScore {
                field: "improvedScore",
                value: improved_score,
            });
        }
        if let RemovalStrategy::MeanAblation { mean } = strategy
            && !mean.is_finite()
        {
            return Err(PruneLogError::NonFiniteMean(mean));
        }
        match self.opening_neurons.get(neuron_uuid) {
            None => return Err(PruneLogError::UnknownNeuron(neuron_uuid.to_string())),
            Some(neuron_type) if neuron_type != "hidden" => {
                return Err(PruneLogError::NotHidden {
                    uuid: neuron_uuid.to_string(),
                    neuron_type: neuron_type.clone(),
                });
            }
            Some(_) => {}
        }

        let enhancement = Enhancement::new(
            Payload::OckhamRemoval {
                removal: OckhamRemoval {
                    neuron_uuid: neuron_uuid.to_string(),
                    strategy,
                },
            },
            &ProducerContext {
                producer: self.producer.clone(),
                base_checksum: self.base_checksum.clone(),
                base_score: self.base_score,
                improved_score,
                corpus_identity: self.corpus_identity.clone(),
                input_count: self.input_count,
                output_count: self.output_count,
            },
        );
        if self
            .accepted
            .iter()
            .any(|e| e.meta.id == enhancement.meta.id)
        {
            return Err(PruneLogError::Duplicate {
                id: enhancement.meta.id,
                neuron_uuid: neuron_uuid.to_string(),
            });
        }
        self.accepted.push(enhancement);
        Ok(self.accepted.last().expect("just pushed"))
    }

    /// SHA-256 of the opening creature, as stamped on every filed prune.
    pub fn base_checksum(&self) -> &str {
        &self.base_checksum
    }

    /// Corpus both the opening score and the rebase verdict are measured on.
    pub fn corpus_identity(&self) -> &str {
        &self.corpus_identity
    }

    /// The prunes filed so far, in acceptance order.
    pub fn enhancements(&self) -> &[Enhancement] {
        &self.accepted
    }

    /// `true` when the run accepted nothing. Such a run has nothing to rebase
    /// and should not invoke Rebase at all.
    pub fn is_empty(&self) -> bool {
        self.accepted.is_empty()
    }

    /// The bundle to hand to Rebase, or `None` when nothing was accepted.
    pub fn bundle(&self) -> Option<EnhancementBundle> {
        if self.accepted.is_empty() {
            return None;
        }
        Some(EnhancementBundle::from_enhancements(self.accepted.clone()))
    }

    /// Write the bundle to `path`, creating the parent directory.
    ///
    /// Returns `true` when a bundle was written and `false` when the run
    /// accepted nothing — in which case no file is created and the caller must
    /// not invoke Rebase. The distinction is returned rather than logged: an
    /// empty run and a written bundle are different outcomes and the caller has
    /// to be able to tell them apart.
    ///
    /// # Errors
    ///
    /// [`PruneLogError::Write`] when the directory or the file cannot be
    /// written.
    pub fn write_bundle(&self, path: &Path) -> Result<bool, PruneLogError> {
        let Some(bundle) = self.bundle() else {
            return Ok(false);
        };
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| PruneLogError::Write(format!("{}: {e}", parent.display())))?;
        }
        let json = serde_json::to_string_pretty(&bundle)
            .map_err(|e| PruneLogError::Write(e.to_string()))?;
        std::fs::write(path, json)
            .map_err(|e| PruneLogError::Write(format!("{}: {e}", path.display())))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enhancement::Payload;
    use crate::fixtures::{creature, evolved_descendant, neuron, synapse};

    const CORPUS: &str = "corpus-ockham";

    fn log() -> PruneLog {
        PruneLog::opening(
            "neat-ai-ockham/test",
            &evolved_descendant(2.0, 0.5),
            0.5000,
            CORPUS,
        )
        .unwrap()
    }

    #[test]
    fn every_filed_prune_carries_the_opening_facts() {
        let opening = evolved_descendant(2.0, 0.5);
        let mut log = log();
        log.accept("h1", RemovalStrategy::MeanAblation { mean: 0.25 }, 0.5100)
            .unwrap();
        log.accept("h2", RemovalStrategy::IdentityCollapse, 0.5150)
            .unwrap();

        let expected = creature_checksum(&opening).unwrap();
        for e in log.enhancements() {
            assert_eq!(e.meta.base_checksum, expected);
            assert!((e.meta.base_score - 0.5000).abs() < 1e-12);
            assert_eq!(e.meta.corpus_identity, CORPUS);
            assert_eq!(e.meta.input_count, opening.input);
            assert_eq!(e.meta.output_count, opening.output);
            assert_eq!(e.meta.producer, "neat-ai-ockham/test");
            assert!(e.id_is_consistent(), "a filed id must match its payload");
        }
        assert!((log.enhancements()[1].meta.improved_score - 0.5150).abs() < 1e-12);
    }

    #[test]
    fn prunes_are_filed_as_v1_ockham_removals_in_acceptance_order() {
        let mut log = log();
        log.accept("h2", RemovalStrategy::IdentityCollapse, 0.51)
            .unwrap();
        log.accept("h1", RemovalStrategy::MeanAblation { mean: 0.125 }, 0.52)
            .unwrap();

        let bundle = log.bundle().expect("two prunes were accepted");
        assert_eq!(
            bundle.version,
            crate::enhancement::ENHANCEMENT_FORMAT_VERSION
        );
        let uuids: Vec<&str> = bundle
            .enhancements
            .iter()
            .map(|e| match &e.payload {
                Payload::OckhamRemoval { removal } => removal.neuron_uuid.as_str(),
                Payload::ForestPatch { .. } => panic!("an Ockham run files removals"),
            })
            .collect();
        assert_eq!(uuids, vec!["h2", "h1"], "acceptance order is preserved");

        // And it is the documented wire form, readable by the consumer side.
        let text = serde_json::to_string(&bundle).unwrap();
        let parsed = EnhancementBundle::parse_json(&text).unwrap();
        assert_eq!(parsed, bundle);
        assert!(text.contains(r#""kind":"ockhamRemoval""#), "{text}");
        assert!(text.contains(r#""strategy":"identityCollapse""#), "{text}");
    }

    #[test]
    fn the_same_removal_filed_twice_fails_closed() {
        let mut log = log();
        log.accept("h1", RemovalStrategy::MeanAblation { mean: 0.25 }, 0.51)
            .unwrap();
        // The mean is a measurement, not part of the identity, so a re-measured
        // repeat is the same enhancement and must not be filed twice.
        let err = log
            .accept("h1", RemovalStrategy::MeanAblation { mean: 0.30 }, 0.52)
            .unwrap_err();
        assert!(matches!(err, PruneLogError::Duplicate { .. }), "{err}");
        assert_eq!(log.enhancements().len(), 1);

        // A different strategy on the same neuron is a different enhancement.
        log.accept("h1", RemovalStrategy::IdentityCollapse, 0.53)
            .unwrap();
        assert_eq!(log.enhancements().len(), 2);
    }

    #[test]
    fn a_uuid_the_opening_creature_never_carried_fails_closed() {
        let mut log = log();
        let err = log
            .accept("h404", RemovalStrategy::IdentityCollapse, 0.51)
            .unwrap_err();
        assert!(matches!(err, PruneLogError::UnknownNeuron(_)), "{err}");
        assert!(log.is_empty());
        assert!(log.bundle().is_none());
    }

    #[test]
    fn a_target_that_is_not_hidden_fails_closed() {
        let mut log = log();
        let err = log
            .accept("output-0", RemovalStrategy::IdentityCollapse, 0.51)
            .unwrap_err();
        assert!(matches!(err, PruneLogError::NotHidden { .. }), "{err}");

        // Including a constant, which is neither hidden nor a removal target.
        let with_constant = creature(
            1,
            1,
            vec![
                neuron("constant", "one", 1.0, None),
                neuron("output", "output-0", 0.0, Some("IDENTITY")),
            ],
            vec![
                synapse("input-0", "output-0", 1.0),
                synapse("one", "output-0", 0.5),
            ],
        );
        let mut log =
            PruneLog::opening("neat-ai-ockham/test", &with_constant, 0.5, CORPUS).unwrap();
        let err = log
            .accept("one", RemovalStrategy::IdentityCollapse, 0.51)
            .unwrap_err();
        assert!(matches!(err, PruneLogError::NotHidden { .. }), "{err}");
    }

    #[test]
    fn non_finite_scores_and_means_fail_closed() {
        let err = PruneLog::opening(
            "neat-ai-ockham/test",
            &evolved_descendant(2.0, 0.5),
            f64::NAN,
            CORPUS,
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                PruneLogError::NonFiniteScore {
                    field: "baseScore",
                    ..
                }
            ),
            "{err}"
        );

        let mut log = log();
        let err = log
            .accept("h1", RemovalStrategy::IdentityCollapse, f64::INFINITY)
            .unwrap_err();
        assert!(
            matches!(
                err,
                PruneLogError::NonFiniteScore {
                    field: "improvedScore",
                    ..
                }
            ),
            "{err}"
        );
        let err = log
            .accept("h1", RemovalStrategy::MeanAblation { mean: f64::NAN }, 0.51)
            .unwrap_err();
        assert!(matches!(err, PruneLogError::NonFiniteMean(_)), "{err}");
        assert!(log.is_empty(), "nothing partial is filed");
    }

    #[test]
    fn a_run_that_accepted_nothing_writes_no_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("bundle.json");
        assert!(!log().write_bundle(&path).unwrap(), "nothing to file");
        assert!(
            !path.exists(),
            "an empty run must not leave a bundle behind"
        );

        let mut log = log();
        log.accept("h1", RemovalStrategy::MeanAblation { mean: 0.25 }, 0.51)
            .unwrap();
        assert!(log.write_bundle(&path).unwrap());
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            EnhancementBundle::parse_json(&written).unwrap(),
            log.bundle().unwrap()
        );
    }

    #[test]
    fn the_bundle_replays_through_the_consumer_side() {
        // The point of filing: what the producer wrote is what the engine can
        // apply to a champion that still carries the neuron.
        let mut log = log();
        log.accept("h1", RemovalStrategy::MeanAblation { mean: 0.25 }, 0.51)
            .unwrap();
        let champion = evolved_descendant(2.0, 0.5);
        let outcome = crate::engine::rebase(&crate::engine::RebaseRequest {
            champion: &champion,
            enhancements: log.enhancements(),
            corpus_identity: CORPUS,
            max_candidates: 0,
        })
        .unwrap();
        assert_eq!(
            outcome.reports[0].outcome,
            crate::engine::EnhancementOutcome::Applied
        );
        let candidate = &outcome.cohort[1].creature;
        assert!(!candidate.neurons.iter().any(|n| n.uuid == "h1"));
        assert!(candidate.neurons.iter().any(|n| n.uuid == "h2"));
    }
}
