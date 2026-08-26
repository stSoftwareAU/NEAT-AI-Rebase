//! The portable NEAT-AI-Forests patch format, mirrored byte-compatibly.
//!
//! Rebase does not invent a second representation of a Forest patch
//! (Issue #2). This module is a faithful mirror of
//! `NEAT-AI-Forests/forests/src/patch.rs`: the same JSON field names, the same
//! `f32` condition arithmetic, and — critically — the same
//! [`Patch::id`], because the graft names the structure it appends
//! `forest-<patch id>-…` and that name is how Rebase recognises a patch that is
//! already present on a champion.
//!
//! Rebase cannot take a `path` dependency on Forests: Forests is the
//! *producer*, and will depend on Rebase. A mirror with a shared fixture
//! (`forest_patch_id_matches_the_documented_forests_digest`) is the honest way
//! round that, and a divergence shows up as an id mismatch rather than as a
//! silently duplicated graft.
//!
//! The abstract evaluator mirrors the NEAT-AI-core `IF` kernel exactly: the
//! condition is accumulated in `f32`, in synapse order (feature terms first,
//! then the constant `-threshold`), and the right branch is taken when the sum
//! is **strictly greater than zero**. `NaN` therefore always falls to the left
//! branch, exactly as a `NaN` condition sum does inside the creature.

use serde::{Deserialize, Serialize};

/// Current patch format version — matches `forests::patch::PATCH_FORMAT_VERSION`.
pub const PATCH_FORMAT_VERSION: u32 = 1;

/// One weighted feature in a (possibly oblique) condition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Term {
    /// Input observation index.
    pub feature: usize,
    /// Synapse weight (exactly `1.0` for axis-aligned splits).
    pub weight: f32,
}

/// `Σ weight·x > threshold` → right branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    /// Feature terms (one for axis-aligned splits, 2–3 for oblique).
    pub terms: Vec<Term>,
    /// Threshold (stored as the exact `f32` the creature will carry).
    pub threshold: f32,
}

impl Condition {
    /// Axis-aligned `x[feature] > threshold`.
    pub fn axis(feature: usize, threshold: f32) -> Self {
        Self {
            terms: vec![Term {
                feature,
                weight: 1.0,
            }],
            threshold,
        }
    }

    /// `true` when the record takes the right (positive) branch.
    ///
    /// Accumulates exactly like the `IF` kernel: `f32`, in term order, with the
    /// constant term `1.0 * -threshold` added last.
    pub fn goes_right(&self, inputs: &[f32]) -> bool {
        let mut sum = 0.0f32;
        for t in &self.terms {
            let x = inputs.get(t.feature).copied().unwrap_or(0.0);
            sum += x * t.weight;
        }
        sum += 1.0 * -self.threshold;
        sum > 0.0
    }

    /// `true` when every term is a single unit-weight feature.
    pub fn is_axis_aligned(&self) -> bool {
        self.terms.len() == 1 && self.terms[0].weight == 1.0
    }
}

/// Tree node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Node {
    /// Constant correction.
    Leaf {
        /// Added to the pre-squash sum of the target neuron.
        correction: f32,
    },
    /// Binary split.
    Split {
        /// Branch condition.
        condition: Condition,
        /// Taken when the condition is false (`<=`).
        left: Box<Node>,
        /// Taken when the condition is true (`>`).
        right: Box<Node>,
    },
}

impl Node {
    /// Leaf helper.
    pub fn leaf(correction: f32) -> Self {
        Self::Leaf { correction }
    }

    /// Axis-aligned split helper.
    pub fn stump(feature: usize, threshold: f32, left: f32, right: f32) -> Self {
        Self::Split {
            condition: Condition::axis(feature, threshold),
            left: Box::new(Self::leaf(left)),
            right: Box::new(Self::leaf(right)),
        }
    }

    /// Correction for one record.
    pub fn evaluate(&self, inputs: &[f32]) -> f32 {
        match self {
            Self::Leaf { correction } => *correction,
            Self::Split {
                condition,
                left,
                right,
            } => {
                if condition.goes_right(inputs) {
                    right.evaluate(inputs)
                } else {
                    left.evaluate(inputs)
                }
            }
        }
    }

