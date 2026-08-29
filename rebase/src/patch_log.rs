//! Filing accepted NEAT-AI-Forests patches as v1 enhancements (Issue #36).
//!
//! [`crate::forest`] is the *consumer* side: it grafts a patch onto whatever
//! champion the fleet has reached. This is the **producer** side — what a
//! Forests run calls at the moment it authoritatively accepts a patch, so the
//! discovery becomes a filed artefact instead of a fact that has to be dug back
//! out of the creature it happened to end up in.
//!
//! ## Why filing beats harvesting
//!
//! [`crate::harvest`] can reconstruct patches from a published creature,
//! because every neuron a graft appends is named `forest-<patch id>-…` and the
//! id digests the correction. That is genuinely good enough to ship on, and it
//! stays for creatures published before this existed. It is not good enough to
//! keep:
//!
//! * **it only sees what survived** — a patch the run accepted and later
//!   dropped, or one a pruner has since rewritten, no longer hashes to its own
//!   id and is silently gone;
//! * **it cannot recover the producer's scores** — a harvest's `baseScore` and
//!   `improvedScore` are its own honest zero, so the journal cannot show what
//!   the producer measured against what the rebase delivered;
//! * **it reconstructs rather than records** — any divergence between what
//!   Forests emits and what [`crate::patch`] mirrors shows up as an id mismatch
//!   and a skipped patch, whereas a filed bundle has nothing to diverge from;
//! * **its order is inferred** — the engine's cumulative prefixes only mean
//!   something when "the first two" means the same at both ends, and a harvest
//!   sorts by id, which is stable but arbitrary.
//!
//! ## Why a log rather than a helper function
//!
//! The run-level facts — the opening creature's checksum and authoritative
//! score, the corpus identity, the widths — are recorded **once, at open**, and
//! stamped on every patch filed afterwards. A producer that rebuilds them per
//! patch eventually rebuilds them from a creature it has already grafted, and
//! files a bundle whose `baseChecksum` names a creature nobody else has.
//!
//! ```no_run
//! use neat_ai_rebase::patch::{Node, Patch, Provenance};
//! use neat_ai_rebase::patch_log::PatchLog;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let opening = neat_ai_rebase::fixtures::evolved_descendant(2.0, 0.5);
//! # let accepted = Patch::new(0, Node::stump(0, 0.25, 0.0, 0.01), Provenance::default());
//! # let combo = vec![accepted.clone()];
//! // At open: the ancestor, its authoritative score, and the corpus.
//! let mut log = PatchLog::opening("neat-ai-forests/0.1.18", &opening, 0.8123, "3f2a1b0c9d8e7f65")?;
//!
//! // At each acceptance: the exact patch the scorer approved, unmodified.
//! log.accept(&accepted, 0.8130)?;
//!
//! // A boosting combo is filed as its members, in the order the combo applies
//! // them; members already filed are left where they are.
//! log.accept_combo(&combo, 0.8141)?;
//!
//! // At re-entry: file the bundle beside `best.json`, then hand it to Rebase
//! // with a *freshly fetched* champion — never the local descendant.
//! if log.write_bundle("run/enhancements.json".as_ref())? {
//!     // neat_ai_rebase --champion <fresh> --enhancements run/enhancements.json …
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## What is refused at filing time
//!
//! Filing is the cheapest place to catch a mis-filed patch, so the log fails
//! closed on what would otherwise surface an hour later as an un-graftable
//! bundle: a non-finite score, a patch format version this build does not
//! implement, a non-finite weight, threshold or leaf, a bare-leaf root, an
//! output or feature index the opening creature does not have, a condition that
//! names one feature twice, and the same patch filed twice. These are the
//! graft's own preconditions ([`crate::forest`]), checked against the creature
//! the producer opened on.
//!
//! It deliberately does **not** graft the patch to check it. The graft happens
//! against the fresh champion, which is a different creature by definition, and
//! a producer that has already accepted a patch must not lose it because the
//! ancestor's anchor walk happens to refuse a re-derivation.

use std::collections::BTreeSet;
use std::path::Path;

use neat_core::CreatureExport;

use crate::creature::creature_checksum;
use crate::enhancement::{Enhancement, EnhancementBundle, Payload, ProducerContext};
use crate::patch::{Node, PATCH_FORMAT_VERSION, Patch};

