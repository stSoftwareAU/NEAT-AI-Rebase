//! The NEAT-AI-Forests graft adapter (Issue #2).
//!
//! A Forest patch is a small tree of residual corrections for one output. This
//! module replays it onto **whatever champion the fleet has reached**, using
//! the same layout Forests itself emits, so a patch accepted on ancestor `A`
//! lands on descendant `B` as the same structure it would have landed on `A`.
//!
//! ## Layout
//!
//! Every neuron the graft appends is named `forest-<patch id>-…`:
//!
//! ```text
//! shared:     three bias-1 constants, one per synapse role, reused from the
//!             creature's own bias-1 constants where it has them, else created
//!             once as `forest-one-a/b/c` (GraftConstants::Shared, the default)
//! per split:  ifN     hidden IF, bias 0
//!                 condition:  input-f (weight w_f per term) and the condition
//!                             constant (weight = −threshold)
//!                 positive:   right child ifN (weight 1) | positive constant
//!                             (weight = right leaf)
//!                 negative:   left  child ifN (weight 1) | negative constant
//!                             (weight = left leaf)
//! root ifN ──(weight 1/gain)──▶ anchor
//! ```
//!
//! The node construction itself is NEAT-AI-core's canonical
//! [`graft_if_nodes`]: Rebase describes the post-order batch and core places,
//! orders and validates it. Nothing here hand-writes synapse roles.
//!
//! ## Idempotence
//!
//! [`crate::patch::Patch::id`] is a digest of the correction itself, and it
//! prefixes every appended neuron. So "is this patch already on the champion?"
//! is a prefix scan — no host exclusion, no bookkeeping, and it works on a
//! champion that reached the population through a path Rebase never saw. A
//! patch already present is a clean no-op ([`Application::AlreadyPresent`]),
//! never an error.
//!
//! ## The anchor walk
//!
//! A point-wise output takes the correction in its pre-squash sum and an `IF`
//! output takes it on both branches — both are the output neuron itself. A
//! `MINIMUM`/`MAXIMUM` clamp is different: its value is one weighted source, so
//! an extra synapse competes with it rather than adding to it. But that shape
//! is *linear in the source it selects*, so the walk steps past the clamp onto
//! the source, multiplying the gain, and the root's outward edge carries
//! `1 / gain` so the patch's leaves stay in the output space its residuals were
//! measured in. The walk descends only where the choice is unambiguous, and
//! fails closed otherwise. Nothing pre-existing is ever rewritten.
//!
//! This mirrors `forests::graft`; keeping the two in step is what makes a
//! rebased candidate the same creature Forests would have published, only on a
//! newer parent.

use std::collections::{HashMap, HashSet};

use neat_core::{
    CreatureExport, IfNodeSpec, NeuronExport, RelaySpec, SquashType, SynapseType, graft_if_nodes,
    graft_relay_node, parse_squash_name,
};

use crate::adapter::Application;
use crate::compat::Incompatibility;
use crate::creature::{output_uuid, outputs_are_last, validate_creature};
use crate::patch::{Node, Patch};

/// How far past aggregate neurons the anchor walk descends.
///
/// The production chain is two clamps deep; the bound only stops a
/// pathological creature from being walked forever.
const MAX_ANCHOR_DEPTH: usize = 8;

/// Who owns the three bias-1 constants a graft's `IF` nodes read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraftConstants {
    /// One set of three shared by every patch on the creature — reused from
    /// the champion's own bias-1 constants where it has them, else created
    /// once as `forest-one-a/b/c`. The default, and what Forests emits.
    #[default]
    Shared,
    /// Three constants per patch, named for it. A patch's `IF` nodes then
    /// depend only on constants that patch introduced.
    PerPatch,
}

/// How a correction reaches both branches of an `IF` anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IfCorrection {
    /// The root feeds both branches directly, as two synapses of different
    /// roles between the same ordered pair. One neuron cheaper; needs
    /// `neat-core` 0.10.6 or newer. The default, and what Forests emits.
    #[default]
    TypedPair,
    /// The root feeds the `positive` branch and an IDENTITY relay carries the
    /// same value into the `negative` one. Kept for creatures that must load
    /// under older runtimes, which silently drop one of a typed pair.
    Relay,
}

