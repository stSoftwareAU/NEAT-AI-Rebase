//! Compatibility checks that run before any adapter touches a champion
//! (Issue #1).
//!
//! Everything here is a **fail-closed** gate. An enhancement that does not
//! clear it is not attempted, not guessed at and not silently dropped: it is
//! recorded as incompatible with a reason a human can act on, and the rest of
//! the bundle carries on without it.
//!
//! The order is deliberate — cheapest and most fundamental first, so the
//! reason reported is the most useful one:
//!
//! 1. **format version** — a v2 envelope means this build does not know what
//!    the change is;
//! 2. **identity** — `meta.id` must be the id the payload actually has, or
//!    idempotence is defeated by a mis-filed name;
//! 3. **corpus identity** — a score measured on another corpus is not evidence
//!    about this one;
//! 4. **dimensions** — an enhancement written against a 42-input creature
//!    cannot address a 40-input champion;
//! 5. **operation-specific preconditions** — left to each adapter, surfaced as
//!    [`Incompatibility::Precondition`].

use std::fmt;

use neat_core::CreatureExport;

use crate::enhancement::{ENHANCEMENT_FORMAT_VERSION, Enhancement};

/// The champion an enhancement is being checked against, plus the corpus the
/// decision will be made on.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    /// The current global champion. Never modified.
    pub creature: &'a CreatureExport,
    /// Identity of the corpus the scorer will judge on.
    pub corpus_identity: &'a str,
}

impl<'a> Target<'a> {
    /// A target from a champion and a corpus identity.
    pub fn new(creature: &'a CreatureExport, corpus_identity: &'a str) -> Self {
        Self {
            creature,
            corpus_identity,
        }
    }
}

/// Why an enhancement cannot be attempted on a target.
#[derive(Debug, Clone, PartialEq)]
pub enum Incompatibility {
    /// The envelope is a version this build does not implement.
    UnsupportedVersion {
        /// Version found.
        found: u32,
        /// Version implemented.
        supported: u32,
    },
    /// `meta.id` is not the id the payload has.
    IdMismatch {
        /// The id the document claims.
        declared: String,
        /// The id the payload actually has.
        computed: String,
    },
    /// The enhancement was measured against a different corpus.
    CorpusMismatch {
        /// Corpus the enhancement was measured on.
        enhancement: String,
        /// Corpus the decision will be made on.
        target: String,
    },
    /// Input width differs between the opening creature and the champion.
    InputWidth {
        /// Width the enhancement was written against.
        enhancement: usize,
        /// Width the champion has.
        target: usize,
    },
    /// Output width differs between the opening creature and the champion.
    OutputWidth {
        /// Width the enhancement was written against.
        enhancement: usize,
        /// Width the champion has.
        target: usize,
    },
    /// An operation-specific precondition the adapter refused.
    Precondition(String),
}