/// Why a patch could not be filed.
#[derive(Debug, Clone, PartialEq)]
pub enum PatchLogError {
    /// The opening creature could not be serialised, so it has no checksum.
    Checksum(String),
    /// A score that is not a number. `field` names which one.
    NonFiniteScore {
        /// `baseScore` or `improvedScore`.
        field: &'static str,
        /// The value supplied.
        value: f64,
    },
    /// A patch format version this build does not implement.
    UnsupportedPatchVersion {
        /// Version found on the patch.
        found: u32,
        /// Version this build implements.
        supported: u32,
    },
    /// A weight, threshold or leaf that is not finite; the graft would refuse
    /// it, so it is refused here.
    NonFiniteTree,
    /// The root is a bare leaf: grafting it would add structure that corrects
    /// nothing.
    BareLeafRoot,
    /// The patch targets an output the opening creature does not have.
    OutputOutOfRange {
        /// Output index the patch names.
        output: usize,
        /// Outputs the opening creature has.
        output_count: usize,
    },
    /// A condition reads an input the opening creature does not have.
    FeatureOutOfRange {
        /// Feature index the condition names.
        feature: usize,
        /// Inputs the opening creature has.
        input_count: usize,
    },
    /// One condition names the same feature twice; one term with the summed
    /// weight says the same thing, and the graft refuses the ambiguity.
    RepeatedFeature(usize),
    /// This exact patch has already been filed by this run.
    Duplicate {
        /// The stable enhancement id, which is [`Patch::id`].
        id: String,
    },
    /// A combo with no members. A combo is an accepted set of patches; an empty
    /// one records nothing and is a caller bug, not an empty result.
    EmptyCombo,
    /// The bundle could not be written.
    Write(String),
}

impl std::fmt::Display for PatchLogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Checksum(m) => write!(f, "cannot checksum the opening creature: {m}"),
            Self::NonFiniteScore { field, value } => write!(f, "{field} {value} is not finite"),
            Self::UnsupportedPatchVersion { found, supported } => write!(
                f,
                "patch format version {found} is not supported (this build implements {supported})"
            ),
            Self::NonFiniteTree => {
                write!(f, "patch carries a non-finite weight, threshold or leaf")
            }
            Self::BareLeafRoot => write!(
                f,
                "patch root is a bare leaf: it would add structure that corrects nothing"
            ),
            Self::OutputOutOfRange {
                output,
                output_count,
            } => write!(
                f,
                "patch targets output {output} but the opening creature has {output_count} outputs"
            ),
            Self::FeatureOutOfRange {
                feature,
                input_count,
            } => write!(
                f,
                "patch reads feature {feature} but the opening creature has {input_count} inputs"
            ),
            Self::RepeatedFeature(feature) => write!(
                f,
                "condition names feature {feature} twice; one term with the summed weight says \
                 the same thing"
            ),
            Self::Duplicate { id } => {
                write!(f, "patch `{id}` has already been filed by this run")
            }
            Self::EmptyCombo => write!(f, "a combo carries at least one patch"),
            Self::Write(m) => write!(f, "cannot write the bundle: {m}"),
        }
    }
}

impl std::error::Error for PatchLogError {}

/// Accepted patches from one Forests run, in acceptance order.
///
/// Order matters: Rebase's cumulative prefixes only mean something when "the
/// first two" means the same thing to producer and consumer.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchLog {
    producer: String,
    base_checksum: String,
    base_score: f64,
    corpus_identity: String,
    input_count: usize,
    output_count: usize,
    accepted: Vec<Enhancement>,
}