/// How a graft is shaped, beyond the patch itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GraftOptions {
    /// Who owns the three bias-1 constants.
    pub constants: GraftConstants,
    /// How a correction reaches both branches of an `IF` anchor.
    pub if_correction: IfCorrection,
}

/// `true` when `target` already carries the structure this patch grafts.
///
/// A prefix scan on `forest-<patch id>-`, which is what the graft names every
/// neuron it appends.
pub fn is_present(patch: &Patch, target: &CreatureExport) -> bool {
    let prefix = format!("{}-", patch.uuid_prefix());
    target.neurons.iter().any(|n| n.uuid.starts_with(&prefix))
}

/// Apply `patch` to a clone of `target` with the default shape.
///
/// # Errors
///
/// [`Incompatibility::Precondition`] when the patch cannot be grafted — a
/// feature or output out of range, a non-finite value, a bare-leaf root, an
/// anchor the walk cannot resolve, or a candidate that fails validation.
pub fn apply(patch: &Patch, target: &CreatureExport) -> Result<Application, Incompatibility> {
    apply_with(patch, target, GraftOptions::default())
}

/// [`apply`], choosing the graft's shape.
///
/// # Errors
///
/// Same conditions as [`apply`].
pub fn apply_with(
    patch: &Patch,
    target: &CreatureExport,
    options: GraftOptions,
) -> Result<Application, Incompatibility> {
    if is_present(patch, target) {
        return Ok(Application::AlreadyPresent);
    }
    let grafted = graft(patch, target, options)?;
    Ok(grafted)
}

fn refuse(message: impl Into<String>) -> Incompatibility {
    Incompatibility::Precondition(message.into())
}

fn graft(
    patch: &Patch,
    target: &CreatureExport,
    options: GraftOptions,
) -> Result<Application, Incompatibility> {
    if patch.version != crate::patch::PATCH_FORMAT_VERSION {
        return Err(refuse(format!(
            "patch format version {} is not supported (this build implements {})",
            patch.version,
            crate::patch::PATCH_FORMAT_VERSION
        )));
    }
    if !patch.root.is_finite() {
        return Err(refuse(
            "patch carries a non-finite weight, threshold or leaf",
        ));
    }
    if matches!(patch.root, Node::Leaf { .. }) {
        return Err(refuse(
            "patch root is a bare leaf: it would add structure that corrects nothing",
        ));
    }
    if patch.output >= target.output {
        return Err(refuse(format!(
            "patch targets output {} but the champion has {} outputs",
            patch.output, target.output
        )));
    }
    outputs_are_last(target).map_err(|e| refuse(e.to_string()))?;

    let output_neuron = output_uuid(target, patch.output).map_err(|e| refuse(e.to_string()))?;
    let anchor = resolve_anchor(target, &output_neuron)?;
    // A correction of `c` at the anchor is worth `gain · c` at the output, so
    // the outward edge carries `1 / gain` and the patch's leaves stay in the
    // output space the residuals were measured in.
    let edge = 1.0 / anchor.gain;

    let existing: HashSet<&str> = target.neurons.iter().map(|x| x.uuid.as_str()).collect();
    let prefix = patch.uuid_prefix();
    let (ones, new_constants) = allocate_ones(target, &existing, &prefix, options.constants)?;
    let mut emitter = Emitter {
        prefix,
        ones: RoleOnes {
            condition: ones[0].clone(),
            positive: ones[1].clone(),
            negative: ones[2].clone(),
        },
        specs: Vec::new(),
        counter: 0,
        input: target.input,
        existing: &existing,
    };
    let root_uuid = emitter.emit(&patch.root)?;

    let target_is_if = anchor.squash == SquashType::If;
    let relay = if target_is_if && options.if_correction == IfCorrection::Relay {
        Some(emitter.fresh("relay")?)
    } else {
        None
    };
    {
        let root = emitter
            .specs
            .last_mut()
            .expect("a split patch describes at least one node");
        *root = match (target_is_if, relay.is_some()) {
            // Both branches from the one source, no relay in between.
            (true, false) => root
                .clone()
                .with_target_role(anchor.uuid.clone(), edge, SynapseType::Positive)
                .with_target_role(anchor.uuid.clone(), edge, SynapseType::Negative),
            (true, true) => {
                root.clone()
                    .with_target_role(anchor.uuid.clone(), edge, SynapseType::Positive)
            }
            (false, _) => root.clone().with_target(anchor.uuid.clone(), edge),
        };
    }

    // New constants go in front of the first non-constant neuron:
    // `creature_validate` rule 11 rejects a constant that follows a hidden one.
    let base = with_constants(target, new_constants);
    let mut creature = graft_if_nodes(&base, &emitter.specs)
        .map_err(|e| refuse(format!("NEAT-AI-core refused the graft: {e:?}")))?;
    if let Some(relay) = relay {
        let spec = RelaySpec::new(relay, 0.0)
            .with_source(root_uuid, 1.0)
            .with_target_role(anchor.uuid.clone(), edge, SynapseType::Negative);
        creature = graft_relay_node(&creature, &spec)
            .map_err(|e| refuse(format!("NEAT-AI-core refused the relay: {e:?}")))?;
    }

    let added_uuids: Vec<String> = creature
        .neurons
        .iter()
        .filter(|n| !existing.contains(n.uuid.as_str()))
        .map(|n| n.uuid.clone())
        .collect();

    validate_creature(&creature).map_err(|e| refuse(e.to_string()))?;
    Ok(Application::Applied {
        creature: Box::new(creature),
        added_uuids,
        removed_uuids: Vec::new(),
    })
}

