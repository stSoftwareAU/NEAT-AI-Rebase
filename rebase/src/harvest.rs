//! Recover an enhancement bundle from a creature that already carries the work
//! (NEAT-AI-Rebase #7, producer side not yet wired).
//!
//! The intended flow is that a producer files its accepted changes as it makes
//! them ([`crate::enhancement`]). Forests does not do that yet — and the fleet
//! is already running, publishing creatures every few minutes, losing exactly
//! the discoveries this project exists to keep.
//!
//! It does not have to be lost, because a Forest graft is **self-identifying**:
//! every neuron it appends is named `forest-<patch id>-…`, and the patch id is
//! a digest of the correction itself. So a creature that carries a graft
//! carries enough to rebuild the patch that made it.
//!
//! ## The check that makes this safe
//!
//! A reconstruction is accepted **only if the rebuilt patch hashes back to the
//! id it was found under**. That is not a sanity check, it is the whole
//! argument: the id is a digest of `(output, root)`, so a patch that hashes
//! correctly is bit-for-bit the tree Forests searched, and one that does not is
//! discarded rather than guessed at. A wrong reconstruction would graft under a
//! different name and silently break idempotence for that patch forever, so
//! there is no "close enough" path here.
//!
//! ## What this is not
//!
//! A substitute for the producer emitting bundles. Harvesting can only see
//! patches that survived into the published creature — a patch Forests accepted
//! and later dropped, or one whose structure a pruner has since rewritten, is
//! gone. It also cannot recover the producer's own scores, so the meta carries
//! the harvest's own provenance rather than an invented measurement.
//!
//! Use it to rebase what the fleet is publishing today; keep #7 open.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use neat_core::CreatureExport;

use crate::creature::creature_checksum;
use crate::enhancement::{Enhancement, Payload, ProducerContext};
use crate::forest::graft_anchor;
use crate::patch::{Condition, Node, Patch, Provenance, Term};

/// Prefix every Forest graft gives the neurons it appends.
const FOREST_PREFIX: &str = "forest-";
/// Length of a patch id in hex characters.
const ID_LEN: usize = 16;

/// A patch found in a creature but not recoverable from it.
#[derive(Debug, Clone, PartialEq)]
pub struct HarvestSkip {
    /// The patch id the structure was named under.
    pub id: String,
    /// Why it could not be recovered.
    pub reason: String,
}

/// What a harvest recovered.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Harvested {
    /// Patches recovered and verified against their own id.
    pub patches: Vec<Patch>,
    /// Patches that were present but could not be recovered, with reasons.
    pub skipped: Vec<HarvestSkip>,
}

impl Harvested {
    /// `true` when nothing was recovered.
    pub fn is_empty(&self) -> bool {
        self.patches.is_empty()
    }

    /// Wrap the recovered patches as an enhancement bundle against `source`.
    ///
    /// The two scores are the harvest's own honest position: it did not measure
    /// anything, so `base_score` and `improved_score` are both `source_score`
    /// and the claimed gain is zero. Rebase never promotes on those numbers
    /// anyway — only the verdict does — but recording an invented gain would
    /// put a number in the journal that nothing stands behind.
    ///
    /// # Errors
    ///
    /// The serialisation failure when `source` cannot be checksummed.
    pub fn into_enhancements(
        self,
        source: &CreatureExport,
        corpus_identity: &str,
        producer: &str,
        source_score: f64,
    ) -> Result<Vec<Enhancement>, String> {
        let context = ProducerContext {
            producer: producer.to_string(),
            base_checksum: creature_checksum(source)?,
            base_score: source_score,
            improved_score: source_score,
            corpus_identity: corpus_identity.to_string(),
            input_count: source.input,
            output_count: source.output,
        };
        Ok(self
            .patches
            .into_iter()
            .map(|patch| Enhancement::new(Payload::ForestPatch { patch }, &context))
            .collect())
    }
}

/// Patch ids the creature carries, ascending.
pub fn patch_ids(creature: &CreatureExport) -> BTreeSet<String> {
    creature
        .neurons
        .iter()
        .filter_map(|n| patch_id_of(&n.uuid))
        .collect()
}