impl PatchLog {
    /// Open a log against the creature the run starts from.
    ///
    /// `base_score` is that creature's **authoritative** score on the corpus
    /// named by `corpus_identity` — the same corpus the rebase verdict will be
    /// measured on. Every patch filed afterwards carries these facts.
    ///
    /// # Errors
    ///
    /// [`PatchLogError::Checksum`] when the opening creature cannot be
    /// serialised, and [`PatchLogError::NonFiniteScore`] when `base_score` is
    /// not a number — a bundle whose provenance is `NaN` explains nothing.
    pub fn opening(
        producer: &str,
        opening: &CreatureExport,
        base_score: f64,
        corpus_identity: &str,
    ) -> Result<Self, PatchLogError> {
        if !base_score.is_finite() {
            return Err(PatchLogError::NonFiniteScore {
                field: "baseScore",
                value: base_score,
            });
        }
        Ok(Self {
            producer: producer.to_string(),
            base_checksum: creature_checksum(opening).map_err(PatchLogError::Checksum)?,
            base_score,
            corpus_identity: corpus_identity.to_string(),
            input_count: opening.input,
            output_count: opening.output,
            accepted: Vec::new(),
        })
    }

    /// File one authoritatively accepted patch.
    ///
    /// The patch is filed **as given**: Rebase carries the bytes through
    /// unchanged, provenance included, because normalising or rounding them
    /// would move [`Patch::id`] and the graft would then be applied twice.
    ///
    /// `improved_score` is what the producer's own scorer measured with the
    /// patch applied. It is evidence and never permission: the rebase verdict
    /// is re-measured against the fresh champion.
    ///
    /// # Errors
    ///
    /// Every fail-closed case in [`PatchLogError`] except
    /// [`PatchLogError::Checksum`], [`PatchLogError::EmptyCombo`] and
    /// [`PatchLogError::Write`].
    pub fn accept(
        &mut self,
        patch: &Patch,
        improved_score: f64,
    ) -> Result<&Enhancement, PatchLogError> {
        self.check_score(improved_score)?;
        self.check_patch(patch)?;
        let id = patch.id();
        if self.contains(&id) {
            return Err(PatchLogError::Duplicate { id });
        }
        self.file(patch, improved_score);
        Ok(self.accepted.last().expect("just filed"))
    }

    /// File an authoritatively accepted **combo** — a set of patches the
    /// producer verified together, as boosting rounds do.
    ///
    /// The members are filed in the order the combo applies them, so the
    /// bundle's prefix of that length reproduces exactly the creature the
    /// combo's score was measured on. A member this run has already filed —
    /// the single that seeded the boosting round, typically — is left where it
    /// already is rather than duplicated; the returned slice is what was newly
    /// filed, so a caller can tell how much of the combo was new.
    ///
    /// `improved_score` is the combo's own authoritative score, stamped on each
    /// newly filed member: with the run's `baseScore`, it brackets the change
    /// the prefix ending at that member delivered.
    ///
    /// # Errors
    ///
    /// [`PatchLogError::EmptyCombo`] for no members,
    /// [`PatchLogError::Duplicate`] when the combo names the same patch twice,
    /// and every per-patch case [`accept`](Self::accept) refuses. Nothing is
    /// filed unless every member passes.
    pub fn accept_combo(
        &mut self,
        patches: &[Patch],
        improved_score: f64,
    ) -> Result<&[Enhancement], PatchLogError> {
        if patches.is_empty() {
            return Err(PatchLogError::EmptyCombo);
        }
        self.check_score(improved_score)?;
        let mut seen = BTreeSet::new();
        for patch in patches {
            self.check_patch(patch)?;
            if !seen.insert(patch.id()) {
                return Err(PatchLogError::Duplicate { id: patch.id() });
            }
        }

        let first_new = self.accepted.len();
        for patch in patches {
            if !self.contains(&patch.id()) {
                self.file(patch, improved_score);
            }
        }
        Ok(&self.accepted[first_new..])
    }

    /// SHA-256 of the opening creature, as stamped on every filed patch.
    pub fn base_checksum(&self) -> &str {
        &self.base_checksum
    }

    /// Corpus both the opening score and the rebase verdict are measured on.
    pub fn corpus_identity(&self) -> &str {
        &self.corpus_identity
    }

    /// The patches filed so far, in acceptance order.
    pub fn enhancements(&self) -> &[Enhancement] {
        &self.accepted
    }