/// Where a correction can enter the creature, and what it is worth at the
/// output once it does.
struct Anchor {
    uuid: String,
    /// `d(output) / d(anchor activation)` along the branch walked.
    gain: f64,
    squash: SquashType,
}

fn resolve_anchor(target: &CreatureExport, output_uuid: &str) -> Result<Anchor, Incompatibility> {
    let by_uuid: HashMap<&str, &NeuronExport> = target
        .neurons
        .iter()
        .map(|n| (n.uuid.as_str(), n))
        .collect();
    let mut uuid = output_uuid.to_string();
    let mut gain = 1.0f64;
    for depth in 0..=MAX_ANCHOR_DEPTH {
        let neuron = by_uuid
            .get(uuid.as_str())
            .ok_or_else(|| refuse(format!("`{uuid}` names no neuron")))?;
        let name = neuron.squash.as_deref().unwrap_or("IDENTITY");
        let squash = parse_squash_name(name)
            .map_err(|e| refuse(format!("squash `{name}` on `{uuid}`: {e}")))?;
        // An `IF` takes the correction on both branches, and an IDENTITY takes
        // it in a sum it does not squash — either is additive wherever it sits.
        if squash == SquashType::If || squash == SquashType::Identity {
            return Ok(Anchor { uuid, gain, squash });
        }
        if squash.is_aggregate() {
            if !matches!(squash, SquashType::Minimum | SquashType::Maximum) {
                // MEAN divides by a synapse count the graft would change;
                // HYPOT is not linear in any source. Neither can be added to
                // or walked past.
                return Err(refuse(format!(
                    "output squash `{name}` is neither additive nor linear in any one source"
                )));
            }
            let mut sources = target
                .synapses
                .iter()
                .filter(|s| s.to_uuid == uuid)
                .filter(|s| {
                    by_uuid
                        .get(s.from_uuid.as_str())
                        .is_some_and(|n| n.neuron_type != "constant")
                });
            let Some(step) = sources.next() else {
                return Err(refuse(format!(
                    "`{uuid}` (`{name}`) selects between inputs and constants only"
                )));
            };
            if sources.next().is_some() {
                return Err(refuse(format!(
                    "`{uuid}` (`{name}`) selects between two or more neurons; no one branch carries the correction"
                )));
            }
            gain *= step.weight;
            if !gain.is_finite() || gain == 0.0 {
                return Err(refuse(format!("the gain through `{uuid}` is {gain}")));
            }
            uuid = step.from_uuid.clone();
            continue;
        }
        // A point-wise squash at the output is the case Forests measures its
        // residuals in, so a correction is additive there. Behind a clamp that
        // space was never measured, so refuse.
        if depth == 0 {
            return Ok(Anchor { uuid, gain, squash });
        }
        return Err(refuse(format!(
            "`{uuid}` behind the clamp squashes with `{name}`; a correction added there is not additive at the output"
        )));
    }
    Err(refuse(format!(
        "more than {MAX_ANCHOR_DEPTH} aggregates stand between `{output_uuid}` and anything a correction can be added to"
    )))
}

