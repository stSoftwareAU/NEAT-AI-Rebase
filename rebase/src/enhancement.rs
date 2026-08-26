//! The v1 portable enhancement contract (Issue #1).
//!
//! An enhancement describes **what an optimiser changed**, not the creature it
//! ended up with. That is the whole point of Rebase: the improved creature is
//! evidence, the change is the portable artefact, and the change is what gets
//! replayed onto whatever champion the fleet has reached by the time the
//! optimiser finishes.
//!
//! ## Wire form
//!
//! ```json
//! {
//!   "meta": {
//!     "version": 1,
//!     "id": "0f3a9c1b2d4e5f60",
//!     "producer": "neat-ai-forests/0.1.17",
//!     "baseChecksum": "9a2f…",
//!     "baseScore": 0.81234,
//!     "improvedScore": 0.81290,
//!     "corpusIdentity": "3f2a1b0c9d8e7f65",
//!     "inputCount": 42,
//!     "outputCount": 1
//!   },
//!   "payload": { "kind": "forestPatch", "patch": { "version": 1, "output": 0, "root": {…} } }
//! }
//! ```
//!
//! ## Which fields participate in identity
//!
//! Only the **semantic change** does. [`Enhancement::stable_id`] is computed
//! from the payload alone:
//!
//! | Kind | Identity is derived from | Deliberately excluded |
//! | --- | --- | --- |
//! | `forestPatch` | the patch's `output` and correction `root` — [`Patch::id`] | provenance, scores, producer, base checksum, corpus |
//! | `ockhamRemoval` | the neuron UUID and the removal strategy name | the measured `mean`, scores, producer, base checksum, corpus |
//!
//! Two producers that discover the same correction file the same id, so a
//! champion that already carries one is recognised as carrying the other. The
//! Ockham `mean` is excluded because it is a *measurement* of one corpus pass,
//! not part of what was decided: the same neuron removed by the same strategy
//! is the same enhancement even when a later run measures a different mean.
//!
//! `meta.id` is checked against [`Enhancement::stable_id`] before anything is
//! applied ([`crate::compat`]), so a hand-edited or mis-filed id fails closed
//! rather than defeating idempotence.
//!
//! ## What v1 deliberately does not carry
//!
//! No generic bias / weight / squash mutation. Their rebase semantics are not
//! yet explicit — "set weight w to 0.31" says nothing useful about a champion
//! that has independently retrained that weight — so they are left out until
//! they can be defined and scorer-tested rather than guessed.

use serde::{Deserialize, Serialize};

use crate::creature::sha256_hex;
use crate::patch::Patch;

/// Version of the portable enhancement envelope this build understands.
///
/// Anything else fails closed: see [`EnhancementError::UnsupportedVersion`].
pub const ENHANCEMENT_FORMAT_VERSION: u32 = 1;

/// Provenance shared by every enhancement kind.
///
/// Everything here is **evidence**. `improvedScore` says the producer's scorer
/// preferred the change on `baseChecksum`; it is never a reason to promote the
/// change on a different champion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnhancementMeta {
    /// Envelope format version. Must be [`ENHANCEMENT_FORMAT_VERSION`].
    pub version: u32,
    /// Stable identity of the semantic change — see [`Enhancement::stable_id`].
    pub id: String,
    /// Who produced it, e.g. `neat-ai-forests/0.1.17`.
    pub producer: String,
    /// SHA-256 of the opening creature the producer started from.
    pub base_checksum: String,
    /// Authoritative score of that opening creature.
    pub base_score: f64,
    /// Authoritative score the producer measured after applying this change.
    pub improved_score: f64,
    /// Identity of the training corpus both scores were measured on.
    pub corpus_identity: String,
    /// Input width of the opening creature.
    pub input_count: usize,
    /// Output width of the opening creature.
    pub output_count: usize,
}

impl EnhancementMeta {
    /// The improvement the producer measured on its own opening creature.
    ///
    /// Reported in the journal so a human can see how much was claimed against
    /// how much the rebased candidate actually delivered.
    pub fn claimed_gain(&self) -> f64 {
        self.improved_score - self.base_score
    }
}