impl fmt::Display for Incompatibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { found, supported } => write!(
                f,
                "enhancement format version {found} is not supported (this build implements {supported})"
            ),
            Self::IdMismatch { declared, computed } => write!(
                f,
                "enhancement id `{declared}` does not match its payload (`{computed}`); \
                 refusing, because idempotence relies on the id naming the change"
            ),
            Self::CorpusMismatch {
                enhancement,
                target,
            } => write!(
                f,
                "enhancement was measured on corpus `{enhancement}` but the decision is on `{target}`"
            ),
            Self::InputWidth {
                enhancement,
                target,
            } => write!(
                f,
                "enhancement expects {enhancement} inputs; the champion has {target}"
            ),
            Self::OutputWidth {
                enhancement,
                target,
            } => write!(
                f,
                "enhancement expects {enhancement} outputs; the champion has {target}"
            ),
            Self::Precondition(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Incompatibility {}

/// Run the checks every enhancement kind shares.
///
/// Operation-specific preconditions are the adapter's job; this is the common
/// gate the engine applies first so one adapter cannot skip it.
///
/// # Errors
///
/// The first [`Incompatibility`] found, in the documented order.
pub fn check_common(enhancement: &Enhancement, target: &Target<'_>) -> Result<(), Incompatibility> {
    if enhancement.meta.version != ENHANCEMENT_FORMAT_VERSION {
        return Err(Incompatibility::UnsupportedVersion {
            found: enhancement.meta.version,
            supported: ENHANCEMENT_FORMAT_VERSION,
        });
    }
    let computed = enhancement.stable_id();
    if enhancement.meta.id != computed {
        return Err(Incompatibility::IdMismatch {
            declared: enhancement.meta.id.clone(),
            computed,
        });
    }
    if enhancement.meta.corpus_identity != target.corpus_identity {
        return Err(Incompatibility::CorpusMismatch {
            enhancement: enhancement.meta.corpus_identity.clone(),
            target: target.corpus_identity.to_string(),
        });
    }
    if enhancement.meta.input_count != target.creature.input {
        return Err(Incompatibility::InputWidth {
            enhancement: enhancement.meta.input_count,
            target: target.creature.input,
        });
    }
    if enhancement.meta.output_count != target.creature.output {
        return Err(Incompatibility::OutputWidth {
            enhancement: enhancement.meta.output_count,
            target: target.creature.output,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enhancement::{Payload, ProducerContext, RemovalStrategy};
    use crate::fixtures::linear_hidden_creature;
    use crate::patch::{Node, Patch, Provenance};

    fn forest_enhancement() -> Enhancement {
        Enhancement::new(
            Payload::ForestPatch {
                patch: Patch::new(0, Node::stump(0, 0.25, 0.0, 0.01), Provenance::default()),
            },
            &ProducerContext {
                producer: "neat-ai-forests/test".into(),
                base_checksum: "base".into(),
                base_score: 0.5,
                improved_score: 0.6,
                corpus_identity: "corpus-1".into(),
                input_count: 2,
                output_count: 1,
            },
        )
    }

    #[test]
    fn a_matching_enhancement_passes() {
        let champion = linear_hidden_creature(2.0);
        let target = Target::new(&champion, "corpus-1");
        check_common(&forest_enhancement(), &target).unwrap();
    }

    #[test]
    fn a_future_version_fails_closed() {
        let champion = linear_hidden_creature(2.0);
        let mut e = forest_enhancement();
        e.meta.version = 2;
        assert_eq!(
            check_common(&e, &Target::new(&champion, "corpus-1")),
            Err(Incompatibility::UnsupportedVersion {
                found: 2,
                supported: 1
            })
        );
    }

    #[test]
    fn a_mis_filed_id_fails_closed() {
        let champion = linear_hidden_creature(2.0);
        let mut e = forest_enhancement();
        let real = e.meta.id.clone();
        e.meta.id = "0000000000000000".into();
        assert_eq!(
            check_common(&e, &Target::new(&champion, "corpus-1")),
            Err(Incompatibility::IdMismatch {
                declared: "0000000000000000".into(),
                computed: real
            })
        );
    }

    #[test]
    fn corpus_drift_fails_closed() {
        let champion = linear_hidden_creature(2.0);
        let e = forest_enhancement();
        let err = check_common(&e, &Target::new(&champion, "corpus-2")).unwrap_err();
        assert!(
            matches!(err, Incompatibility::CorpusMismatch { .. }),
            "{err}"
        );
    }

    #[test]
    fn dimension_drift_fails_closed() {
        let champion = linear_hidden_creature(2.0);
        let mut e = forest_enhancement();
        e.meta.input_count = 40;
        e.meta.id = e.stable_id();
        assert_eq!(
            check_common(&e, &Target::new(&champion, "corpus-1")),
            Err(Incompatibility::InputWidth {
                enhancement: 40,
                target: 2
            })
        );

        let mut e = forest_enhancement();
        e.meta.output_count = 3;
        assert_eq!(
            check_common(&e, &Target::new(&champion, "corpus-1")),
            Err(Incompatibility::OutputWidth {
                enhancement: 3,
                target: 1
            })
        );
    }

    #[test]
    fn the_gate_runs_before_any_operation_specific_check() {
        // A removal naming a neuron the champion does not have is still
        // reported as a corpus mismatch when both are wrong: the common gate
        // runs first, so the most fundamental reason is the one reported.
        let champion = linear_hidden_creature(2.0);
        let e = Enhancement::new(
            Payload::OckhamRemoval {
                removal: crate::enhancement::OckhamRemoval {
                    neuron_uuid: "not-here".into(),
                    strategy: RemovalStrategy::IdentityCollapse,
                },
            },
            &ProducerContext {
                producer: "neat-ai-ockham/test".into(),
                base_checksum: "base".into(),
                base_score: 0.5,
                improved_score: 0.6,
                corpus_identity: "corpus-other".into(),
                input_count: 2,
                output_count: 1,
            },
        );
        assert!(matches!(
            check_common(&e, &Target::new(&champion, "corpus-1")),
            Err(Incompatibility::CorpusMismatch { .. })
        ));
    }
}