/// The three bias-1 constants a graft's `IF` nodes hang off — one per role.
#[derive(Debug, Clone)]
struct RoleOnes {
    condition: String,
    positive: String,
    negative: String,
}

/// `wanted` UUIDs for bias-1 constants that no neuron in the creature carries.
///
/// A name already taken is skipped and the next free one used, rather than
/// refusing: refusing would make every later graft on that champion fail too.
fn free_one_names(
    existing: &HashSet<&str>,
    wanted: usize,
    mode: GraftConstants,
    prefix: &str,
) -> Result<Vec<String>, Incompatibility> {
    let letters = match mode {
        GraftConstants::Shared => ['a', 'b', 'c'],
        GraftConstants::PerPatch => ['c', 'p', 'n'],
    };
    let mut out: Vec<String> = Vec::with_capacity(wanted);
    for round in 0..=existing.len() + 1 {
        if out.len() == wanted {
            break;
        }
        for letter in letters {
            let base = match mode {
                GraftConstants::Shared => format!("forest-one-{letter}"),
                GraftConstants::PerPatch => format!("{prefix}-one-{letter}"),
            };
            let name = match round {
                0 => base,
                r => format!("{base}{}", r + 1),
            };
            if out.len() < wanted && !existing.contains(name.as_str()) {
                out.push(name);
            }
        }
    }
    if out.len() < wanted {
        // Unreachable for any finite creature; fail closed rather than emit a
        // creature with two neurons under one uuid.
        return Err(refuse(format!(
            "no free constant name for `{prefix}-one-*`"
        )));
    }
    Ok(out)
}

fn allocate_ones(
    target: &CreatureExport,
    existing: &HashSet<&str>,
    prefix: &str,
    mode: GraftConstants,
) -> Result<(Vec<String>, Vec<NeuronExport>), Incompatibility> {
    let mut ones: Vec<String> = match mode {
        GraftConstants::PerPatch => Vec::new(),
        GraftConstants::Shared => target
            .neurons
            .iter()
            .filter(|n| n.neuron_type == "constant" && n.bias == 1.0 && n.squash.is_none())
            .map(|n| n.uuid.clone())
            .take(3)
            .collect(),
    };
    let mut new_constants = Vec::with_capacity(3 - ones.len());
    for name in free_one_names(existing, 3 - ones.len(), mode, prefix)? {
        new_constants.push(NeuronExport {
            id: None,
            neuron_type: "constant".into(),
            uuid: name.clone(),
            bias: 1.0,
            squash: None,
        });
        ones.push(name);
    }
    Ok((ones, new_constants))
}

/// Clone `target` with `constants` listed ahead of its first non-constant
/// neuron, which is where `creature_validate` rule 11 requires them.
fn with_constants(target: &CreatureExport, constants: Vec<NeuronExport>) -> CreatureExport {
    let first_output = target.neurons.len() - target.output;
    let first_hidden = target.neurons[..first_output]
        .iter()
        .position(|n| n.neuron_type != "constant")
        .unwrap_or(first_output);
    let mut creature = target.clone();
    creature
        .neurons
        .splice(first_hidden..first_hidden, constants);
    creature
}