/// How an Ockham removal reproduces its transformation.
///
/// The UUID alone is not enough: "remove `h7`" has at least two safe readings,
/// and replaying the wrong one produces a different creature from the one the
/// scorer accepted. The strategy names which one, and Rebase refuses a
/// strategy it cannot reproduce rather than substituting the other.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "strategy", rename_all = "camelCase")]
pub enum RemovalStrategy {
    /// Replace the neuron's downstream contribution with its measured mean
    /// post-activation: `bias_j += mean_i * w_ij` for every outgoing synapse,
    /// then cascade-clean whatever that left dead.
    ///
    /// Deliberately approximate. The mean is the producer's full-corpus
    /// measurement on **its** opening creature; on a newer champion the same
    /// neuron may activate differently, which is exactly why the result is
    /// scored rather than trusted.
    MeanAblation {
        /// Full-corpus mean post-activation the producer measured.
        mean: f64,
    },
    /// Collapse a hidden IDENTITY neuron exactly: fold its bias into each
    /// downstream neuron and bypass `x → y → z` as `x → z` with the product
    /// weight. Exact, so no measurement is needed.
    IdentityCollapse,
}

impl RemovalStrategy {
    /// Stable label used in identity, journals and error text.
    pub fn label(&self) -> &'static str {
        match self {
            Self::MeanAblation { .. } => "meanAblation",
            Self::IdentityCollapse => "identityCollapse",
        }
    }
}

/// A scorer-proven NEAT-AI-Ockham removal, described so it can be replayed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OckhamRemoval {
    /// UUID of the hidden neuron that was removed.
    pub neuron_uuid: String,
    /// The transformation to reproduce.
    #[serde(flatten)]
    pub strategy: RemovalStrategy,
}

/// The v1 payload types.
///
/// Internally tagged by `kind`, so an unknown kind fails to deserialise rather
/// than arriving as a half-understood enhancement — the fail-closed rule of
/// Issue #1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Payload {
    /// A portable NEAT-AI-Forests residual-correction graft.
    ForestPatch {
        /// The patch itself, in the Forests wire format.
        patch: Patch,
    },
    /// A scorer-proven NEAT-AI-Ockham removal.
    OckhamRemoval {
        /// The removal, by UUID and strategy.
        #[serde(flatten)]
        removal: OckhamRemoval,
    },
}

impl Payload {
    /// Stable label for journals and error text.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ForestPatch { .. } => "forestPatch",
            Self::OckhamRemoval { .. } => "ockhamRemoval",
        }
    }
}

/// The run-level facts a producer stamps on every enhancement it files.
///
/// One value per optimiser run: the opening creature it started from, the two
/// authoritative scores that bracket the change, the corpus both were measured
/// on, and the widths the change was written against. Passing it as one value
/// rather than seven positional arguments is not tidiness — `base_score` and
/// `improved_score`, and `input_count` and `output_count`, are adjacent and
/// same-typed, and transposing either pair would silently mis-file a bundle.
#[derive(Debug, Clone, PartialEq)]
pub struct ProducerContext {
    /// Who produced the run, e.g. `neat-ai-forests/0.1.17`.
    pub producer: String,
    /// SHA-256 of the creature the run opened on.
    pub base_checksum: String,
    /// Authoritative score of that opening creature.
    pub base_score: f64,
    /// Authoritative score measured after applying the change.
    pub improved_score: f64,
    /// Identity of the corpus both scores were measured on.
    pub corpus_identity: String,
    /// Input width of the opening creature.
    pub input_count: usize,
    /// Output width of the opening creature.
    pub output_count: usize,
}

/// One portable, versioned enhancement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Enhancement {
    /// Provenance and compatibility facts.
    pub meta: EnhancementMeta,
    /// The semantic change.
    pub payload: Payload,
}