/// The patch id a grafted neuron belongs to, if it is one.
fn patch_id_of(uuid: &str) -> Option<String> {
    let rest = uuid.strip_prefix(FOREST_PREFIX)?;
    let (id, tail) = rest.split_at_checked(ID_LEN)?;
    if !tail.starts_with('-') || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(id.to_string())
}

/// Recover every patch that `source` carries and `target` does not.
///
/// This is the Δ of the race: the discoveries that would be destroyed by
/// publishing `target`, or lost by publishing `source` over a `target` that has
/// moved on. Order is by patch id, which is stable and independent of how
/// either creature happens to list its neurons.
pub fn harvest_delta(source: &CreatureExport, target: &CreatureExport) -> Harvested {
    let present = patch_ids(target);
    harvest_ids(
        source,
        patch_ids(source)
            .into_iter()
            .filter(|id| !present.contains(id)),
    )
}

/// Recover every patch `source` carries.
pub fn harvest_all(source: &CreatureExport) -> Harvested {
    harvest_ids(source, patch_ids(source).into_iter())
}

fn harvest_ids(source: &CreatureExport, ids: impl Iterator<Item = String>) -> Harvested {
    let index = Index::of(source);
    let mut out = Harvested::default();
    for id in ids {
        match reconstruct(source, &index, &id) {
            Ok(patch) => out.patches.push(patch),
            Err(reason) => out.skipped.push(HarvestSkip { id, reason }),
        }
    }
    out
}

/// Inbound synapses per neuron, and the anchor of each output.
struct Index<'a> {
    inbound: HashMap<&'a str, Vec<&'a neat_core::SynapseExport>>,
    outbound: HashMap<&'a str, Vec<&'a neat_core::SynapseExport>>,
    constants: BTreeSet<&'a str>,
    /// anchor uuid -> output index, for every output whose anchor resolves.
    anchors: BTreeMap<String, usize>,
}

impl<'a> Index<'a> {
    fn of(creature: &'a CreatureExport) -> Self {
        let mut inbound: HashMap<&str, Vec<&neat_core::SynapseExport>> = HashMap::new();
        let mut outbound: HashMap<&str, Vec<&neat_core::SynapseExport>> = HashMap::new();
        for s in &creature.synapses {
            inbound.entry(s.to_uuid.as_str()).or_default().push(s);
            outbound.entry(s.from_uuid.as_str()).or_default().push(s);
        }
        let constants = creature
            .neurons
            .iter()
            .filter(|n| n.neuron_type == "constant")
            .map(|n| n.uuid.as_str())
            .collect();
        let mut anchors = BTreeMap::new();
        for output in 0..creature.output {
            if let Ok((uuid, _gain)) = graft_anchor(creature, output) {
                anchors.insert(uuid, output);
            }
        }
        Self {
            inbound,
            outbound,
            constants,
            anchors,
        }
    }
}

/// Rebuild patch `id` from the structure `source` carries under that name.
fn reconstruct(source: &CreatureExport, index: &Index<'_>, id: &str) -> Result<Patch, String> {
    let prefix = format!("{FOREST_PREFIX}{id}-");
    let node_prefix = format!("{prefix}if");
    let mut nodes: Vec<&str> = source
        .neurons
        .iter()
        .map(|n| n.uuid.as_str())
        .filter(|u| u.starts_with(node_prefix.as_str()))
        .collect();
    if nodes.is_empty() {
        return Err(format!("no `{node_prefix}N` neurons"));
    }
    // Post-order emission names them if0, if1, … so the root is the highest —
    // but the root is defined by the graph, not the name, so derive it.
    nodes.sort_unstable();
    let members: BTreeSet<&str> = nodes.iter().copied().collect();

    let mut branches: HashMap<&str, Branch> = HashMap::new();
    for &uuid in &nodes {
        branches.insert(uuid, read_branch(index, uuid, &members)?);
    }
    let root = nodes
        .iter()
        .copied()
        .find(|uuid| {
            !branches
                .values()
                .any(|b| b.positive.child() == Some(uuid) || b.negative.child() == Some(uuid))
        })
        .ok_or("every node feeds another; the patch has no root")?;

    let tree = build(root, &branches, 0)?;
    let output = resolve_output(index, root, &prefix)?;

    let patch = Patch::new(
        output,
        tree,
        Provenance {
            strategy: "harvested".into(),
            backend: "creature".into(),
            incumbent_checksum: creature_checksum(source).unwrap_or_default(),
            notes: vec![format!("recovered from grafted structure `{prefix}…`")],
            ..Provenance::default()
        },
    );
    // The whole argument for trusting this: the rebuilt tree hashes back to the
    // name it was found under, so it is the tree Forests searched.
    if patch.id() != id {
        return Err(format!(
            "reconstruction hashes to `{}`, not `{id}`; discarding rather than \
             grafting under a name that would break idempotence",
            patch.id()
        ));
    }
    Ok(patch)
}