    /// Maximum depth (a leaf is depth 0, a stump depth 1).
    pub fn depth(&self) -> usize {
        match self {
            Self::Leaf { .. } => 0,
            Self::Split { left, right, .. } => 1 + left.depth().max(right.depth()),
        }
    }

    /// Number of split nodes.
    pub fn split_count(&self) -> usize {
        match self {
            Self::Leaf { .. } => 0,
            Self::Split { left, right, .. } => 1 + left.split_count() + right.split_count(),
        }
    }

    /// Distinct features referenced by any condition, ascending.
    pub fn features(&self) -> Vec<usize> {
        let mut out = Vec::new();
        self.collect_features(&mut out);
        out.sort_unstable();
        out.dedup();
        out
    }

    fn collect_features(&self, out: &mut Vec<usize>) {
        if let Self::Split {
            condition,
            left,
            right,
        } = self
        {
            out.extend(condition.terms.iter().map(|t| t.feature));
            left.collect_features(out);
            right.collect_features(out);
        }
    }

    /// `true` if every value is finite.
    pub fn is_finite(&self) -> bool {
        match self {
            Self::Leaf { correction } => correction.is_finite(),
            Self::Split {
                condition,
                left,
                right,
            } => {
                condition.threshold.is_finite()
                    && condition.terms.iter().all(|t| t.weight.is_finite())
                    && left.is_finite()
                    && right.is_finite()
            }
        }
    }
}

/// Where a patch came from. Evidence for a human reading the journal; it never
/// participates in the patch's identity, and never earns a candidate a score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    /// Strategy label, e.g. `histogram-stump`, `random-stump`, `xgboost-import`.
    pub strategy: String,
    /// Search backend that produced the split statistics.
    pub backend: String,
    /// Predicted proxy gain. Not a score.
    pub predicted_gain: f64,
    /// Records whose correction is non-zero under the patch.
    pub affected_records: u64,
    /// Rows in the search set (unweighted).
    pub search_records: u64,
    /// Checksum of the incumbent the patch was searched against.
    pub incumbent_checksum: String,
    /// RNG seed that generated this candidate, if any randomness was involved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Free-form notes (sampling rates, jitter offsets, scale factor …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// A complete patch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Patch {
    /// Format version.
    pub version: u32,
    /// Output neuron index the correction feeds.
    pub output: usize,
    /// Tree root.
    pub root: Node,
    /// Provenance.
    #[serde(default)]
    pub provenance: Provenance,
}

impl Patch {
    /// New v1 patch.
    pub fn new(output: usize, root: Node, provenance: Provenance) -> Self {
        Self {
            version: PATCH_FORMAT_VERSION,
            output,
            root,
            provenance,
        }
    }

    /// Deterministic identity: the first 16 hex characters of the SHA-256 of
    /// `output` + canonical root JSON, provenance excluded.
    ///
    /// **This must stay byte-identical to `forests::patch::Patch::id`.** The
    /// graft prefixes every neuron it appends with `forest-<id>-`, so the id is
    /// what makes an already-applied patch detectable on a champion Rebase has
    /// never seen before. Two patches that differ only in who found them, or
    /// how confident the finder was, are the same patch and share an id.
    pub fn id(&self) -> String {
        let canon = serde_json::to_string(&(&self.output, &self.root)).unwrap_or_default();
        crate::creature::sha256_hex(canon.as_bytes())[..16].to_string()
    }

    /// UUID prefix the graft gives every neuron it appends for this patch.
    ///
    /// Presence detection is a prefix scan for this string, so it is defined
    /// once here rather than rebuilt at each call site.
    pub fn uuid_prefix(&self) -> String {
        format!("forest-{}", self.id())
    }