/// An ordered bundle of enhancements from one producer run.
///
/// Order is the order the producer accepted them, and it is preserved: the
/// engine's cumulative prefixes only mean something if "the first two" means
/// the same thing to producer and consumer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnhancementBundle {
    /// Envelope format version. Must be [`ENHANCEMENT_FORMAT_VERSION`].
    pub version: u32,
    /// Who produced the run.
    pub producer: String,
    /// SHA-256 of the creature the run opened on.
    pub base_checksum: String,
    /// Authoritative score of that opening creature.
    pub base_score: f64,
    /// Identity of the corpus the run measured against.
    pub corpus_identity: String,
    /// Accepted enhancements, in acceptance order.
    pub enhancements: Vec<Enhancement>,
}

/// Why an enhancement or bundle was refused before anything was attempted.
#[derive(Debug, Clone, PartialEq)]
pub enum EnhancementError {
    /// Malformed JSON, or a `kind` this build does not implement.
    Malformed(String),
    /// A format version this build does not implement.
    UnsupportedVersion {
        /// Version found in the document.
        found: u32,
        /// Version this build implements.
        supported: u32,
    },
}

impl std::fmt::Display for EnhancementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(m) => write!(f, "malformed enhancement: {m}"),
            Self::UnsupportedVersion { found, supported } => write!(
                f,
                "enhancement format version {found} is not supported (this build implements {supported})"
            ),
        }
    }
}

impl std::error::Error for EnhancementError {}

impl Enhancement {
    /// Identity of the semantic change, independent of who found it.
    ///
    /// See the module documentation for which fields participate.
    pub fn stable_id(&self) -> String {
        match &self.payload {
            Payload::ForestPatch { patch } => patch.id(),
            Payload::OckhamRemoval { removal } => {
                let canon = serde_json::to_string(&(
                    "ockhamRemoval",
                    &removal.neuron_uuid,
                    removal.strategy.label(),
                ))
                .unwrap_or_default();
                sha256_hex(canon.as_bytes())[..16].to_string()
            }
        }
    }

    /// `true` when `meta.id` is the id the payload actually has.
    pub fn id_is_consistent(&self) -> bool {
        self.meta.id == self.stable_id()
    }

    /// Build the envelope for `payload`, filling in the id from the payload so
    /// a producer cannot file an inconsistent one by accident.
    pub fn new(payload: Payload, context: &ProducerContext) -> Self {
        let mut e = Self {
            meta: EnhancementMeta {
                version: ENHANCEMENT_FORMAT_VERSION,
                id: String::new(),
                producer: context.producer.clone(),
                base_checksum: context.base_checksum.clone(),
                base_score: context.base_score,
                improved_score: context.improved_score,
                corpus_identity: context.corpus_identity.clone(),
                input_count: context.input_count,
                output_count: context.output_count,
            },
            payload,
        };
        e.meta.id = e.stable_id();
        e
    }

    /// Parse one enhancement, refusing an unsupported version or an unknown
    /// `kind`.
    ///
    /// # Errors
    ///
    /// [`EnhancementError::Malformed`] for bad JSON or an unimplemented kind,
    /// [`EnhancementError::UnsupportedVersion`] for a future envelope.
    pub fn parse_json(text: &str) -> Result<Self, EnhancementError> {
        // The version is read before the body, so a v2 document with a v2
        // payload shape is refused as "unsupported version" rather than as
        // whichever field happens to be missing from the v1 shape.
        check_version(text)?;
        serde_json::from_str(text).map_err(|e| EnhancementError::Malformed(e.to_string()))
    }
}

impl EnhancementBundle {
    /// A bundle carrying `enhancements`, with the run-level facts taken from
    /// the first of them.
    ///
    /// # Panics
    ///
    /// Panics on an empty slice — a bundle exists to carry accepted changes,
    /// and the run-level facts have nowhere to come from without one. Producers
    /// with nothing to report skip the rebase call instead.
    pub fn from_enhancements(enhancements: Vec<Enhancement>) -> Self {
        let first = enhancements
            .first()
            .expect("a bundle carries at least one enhancement");
        Self {
            version: ENHANCEMENT_FORMAT_VERSION,
            producer: first.meta.producer.clone(),
            base_checksum: first.meta.base_checksum.clone(),
            base_score: first.meta.base_score,
            corpus_identity: first.meta.corpus_identity.clone(),
            enhancements,
        }
    }