/// One `IF` node's three roles, as read back off the creature.
struct Branch {
    terms: Vec<Term>,
    threshold: f32,
    positive: Side,
    negative: Side,
}

/// What a branch reads: a constant leaf, or a child node.
enum Side {
    Leaf(f32),
    Child(String),
    Missing,
}

impl Side {
    fn child(&self) -> Option<&str> {
        match self {
            Self::Child(uuid) => Some(uuid.as_str()),
            _ => None,
        }
    }
}

fn read_branch(index: &Index<'_>, uuid: &str, members: &BTreeSet<&str>) -> Result<Branch, String> {
    let inbound = index
        .inbound
        .get(uuid)
        .ok_or_else(|| format!("`{uuid}` has no inbound synapses"))?;
    let mut terms = Vec::new();
    let mut threshold = None;
    let mut positive = Side::Missing;
    let mut negative = Side::Missing;
    for s in inbound {
        let role = s.synapse_type.as_deref().unwrap_or("");
        let from = s.from_uuid.as_str();
        match role {
            "condition" => {
                if let Some(feature) = from.strip_prefix("input-") {
                    let feature: usize = feature
                        .parse()
                        .map_err(|_| format!("condition source `{from}` is not an input index"))?;
                    terms.push(Term {
                        feature,
                        weight: s.weight as f32,
                    });
                } else if index.constants.contains(from) {
                    if threshold.replace(-s.weight as f32).is_some() {
                        return Err(format!("`{uuid}` has two condition constants"));
                    }
                } else {
                    return Err(format!(
                        "`{uuid}` reads condition from `{from}`, which is neither an input nor a constant"
                    ));
                }
            }
            "positive" | "negative" => {
                let side = if members.contains(from) {
                    Side::Child(from.to_string())
                } else if index.constants.contains(from) {
                    Side::Leaf(s.weight as f32)
                } else {
                    return Err(format!(
                        "`{uuid}` reads {role} from `{from}`, which is neither a leaf constant nor a node of this patch"
                    ));
                };
                let slot = if role == "positive" {
                    &mut positive
                } else {
                    &mut negative
                };
                if !matches!(slot, Side::Missing) {
                    return Err(format!("`{uuid}` has two {role} sources"));
                }
                *slot = side;
            }
            other => {
                return Err(format!("`{uuid}` has an inbound `{other}` synapse"));
            }
        }
    }
    if terms.is_empty() {
        return Err(format!("`{uuid}` has no condition terms"));
    }
    let threshold = threshold.ok_or_else(|| format!("`{uuid}` has no condition constant"))?;
    Ok(Branch {
        terms,
        threshold,
        positive,
        negative,
    })
}

/// Depth bound; a Forest tree is depth 3 in production and the bound only stops
/// a malformed cycle from recursing forever.
const MAX_DEPTH: usize = 16;