    /// `true` when this run has already filed the patch with this id.
    pub fn contains(&self, id: &str) -> bool {
        self.accepted.iter().any(|e| e.meta.id == id)
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
    /// [`PatchLogError::Write`] when the directory or the file cannot be
    /// written.
    pub fn write_bundle(&self, path: &Path) -> Result<bool, PatchLogError> {
        let Some(bundle) = self.bundle() else {
            return Ok(false);
        };
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| PatchLogError::Write(format!("{}: {e}", parent.display())))?;
        }
        let json = serde_json::to_string_pretty(&bundle)
            .map_err(|e| PatchLogError::Write(e.to_string()))?;
        std::fs::write(path, json)
            .map_err(|e| PatchLogError::Write(format!("{}: {e}", path.display())))?;
        Ok(true)
    }

    /// Append `patch` to the log, stamped with the opening facts.
    fn file(&mut self, patch: &Patch, improved_score: f64) {
        self.accepted.push(Enhancement::new(
            Payload::ForestPatch {
                patch: patch.clone(),
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
        ));
    }

    fn check_score(&self, improved_score: f64) -> Result<(), PatchLogError> {
        if improved_score.is_finite() {
            return Ok(());
        }
        Err(PatchLogError::NonFiniteScore {
            field: "improvedScore",
            value: improved_score,
        })
    }

    /// The graft's own preconditions, measured against the opening creature.
    fn check_patch(&self, patch: &Patch) -> Result<(), PatchLogError> {
        if patch.version != PATCH_FORMAT_VERSION {
            return Err(PatchLogError::UnsupportedPatchVersion {
                found: patch.version,
                supported: PATCH_FORMAT_VERSION,
            });
        }
        if !patch.root.is_finite() {
            return Err(PatchLogError::NonFiniteTree);
        }
        if matches!(patch.root, Node::Leaf { .. }) {
            return Err(PatchLogError::BareLeafRoot);
        }
        if patch.output >= self.output_count {
            return Err(PatchLogError::OutputOutOfRange {
                output: patch.output,
                output_count: self.output_count,
            });
        }
        check_conditions(&patch.root, self.input_count)
    }
}