    /// Parse a bundle, refusing an unsupported version or an unknown `kind`.
    ///
    /// # Errors
    ///
    /// [`EnhancementError::Malformed`] for bad JSON or an unimplemented kind,
    /// [`EnhancementError::UnsupportedVersion`] for a future envelope — either
    /// at the bundle level or inside any member.
    pub fn parse_json(text: &str) -> Result<Self, EnhancementError> {
        check_version(text)?;
        let bundle: Self =
            serde_json::from_str(text).map_err(|e| EnhancementError::Malformed(e.to_string()))?;
        for e in &bundle.enhancements {
            if e.meta.version != ENHANCEMENT_FORMAT_VERSION {
                return Err(EnhancementError::UnsupportedVersion {
                    found: e.meta.version,
                    supported: ENHANCEMENT_FORMAT_VERSION,
                });
            }
        }
        Ok(bundle)
    }
}

/// Read the top-level `version` (bundle) or `meta.version` (enhancement) and
/// refuse anything this build does not implement.
fn check_version(text: &str) -> Result<(), EnhancementError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| EnhancementError::Malformed(e.to_string()))?;
    let found = value
        .get("version")
        .or_else(|| value.get("meta").and_then(|m| m.get("version")))
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| EnhancementError::Malformed("no format version".into()))?;
    if found != u64::from(ENHANCEMENT_FORMAT_VERSION) {
        return Err(EnhancementError::UnsupportedVersion {
            found: found as u32,
            supported: ENHANCEMENT_FORMAT_VERSION,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::{Node, Provenance};

    fn forest(output: usize, feature: usize) -> Enhancement {
        Enhancement::new(
            Payload::ForestPatch {
                patch: Patch::new(
                    output,
                    Node::stump(feature, 0.25, 0.0, 0.01),
                    Provenance::default(),
                ),
            },
            &ProducerContext {
                producer: "neat-ai-forests/test".into(),
                base_checksum: "base-checksum".into(),
                base_score: 0.5,
                improved_score: 0.6,
                corpus_identity: "corpus-1".into(),
                input_count: 4,
                output_count: 1,
            },
        )
    }

    fn removal(uuid: &str, strategy: RemovalStrategy) -> Enhancement {
        Enhancement::new(
            Payload::OckhamRemoval {
                removal: OckhamRemoval {
                    neuron_uuid: uuid.into(),
                    strategy,
                },
            },
            &ProducerContext {
                producer: "neat-ai-ockham/test".into(),
                base_checksum: "base-checksum".into(),
                base_score: 0.5,
                improved_score: 0.6,
                corpus_identity: "corpus-1".into(),
                input_count: 4,
                output_count: 1,
            },
        )
    }

    #[test]
    fn round_trips_through_json() {
        for e in [
            forest(0, 3),
            removal("h7", RemovalStrategy::MeanAblation { mean: 0.125 }),
            removal("h9", RemovalStrategy::IdentityCollapse),
        ] {
            let text = serde_json::to_string(&e).unwrap();
            assert_eq!(Enhancement::parse_json(&text).unwrap(), e);
        }
    }

    #[test]
    fn wire_form_is_the_documented_shape() {
        let e = removal("h7", RemovalStrategy::MeanAblation { mean: 0.125 });
        let text = serde_json::to_string(&e).unwrap();
        assert!(text.contains(r#""kind":"ockhamRemoval""#), "{text}");
        assert!(text.contains(r#""strategy":"meanAblation""#), "{text}");
        assert!(text.contains(r#""neuronUuid":"h7""#), "{text}");
        assert!(text.contains(r#""mean":0.125"#), "{text}");
        let f = forest(0, 3);
        let text = serde_json::to_string(&f).unwrap();
        assert!(text.contains(r#""kind":"forestPatch""#), "{text}");
        assert!(text.contains(r#""corpusIdentity":"corpus-1""#), "{text}");
    }

    #[test]
    fn identical_semantic_changes_share_an_id() {
        // Same tree, different producer, scores and provenance.
        let mut a = forest(0, 3);
        let mut b = forest(0, 3);
        b.meta.producer = "someone-else/9.9".into();
        b.meta.base_checksum = "different".into();
        b.meta.improved_score = 0.99;
        if let Payload::ForestPatch { patch } = &mut b.payload {
            patch.provenance.strategy = "random-stump".into();
            patch.provenance.seed = Some(42);
        }
        assert_eq!(a.stable_id(), b.stable_id());
        assert!(a.id_is_consistent() && b.id_is_consistent());

        // A different tree is a different enhancement.
        a.payload = forest(0, 4).payload;
        assert_ne!(a.stable_id(), b.stable_id());
    }

    #[test]
    fn removal_identity_ignores_the_measured_mean_but_not_the_strategy() {
        let a = removal("h7", RemovalStrategy::MeanAblation { mean: 0.1 });
        let b = removal("h7", RemovalStrategy::MeanAblation { mean: 0.9 });
        assert_eq!(a.stable_id(), b.stable_id());

        let c = removal("h7", RemovalStrategy::IdentityCollapse);
        assert_ne!(a.stable_id(), c.stable_id());

        let d = removal("h8", RemovalStrategy::MeanAblation { mean: 0.1 });
        assert_ne!(a.stable_id(), d.stable_id());
    }

    #[test]
    fn unknown_kind_fails_closed() {
        let text = r#"{"meta":{"version":1,"id":"x","producer":"p","baseChecksum":"c",
            "baseScore":0.1,"improvedScore":0.2,"corpusIdentity":"k","inputCount":1,"outputCount":1},
            "payload":{"kind":"weightNudge","from":"a","to":"b","weight":0.5}}"#;
        let err = Enhancement::parse_json(text).unwrap_err();
        assert!(
            matches!(err, EnhancementError::Malformed(_)),
            "unknown kinds must not parse: {err}"
        );
    }

    #[test]
    fn unknown_version_fails_closed() {
        let mut e = forest(0, 3);
        e.meta.version = 2;
        let text = serde_json::to_string(&e).unwrap();
        assert_eq!(
            Enhancement::parse_json(&text).unwrap_err(),
            EnhancementError::UnsupportedVersion {
                found: 2,
                supported: 1
            }
        );
    }

    #[test]
    fn bundle_round_trips_and_refuses_a_future_member() {
        let bundle = EnhancementBundle::from_enhancements(vec![
            forest(0, 3),
            removal("h7", RemovalStrategy::IdentityCollapse),
        ]);
        let text = serde_json::to_string(&bundle).unwrap();
        assert_eq!(EnhancementBundle::parse_json(&text).unwrap(), bundle);

        let mut future = bundle.clone();
        future.enhancements[1].meta.version = 7;
        let text = serde_json::to_string(&future).unwrap();
        assert_eq!(
            EnhancementBundle::parse_json(&text).unwrap_err(),
            EnhancementError::UnsupportedVersion {
                found: 7,
                supported: 1
            }
        );
    }

    #[test]
    fn claimed_gain_is_the_producers_own_delta() {
        let e = forest(0, 3);
        assert!((e.meta.claimed_gain() - 0.1).abs() < 1e-12);
    }

    #[test]
    fn payloads_carry_no_application_domain_assumptions() {
        // Features and outputs are plain indices; the removal is a plain UUID.
        // Nothing in the v1 payloads names a domain, a units system or a
        // corpus layout — the format is as usable for a weather model as for
        // anything else.
        let text = serde_json::to_string(&forest(0, 3)).unwrap()
            + &serde_json::to_string(&removal("h7", RemovalStrategy::IdentityCollapse)).unwrap();
        for word in ["stock", "price", "market", "ticker", "share"] {
            assert!(!text.to_lowercase().contains(word), "leaked `{word}`");
        }
    }
}