fn build(uuid: &str, branches: &HashMap<&str, Branch>, depth: usize) -> Result<Node, String> {
    if depth > MAX_DEPTH {
        return Err(format!("patch nests deeper than {MAX_DEPTH}; refusing"));
    }
    let branch = branches
        .get(uuid)
        .ok_or_else(|| format!("`{uuid}` is not a node of this patch"))?;
    let side = |s: &Side| -> Result<Node, String> {
        match s {
            Side::Leaf(correction) => Ok(Node::leaf(*correction)),
            Side::Child(child) => build(child, branches, depth + 1),
            Side::Missing => Err(format!("`{uuid}` is missing a branch")),
        }
    };
    Ok(Node::Split {
        condition: Condition {
            terms: branch.terms.clone(),
            threshold: branch.threshold,
        },
        left: Box::new(side(&branch.negative)?),
        right: Box::new(side(&branch.positive)?),
    })
}

/// Which output index the root's correction reaches.
fn resolve_output(index: &Index<'_>, root: &str, prefix: &str) -> Result<usize, String> {
    let outward = index
        .outbound
        .get(root)
        .ok_or_else(|| format!("root `{root}` feeds nothing"))?;
    let mut targets: BTreeSet<&str> = BTreeSet::new();
    for s in outward {
        let to = s.to_uuid.as_str();
        // A relay carries the negative half on older runtimes; follow it.
        if to.starts_with(prefix) {
            for r in index.outbound.get(to).into_iter().flatten() {
                targets.insert(r.to_uuid.as_str());
            }
        } else {
            targets.insert(to);
        }
    }
    let mut outputs: BTreeSet<usize> = BTreeSet::new();
    for target in &targets {
        match index.anchors.get(*target) {
            Some(output) => {
                outputs.insert(*output);
            }
            None => {
                return Err(format!(
                    "root attaches to `{target}`, which is not the graft anchor of any output"
                ));
            }
        }
    }
    match outputs.len() {
        1 => Ok(*outputs.iter().next().expect("one output")),
        0 => Err("root attaches to nothing".into()),
        _ => Err(format!(
            "root attaches to {} different outputs",
            outputs.len()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::Application;
    use crate::fixtures::{clamped_output_creature, evolved_descendant, linear_hidden_creature};
    use crate::forest;

    fn graft(creature: &CreatureExport, patch: &Patch) -> CreatureExport {
        match forest::apply(patch, creature).unwrap() {
            Application::Applied { creature, .. } => *creature,
            Application::AlreadyPresent => panic!("expected the patch to apply"),
        }
    }

    fn stump(feature: usize, threshold: f32, left: f32, right: f32) -> Patch {
        Patch::new(
            0,
            Node::stump(feature, threshold, left, right),
            Provenance::default(),
        )
    }

    #[test]
    fn a_grafted_stump_round_trips_back_to_the_patch_that_made_it() {
        let base = linear_hidden_creature(2.0);
        let patch = stump(1, 0.5, 0.0, 0.25);
        let grafted = graft(&base, &patch);

        let harvest = harvest_all(&grafted);
        assert!(harvest.skipped.is_empty(), "{:?}", harvest.skipped);
        assert_eq!(harvest.patches.len(), 1);
        let recovered = &harvest.patches[0];
        assert_eq!(recovered.id(), patch.id());
        assert_eq!(recovered.output, patch.output);
        assert_eq!(recovered.root, patch.root);
    }

    #[test]
    fn a_nested_tree_round_trips() {
        let base = evolved_descendant(2.0, 0.5);
        let root = Node::Split {
            condition: Condition::axis(0, 0.5),
            left: Box::new(Node::stump(1, 0.25, -0.1, 0.1)),
            right: Box::new(Node::stump(1, 0.75, 0.2, 0.3)),
        };
        let patch = Patch::new(0, root, Provenance::default());
        let grafted = graft(&base, &patch);

        let harvest = harvest_all(&grafted);
        assert!(harvest.skipped.is_empty(), "{:?}", harvest.skipped);
        assert_eq!(harvest.patches[0].root, patch.root);
        assert_eq!(harvest.patches[0].id(), patch.id());
    }

    #[test]
    fn a_graft_behind_a_clamp_recovers_the_right_output_index() {
        // The anchor is the body behind a MINIMUM clamp, not the output, so the
        // output index has to come from the anchor walk rather than the edge.
        let base = clamped_output_creature();
        let patch = stump(1, 0.5, 0.0, 0.25);
        let grafted = graft(&base, &patch);

        let harvest = harvest_all(&grafted);
        assert!(harvest.skipped.is_empty(), "{:?}", harvest.skipped);
        assert_eq!(harvest.patches[0].output, 0);
        assert_eq!(harvest.patches[0].id(), patch.id());
    }

    #[test]
    fn the_delta_is_what_the_source_has_and_the_target_lacks() {
        let base = linear_hidden_creature(2.0);
        let shared = stump(0, 0.1, 0.0, 0.05);
        let only_in_source = stump(1, 0.5, 0.0, 0.25);

        let target = graft(&base, &shared);
        let source = graft(&graft(&base, &shared), &only_in_source);

        let harvest = harvest_delta(&source, &target);
        assert_eq!(harvest.patches.len(), 1);
        assert_eq!(harvest.patches[0].id(), only_in_source.id());
        assert!(harvest.skipped.is_empty());

        // And nothing to harvest the other way round.
        assert!(harvest_delta(&target, &source).is_empty());
    }

    #[test]
    fn a_tampered_leaf_is_discarded_rather_than_grafted_under_the_wrong_name() {
        let base = linear_hidden_creature(2.0);
        let patch = stump(1, 0.5, 0.0, 0.25);
        let mut grafted = graft(&base, &patch);
        // Evolution retrained a leaf weight. The structure still carries the
        // original patch id, but it is no longer that patch.
        let leaf = grafted
            .synapses
            .iter_mut()
            .find(|s| {
                s.synapse_type.as_deref() == Some("positive") && s.to_uuid.starts_with("forest-")
            })
            .expect("a positive leaf");
        leaf.weight += 0.001;

        let harvest = harvest_all(&grafted);
        assert!(harvest.patches.is_empty());
        assert_eq!(harvest.skipped.len(), 1);
        assert!(
            harvest.skipped[0].reason.contains("hashes to"),
            "{:?}",
            harvest.skipped
        );
    }

    #[test]
    fn a_harvested_patch_grafts_onto_a_creature_that_never_had_it() {
        let base = linear_hidden_creature(2.0);
        let patch = stump(1, 0.5, 0.0, 0.25);
        let grafted = graft(&base, &patch);

        let recovered = harvest_all(&grafted).patches.remove(0);
        // Onto an independently evolved descendant that never saw the patch.
        let fresh = evolved_descendant(2.0, 0.5);
        let rebased = graft(&fresh, &recovered);
        crate::creature::validate_source_creature(&rebased).unwrap();
        assert!(forest::is_present(&patch, &rebased), "same id, same names");
        assert!(rebased.neurons.iter().any(|n| n.uuid == "h2"));
    }

    #[test]
    fn harvested_patches_become_a_bundle_with_an_honest_zero_claim() {
        let base = linear_hidden_creature(2.0);
        let grafted = graft(&base, &stump(1, 0.5, 0.0, 0.25));
        let enhancements = harvest_all(&grafted)
            .into_enhancements(&grafted, "corpus-x", "harvest/test", 0.5)
            .unwrap();
        assert_eq!(enhancements.len(), 1);
        let meta = &enhancements[0].meta;
        assert_eq!(meta.claimed_gain(), 0.0, "a harvest measured nothing");
        assert_eq!(meta.corpus_identity, "corpus-x");
        assert_eq!(meta.input_count, grafted.input);
        assert!(enhancements[0].id_is_consistent());
    }

    #[test]
    fn patch_ids_ignores_names_that_only_look_like_grafts() {
        let mut creature = linear_hidden_creature(2.0);
        creature.neurons[0].uuid = "forest-not-a-hex-id-if0".into();
        assert!(patch_ids(&creature).is_empty());
        assert_eq!(
            patch_id_of("forest-0123456789abcdef-if0").as_deref(),
            Some("0123456789abcdef")
        );
        assert_eq!(patch_id_of("forest-0123456789abcde-if0"), None);
        assert_eq!(patch_id_of("h1"), None);
    }
}