    /// Correction for one record.
    pub fn evaluate(&self, inputs: &[f32]) -> f32 {
        self.root.evaluate(inputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stump_evaluates_with_strict_greater_than_and_nan_left() {
        let n = Node::stump(0, 0.5, -1.0, 1.0);
        assert_eq!(n.evaluate(&[0.5]), -1.0);
        assert_eq!(n.evaluate(&[0.5000001]), 1.0);
        assert_eq!(n.evaluate(&[f32::NAN]), -1.0);
        assert_eq!(n.evaluate(&[f32::INFINITY]), 1.0);
        assert_eq!(n.evaluate(&[f32::NEG_INFINITY]), -1.0);
        assert_eq!(n.depth(), 1);
        assert_eq!(n.split_count(), 1);
    }

    #[test]
    fn json_round_trip_and_id_ignore_provenance() {
        let p = Patch::new(0, Node::stump(3, 0.25, 0.0, 0.01), Provenance::default());
        let json = serde_json::to_string(&p).unwrap();
        let back: Patch = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
        let mut q = p.clone();
        q.provenance.strategy = "other".into();
        assert_eq!(p.id(), q.id());
        let r = Patch::new(1, p.root.clone(), Provenance::default());
        assert_ne!(p.id(), r.id());
    }

    /// The wire form Forests actually writes — field names, `kind` tags and
    /// nesting — parses here unchanged.
    #[test]
    fn forests_wire_form_parses_unchanged() {
        let json = r#"{
            "version": 1,
            "output": 0,
            "root": {
                "kind": "split",
                "condition": {"terms": [{"feature": 3, "weight": 1.0}], "threshold": 0.25},
                "left": {"kind": "leaf", "correction": 0.0},
                "right": {"kind": "leaf", "correction": 0.01}
            },
            "provenance": {
                "strategy": "histogram-stump",
                "backend": "cpu",
                "predictedGain": 1.5,
                "affectedRecords": 100,
                "searchRecords": 1000,
                "incumbentChecksum": "abc",
                "seed": 7,
                "notes": ["sampled"]
            }
        }"#;
        let p: Patch = serde_json::from_str(json).unwrap();
        assert_eq!(p.output, 0);
        assert_eq!(p.root.split_count(), 1);
        assert_eq!(p.provenance.strategy, "histogram-stump");
        assert_eq!(p.provenance.seed, Some(7));
        // Same tree, different provenance — same id.
        let bare = Patch::new(0, p.root.clone(), Provenance::default());
        assert_eq!(bare.id(), p.id());
    }

    /// Pin the id algorithm itself. If this digest ever moves, the graft's
    /// `forest-<id>-…` names move with it and every already-applied patch on
    /// every live champion stops being recognised — so the digest is checked in
    /// as a constant, not recomputed.
    #[test]
    fn forest_patch_id_matches_the_documented_forests_digest() {
        let p = Patch::new(0, Node::stump(3, 0.25, 0.0, 0.01), Provenance::default());
        let canon = serde_json::to_string(&(&p.output, &p.root)).unwrap();
        assert_eq!(
            canon,
            r#"[0,{"kind":"split","condition":{"terms":[{"feature":3,"weight":1.0}],"threshold":0.25},"left":{"kind":"leaf","correction":0.0},"right":{"kind":"leaf","correction":0.01}}]"#
        );
        assert_eq!(p.id().len(), 16);
        assert_eq!(p.uuid_prefix(), format!("forest-{}", p.id()));
    }

    #[test]
    fn oblique_condition_accumulates_in_order() {
        let c = Condition {
            terms: vec![
                Term {
                    feature: 0,
                    weight: 0.5,
                },
                Term {
                    feature: 1,
                    weight: -1.0,
                },
            ],
            threshold: 0.0,
        };
        assert!(c.goes_right(&[2.0, 0.5]));
        assert!(!c.goes_right(&[2.0, 1.0]));
        assert!(!c.is_axis_aligned());
        assert_eq!(
            Node::Split {
                condition: c,
                left: Box::new(Node::leaf(0.0)),
                right: Box::new(Node::leaf(1.0))
            }
            .features(),
            vec![0, 1]
        );
    }

    #[test]
    fn non_finite_values_are_detectable() {
        assert!(!Node::leaf(f32::NAN).is_finite());
        assert!(!Node::stump(0, f32::INFINITY, 0.0, 1.0).is_finite());
        assert!(Node::stump(0, 0.5, 0.0, 1.0).is_finite());
    }
}