/// Every condition in the tree reads inputs the opening creature has, and names
/// each of them once.
fn check_conditions(node: &Node, input_count: usize) -> Result<(), PatchLogError> {
    let Node::Split {
        condition,
        left,
        right,
    } = node
    else {
        return Ok(());
    };
    let mut seen = BTreeSet::new();
    for term in &condition.terms {
        if term.feature >= input_count {
            return Err(PatchLogError::FeatureOutOfRange {
                feature: term.feature,
                input_count,
            });
        }
        if !seen.insert(term.feature) {
            return Err(PatchLogError::RepeatedFeature(term.feature));
        }
    }
    check_conditions(left, input_count)?;
    check_conditions(right, input_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::evolved_descendant;
    use crate::patch::{Condition, Provenance, Term};

    const CORPUS: &str = "corpus-forests";

    fn log() -> PatchLog {
        PatchLog::opening(
            "neat-ai-forests/test",
            &evolved_descendant(2.0, 0.5),
            0.5000,
            CORPUS,
        )
        .unwrap()
    }

    fn stump(feature: usize, right: f32) -> Patch {
        Patch::new(
            0,
            Node::stump(feature, 0.25, 0.0, right),
            Provenance::default(),
        )
    }

    fn filed_patches(log: &PatchLog) -> Vec<&Patch> {
        log.enhancements()
            .iter()
            .map(|e| match &e.payload {
                Payload::ForestPatch { patch } => patch,
                Payload::OckhamRemoval { .. } => panic!("a Forests run files patches"),
            })
            .collect()
    }

    #[test]
    fn every_filed_patch_carries_the_opening_facts() {
        let opening = evolved_descendant(2.0, 0.5);
        let mut log = log();
        log.accept(&stump(0, 0.01), 0.5100).unwrap();
        log.accept(&stump(1, 0.02), 0.5150).unwrap();

        let expected = creature_checksum(&opening).unwrap();
        for e in log.enhancements() {
            assert_eq!(e.meta.base_checksum, expected);
            assert!((e.meta.base_score - 0.5000).abs() < 1e-12);
            assert_eq!(e.meta.corpus_identity, CORPUS);
            assert_eq!(e.meta.input_count, opening.input);
            assert_eq!(e.meta.output_count, opening.output);
            assert_eq!(e.meta.producer, "neat-ai-forests/test");
            assert!(e.id_is_consistent(), "a filed id must match its payload");
        }
        assert!((log.enhancements()[1].meta.improved_score - 0.5150).abs() < 1e-12);
        assert_eq!(log.base_checksum(), expected);
        assert_eq!(log.corpus_identity(), CORPUS);
    }

    #[test]
    fn patches_are_filed_as_v1_forest_patches_in_acceptance_order() {
        let mut log = log();
        log.accept(&stump(1, 0.02), 0.51).unwrap();
        log.accept(&stump(0, 0.01), 0.52).unwrap();

        let bundle = log.bundle().expect("two patches were accepted");
        assert_eq!(
            bundle.version,
            crate::enhancement::ENHANCEMENT_FORMAT_VERSION
        );
        assert_eq!(
            filed_patches(&log),
            vec![&stump(1, 0.02), &stump(0, 0.01)],
            "acceptance order is preserved"
        );

        // And it is the documented wire form, readable by the consumer side.
        let text = serde_json::to_string(&bundle).unwrap();
        let parsed = EnhancementBundle::parse_json(&text).unwrap();
        assert_eq!(parsed, bundle);
        assert!(text.contains(r#""kind":"forestPatch""#), "{text}");
    }

    /// The whole point of filing rather than harvesting: the id the bundle
    /// carries is the id the graft names its structure with, so a champion that
    /// already carries the patch is recognised as carrying it.
    #[test]
    fn a_filed_id_is_the_id_the_graft_uses() {
        let patch = stump(0, 0.01);
        let mut log = log();
        let filed = log.accept(&patch, 0.51).unwrap().clone();
        assert_eq!(filed.meta.id, patch.id());

        let champion = evolved_descendant(2.0, 0.5);
        let application = crate::forest::apply(&patch, &champion).unwrap();
        let grafted = application
            .creature()
            .expect("the patch must graft onto the fixture");
        assert!(
            grafted
                .neurons
                .iter()
                .any(|n| n.uuid.starts_with(&format!("forest-{}-", filed.meta.id))),
            "the graft names its structure for the filed id"
        );
        assert!(crate::forest::is_present(&patch, grafted));
    }

    #[test]
    fn the_same_patch_filed_twice_fails_closed() {
        let mut log = log();
        log.accept(&stump(0, 0.01), 0.51).unwrap();
        // Provenance is not part of the identity, so a re-found patch is the
        // same patch and must not be filed twice.
        let mut refound = stump(0, 0.01);
        refound.provenance.strategy = "random-stump".into();
        let err = log.accept(&refound, 0.52).unwrap_err();
        assert!(matches!(err, PatchLogError::Duplicate { .. }), "{err}");
        assert_eq!(log.enhancements().len(), 1);

        // A different correction is a different enhancement.
        log.accept(&stump(0, 0.02), 0.53).unwrap();
        assert_eq!(log.enhancements().len(), 2);
    }

    #[test]
    fn a_combo_files_its_members_in_order_and_repeats_nothing() {
        let seed = stump(0, 0.01);
        let grown = stump(1, 0.02);
        let mut log = log();
        log.accept(&seed, 0.51).unwrap();

        // The boosting round verified [seed, grown] together: only `grown` is
        // new, and the bundle's two-member prefix now reproduces the combo.
        let newly = log
            .accept_combo(&[seed.clone(), grown.clone()], 0.55)
            .unwrap();
        assert_eq!(newly.len(), 1);
        assert_eq!(newly[0].meta.id, grown.id());
        assert!((newly[0].meta.improved_score - 0.55).abs() < 1e-12);
        assert_eq!(filed_patches(&log), vec![&seed, &grown]);
        assert!(
            (log.enhancements()[0].meta.improved_score - 0.51).abs() < 1e-12,
            "a member already filed keeps the score it was filed with"
        );

        // A combo naming the same patch twice is a mis-filing, not a prefix.
        let err = log
            .accept_combo(&[grown.clone(), grown.clone()], 0.56)
            .unwrap_err();
        assert!(matches!(err, PatchLogError::Duplicate { .. }), "{err}");
        let err = log.accept_combo(&[], 0.56).unwrap_err();
        assert!(matches!(err, PatchLogError::EmptyCombo), "{err}");
        assert_eq!(log.enhancements().len(), 2, "nothing partial is filed");
    }

    #[test]
    fn a_patch_the_opening_creature_cannot_carry_fails_closed() {
        let mut log = log();
        let err = log.accept(&stump(9, 0.01), 0.51).unwrap_err();
        assert!(
            matches!(err, PatchLogError::FeatureOutOfRange { feature: 9, .. }),
            "{err}"
        );

        let out_of_range = Patch::new(3, Node::stump(0, 0.25, 0.0, 0.01), Provenance::default());
        let err = log.accept(&out_of_range, 0.51).unwrap_err();
        assert!(
            matches!(err, PatchLogError::OutputOutOfRange { output: 3, .. }),
            "{err}"
        );

        let repeated = Patch::new(
            0,
            Node::Split {
                condition: Condition {
                    terms: vec![
                        Term {
                            feature: 0,
                            weight: 1.0,
                        },
                        Term {
                            feature: 0,
                            weight: -0.5,
                        },
                    ],
                    threshold: 0.0,
                },
                left: Box::new(Node::leaf(0.0)),
                right: Box::new(Node::leaf(0.01)),
            },
            Provenance::default(),
        );
        let err = log.accept(&repeated, 0.51).unwrap_err();
        assert!(matches!(err, PatchLogError::RepeatedFeature(0)), "{err}");
        assert!(log.is_empty(), "nothing partial is filed");
        assert!(log.bundle().is_none());
    }

    #[test]
    fn a_patch_the_graft_could_never_apply_fails_closed() {
        let mut log = log();
        let err = log
            .accept(
                &Patch::new(0, Node::leaf(0.01), Provenance::default()),
                0.51,
            )
            .unwrap_err();
        assert!(matches!(err, PatchLogError::BareLeafRoot), "{err}");

        let mut future = stump(0, 0.01);
        future.version = PATCH_FORMAT_VERSION + 1;
        let err = log.accept(&future, 0.51).unwrap_err();
        assert!(
            matches!(err, PatchLogError::UnsupportedPatchVersion { .. }),
            "{err}"
        );
        assert!(log.is_empty());
    }

    #[test]
    fn non_finite_scores_and_trees_fail_closed() {
        let err = PatchLog::opening(
            "neat-ai-forests/test",
            &evolved_descendant(2.0, 0.5),
            f64::NAN,
            CORPUS,
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                PatchLogError::NonFiniteScore {
                    field: "baseScore",
                    ..
                }
            ),
            "{err}"
        );

        let mut log = log();
        let err = log.accept(&stump(0, 0.01), f64::INFINITY).unwrap_err();
        assert!(
            matches!(
                err,
                PatchLogError::NonFiniteScore {
                    field: "improvedScore",
                    ..
                }
            ),
            "{err}"
        );
        let err = log.accept(&stump(0, f32::NAN), 0.51).unwrap_err();
        assert!(matches!(err, PatchLogError::NonFiniteTree), "{err}");
        assert!(log.is_empty(), "nothing partial is filed");
    }

    #[test]
    fn a_run_that_accepted_nothing_writes_no_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("enhancements.json");
        assert!(!log().write_bundle(&path).unwrap(), "nothing to file");
        assert!(
            !path.exists(),
            "an empty run must not leave a bundle behind"
        );

        let mut log = log();
        log.accept(&stump(0, 0.01), 0.51).unwrap();
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
        // graft onto a champion the run never saw.
        let mut log = log();
        log.accept(&stump(0, 0.01), 0.51).unwrap();
        log.accept(&stump(1, 0.02), 0.52).unwrap();
        let champion = evolved_descendant(2.0, 0.5);
        let outcome = crate::engine::rebase(&crate::engine::RebaseRequest {
            champion: &champion,
            enhancements: log.enhancements(),
            corpus_identity: CORPUS,
            max_candidates: 0,
        })
        .unwrap();
        for report in &outcome.reports {
            assert_eq!(report.outcome, crate::engine::EnhancementOutcome::Applied);
        }
        let full = outcome
            .cohort
            .iter()
            .find(|c| c.label == "bundle")
            .expect("both patches applied, so a full bundle candidate exists");
        assert_eq!(full.applied_ids.len(), 2);
        for patch in filed_patches(&log) {
            assert!(crate::forest::is_present(patch, &full.creature));
        }
    }
}