/// Describes a patch as a post-order list of canonical [`IfNodeSpec`]s — a
/// child is described before the parent whose branch reads it.
struct Emitter<'a> {
    prefix: String,
    ones: RoleOnes,
    specs: Vec<IfNodeSpec>,
    counter: usize,
    input: usize,
    existing: &'a HashSet<&'a str>,
}

impl Emitter<'_> {
    fn fresh(&mut self, tag: &str) -> Result<String, Incompatibility> {
        let uuid = format!("{}-{tag}{}", self.prefix, self.counter);
        self.counter += 1;
        if self.existing.contains(uuid.as_str()) {
            return Err(refuse(format!("uuid `{uuid}` is already taken")));
        }
        Ok(uuid)
    }

    /// Source feeding a parent's branch: a leaf is `(role constant, weight =
    /// correction)`, a split is `(its IF neuron, 1.0)`. `positive` selects the
    /// constant so the two leaves of one `IF` never share a source.
    fn branch_source(
        &mut self,
        node: &Node,
        positive: bool,
    ) -> Result<(String, f64), Incompatibility> {
        match node {
            Node::Leaf { correction } => {
                let one = if positive {
                    self.ones.positive.clone()
                } else {
                    self.ones.negative.clone()
                };
                Ok((one, f64::from(*correction)))
            }
            Node::Split { .. } => Ok((self.emit(node)?, 1.0)),
        }
    }

    fn emit(&mut self, node: &Node) -> Result<String, Incompatibility> {
        match node {
            Node::Leaf { .. } => Err(refuse("patch root is a bare leaf")),
            Node::Split {
                condition,
                left,
                right,
            } => {
                let (left_src, left_w) = self.branch_source(left, false)?;
                let (right_src, right_w) = self.branch_source(right, true)?;
                let uuid = self.fresh("if")?;
                let mut spec = IfNodeSpec::new(uuid.clone(), 0.0);
                let mut seen = HashSet::new();
                for t in &condition.terms {
                    if t.feature >= self.input {
                        return Err(refuse(format!(
                            "patch reads feature {} but the champion has {} inputs",
                            t.feature, self.input
                        )));
                    }
                    if !seen.insert(t.feature) {
                        return Err(refuse(format!(
                            "condition names feature {} twice; one term with the summed weight says the same thing",
                            t.feature
                        )));
                    }
                    spec = spec.with_condition(format!("input-{}", t.feature), f64::from(t.weight));
                }
                // The split point rides as a weight on the condition constant:
                // Σ w·x − threshold > 0 ⇔ right.
                spec = spec
                    .with_condition(self.ones.condition.clone(), f64::from(-condition.threshold))
                    .with_positive(right_src, right_w)
                    .with_negative(left_src, left_w);
                self.specs.push(spec);
                Ok(uuid)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::creature::{creature_checksum, validate_source_creature};
    use crate::fixtures::{clamped_output_creature, evolved_descendant, linear_hidden_creature};
    use crate::patch::{Condition, Provenance, Term};
    use neat_core::{compile_creature, creature_to_json};

    fn stump_patch() -> Patch {
        Patch::new(0, Node::stump(1, 0.5, 0.0, 0.25), Provenance::default())
    }

    fn applied(patch: &Patch, target: &CreatureExport) -> CreatureExport {
        match apply(patch, target).unwrap() {
            Application::Applied { creature, .. } => *creature,
            Application::AlreadyPresent => panic!("expected the patch to apply"),
        }
    }

    #[test]
    fn patch_applies_to_its_original_parent() {
        let parent = linear_hidden_creature(2.0);
        let out = applied(&stump_patch(), &parent);
        validate_source_creature(&out).unwrap();
        assert!(out.neurons.len() > parent.neurons.len());
        assert!(is_present(&stump_patch(), &out));
    }

    #[test]
    fn the_same_patch_applies_to_a_structurally_evolved_descendant() {
        // The fleet added `h2` while the patch was being discovered on the
        // parent. The patch still lands, and `h2` survives untouched — this is
        // the whole reason Rebase exists.
        let descendant = evolved_descendant(2.0, 0.5);
        let out = applied(&stump_patch(), &descendant);
        validate_source_creature(&out).unwrap();
        assert!(out.neurons.iter().any(|n| n.uuid == "h2"));
        assert!(
            out.synapses
                .iter()
                .any(|s| s.from_uuid == "h2" && s.to_uuid == "output-0" && s.weight == 1.0),
            "the unrelated fleet improvement must survive the rebase"
        );
    }

    #[test]
    fn an_already_present_patch_is_a_clean_no_op() {
        let parent = linear_hidden_creature(2.0);
        let once = applied(&stump_patch(), &parent);
        let neurons = once.neurons.len();
        assert_eq!(
            apply(&stump_patch(), &once).unwrap(),
            Application::AlreadyPresent
        );
        // Nothing was created on the second pass.
        assert_eq!(once.neurons.len(), neurons);
    }

    #[test]
    fn presence_survives_the_creature_being_evolved_further() {
        // A champion that carries the graft *and* has since grown is still
        // recognised: presence is a prefix scan, not a checksum comparison.
        let mut grafted = applied(&stump_patch(), &linear_hidden_creature(2.0));
        let out_index = grafted.neurons.len() - 1;
        grafted.neurons[out_index].bias += 0.01;
        assert!(is_present(&stump_patch(), &grafted));
    }

    #[test]
    fn provenance_tags_ride_on_the_new_structure() {
        let out = applied(&stump_patch(), &linear_hidden_creature(2.0));
        let prefix = stump_patch().uuid_prefix();
        assert!(
            out.neurons.iter().any(|n| n.uuid.starts_with(&prefix)),
            "grafted structure must carry the patch id that identifies it"
        );
    }

    #[test]
    fn a_feature_out_of_range_fails_closed_with_an_actionable_reason() {
        let parent = linear_hidden_creature(2.0); // 2 inputs
        let patch = Patch::new(0, Node::stump(9, 0.5, 0.0, 0.25), Provenance::default());
        let err = apply(&patch, &parent).unwrap_err();
        assert!(err.to_string().contains("feature 9"), "{err}");
        assert!(err.to_string().contains("2 inputs"), "{err}");
    }

    #[test]
    fn an_output_out_of_range_fails_closed() {
        let parent = linear_hidden_creature(2.0); // 1 output
        let patch = Patch::new(3, Node::stump(0, 0.5, 0.0, 0.25), Provenance::default());
        let err = apply(&patch, &parent).unwrap_err();
        assert!(err.to_string().contains("output 3"), "{err}");
    }

    #[test]
    fn a_bare_leaf_root_and_a_non_finite_leaf_fail_closed() {
        let parent = linear_hidden_creature(2.0);
        let leaf = Patch::new(0, Node::leaf(0.5), Provenance::default());
        assert!(
            apply(&leaf, &parent)
                .unwrap_err()
                .to_string()
                .contains("leaf")
        );

        let nan = Patch::new(0, Node::stump(0, 0.5, 0.0, f32::NAN), Provenance::default());
        assert!(
            apply(&nan, &parent)
                .unwrap_err()
                .to_string()
                .contains("non-finite")
        );
    }

    #[test]
    fn the_source_champion_is_never_modified() {
        let parent = linear_hidden_creature(2.0);
        let before = creature_to_json(&parent).unwrap();
        let _ = applied(&stump_patch(), &parent);
        assert_eq!(creature_to_json(&parent).unwrap(), before);
    }

    #[test]
    fn a_depth_two_tree_grafts_as_nested_if_nodes() {
        let root = Node::Split {
            condition: Condition::axis(0, 0.5),
            left: Box::new(Node::stump(1, 0.25, -0.1, 0.1)),
            right: Box::new(Node::stump(1, 0.75, 0.2, 0.3)),
        };
        let patch = Patch::new(0, root, Provenance::default());
        let out = applied(&patch, &linear_hidden_creature(2.0));
        validate_source_creature(&out).unwrap();
        let prefix = format!("{}-if", patch.uuid_prefix());
        assert_eq!(
            out.neurons
                .iter()
                .filter(|n| n.uuid.starts_with(&prefix))
                .count(),
            3,
            "a depth-2 tree is three IF nodes"
        );
    }

    #[test]
    fn an_oblique_condition_grafts_one_synapse_per_term() {
        let root = Node::Split {
            condition: Condition {
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
            },
            left: Box::new(Node::leaf(0.0)),
            right: Box::new(Node::leaf(0.4)),
        };
        let patch = Patch::new(0, root, Provenance::default());
        let out = applied(&patch, &linear_hidden_creature(2.0));
        validate_source_creature(&out).unwrap();
    }

    #[test]
    fn a_repeated_feature_in_one_condition_fails_closed() {
        let root = Node::Split {
            condition: Condition {
                terms: vec![
                    Term {
                        feature: 0,
                        weight: 0.5,
                    },
                    Term {
                        feature: 0,
                        weight: 0.25,
                    },
                ],
                threshold: 0.0,
            },
            left: Box::new(Node::leaf(0.0)),
            right: Box::new(Node::leaf(0.4)),
        };
        let patch = Patch::new(0, root, Provenance::default());
        let err = apply(&patch, &linear_hidden_creature(2.0)).unwrap_err();
        assert!(err.to_string().contains("twice"), "{err}");
    }

    #[test]
    fn the_anchor_walk_steps_past_a_minimum_clamp() {
        let clamped = clamped_output_creature();
        let out = applied(&stump_patch(), &clamped);
        validate_source_creature(&out).unwrap();
        // The correction lands on the body behind the clamp, carrying 1/gain
        // so a leaf of `c` still moves the output by `c`.
        let root_edge = out
            .synapses
            .iter()
            .find(|s| s.to_uuid == "body" && s.from_uuid.starts_with("forest-"))
            .expect("the root attaches to the clamped body");
        assert!((root_edge.weight - 0.5).abs() < 1e-12, "{root_edge:?}");
    }

    #[test]
    fn two_patches_stack_and_produce_a_distinct_creature() {
        let parent = linear_hidden_creature(2.0);
        let a = stump_patch();
        let b = Patch::new(0, Node::stump(0, 0.1, 0.0, -0.05), Provenance::default());
        let once = applied(&a, &parent);
        let twice = applied(&b, &once);
        validate_source_creature(&twice).unwrap();
        assert!(is_present(&a, &twice) && is_present(&b, &twice));
        assert_ne!(
            creature_checksum(&once).unwrap(),
            creature_checksum(&twice).unwrap()
        );
        // The second graft reuses the shared bias-1 constants the first made.
        assert_eq!(
            twice
                .neurons
                .iter()
                .filter(|n| n.uuid.starts_with("forest-one-"))
                .count(),
            3
        );
    }

    #[test]
    fn the_relay_shape_is_available_for_older_runtimes() {
        // An IF anchor: graft once with the typed pair, then re-graft a second
        // patch onto the result using the relay shape, which lands on the IF
        // node chain the first graft created.
        let parent = linear_hidden_creature(2.0);
        let out = applied(&stump_patch(), &parent);
        let second = Patch::new(0, Node::stump(0, 0.9, 0.0, 0.02), Provenance::default());
        let relayed = apply_with(
            &second,
            &out,
            GraftOptions {
                constants: GraftConstants::PerPatch,
                if_correction: IfCorrection::Relay,
            },
        )
        .unwrap();
        validate_source_creature(relayed.creature().unwrap()).unwrap();
    }

    #[test]
    fn the_result_compiles_under_neat_ai_core() {
        let out = applied(&stump_patch(), &evolved_descendant(2.0, 0.5));
        compile_creature(&out).unwrap();
    }
}
