//! The NEAT-AI-Ockham removal adapter (Issue #3).
//!
//! Ockham proves that a piece of structure no longer earns its keep. What it
//! proved is not "this creature is better" but "removing neuron `u` this way
//! is better", and that is what replays here — onto whatever champion the fleet
//! has reached.
//!
//! ## Idempotence
//!
//! A removal is identified by the neuron UUID, and the UUID is the whole story:
//! if it is already gone from the champion, the enhancement is **already
//! incorporated**, whether it got there through Rebase, through Ockham's own
//! re-entry path, or through the fleet independently pruning it. That is a
//! clean [`Application::AlreadyPresent`], never an error and never a retry.
//!
//! ## The two strategies
//!
//! [`RemovalStrategy::MeanAblation`] replaces the neuron's downstream
//! contribution with its measured mean post-activation — `bias_j += mean_i ·
//! w_ij` for each outgoing synapse — and then cascade-cleans whatever that left
//! dead. Deliberately approximate; the mean is the producer's measurement on
//! *its* opening creature.
//!
//! [`RemovalStrategy::IdentityCollapse`] is exact: a hidden IDENTITY neuron `y`
//! between `x` and `z` folds as `bias_z += bias_y · b` and `x → z` with weight
//! `a · b`, merging into a parallel synapse where one exists.
//!
//! The strategy is part of the enhancement because the two produce different
//! creatures. Rebase reproduces the one that was accepted, or refuses; it never
//! substitutes the other.
//!
//! ## What is deliberately not reproduced
//!
//! Ockham refuses to *emit* an IDENTITY collapse that raises growth units,
//! because a pruner proposing to grow is proposing the wrong thing. Replay does
//! not apply that heuristic: the transformation was already judged worth
//! scoring, and on a new champion the authoritative scorer — which prices
//! complexity itself — is the one entitled to that opinion. Rebase constructs
//! the candidate and lets the verdict decide.
//!
//! Preconditions that are about *safety* rather than taste are all kept: typed
//! synapses, aggregate neighbours, unknown squashes and self-loops fail closed.

use std::fmt;

use neat_core::{
    CreatureExport, NeuronExport, SquashType, SynapseExport, apply_squash, parse_squash_name,
};

use crate::adapter::Application;
use crate::compat::Incompatibility;
use crate::creature::{sort_synapses_canonically, validate_creature};
use crate::enhancement::{OckhamRemoval, RemovalStrategy};

/// Why a replay could not reproduce the transformation safely.
#[derive(Debug, Clone, PartialEq)]
enum Refusal {
    NotHidden {
        uuid: String,
        neuron_type: String,
    },
    NonFiniteMean(f64),
    AggregateNeuron {
        uuid: String,
        squash: String,
    },
    NotIdentity {
        uuid: String,
        squash: String,
    },
    TypedSynapse {
        from: String,
        to: String,
        role: String,
    },
    AggregateTarget {
        uuid: String,
        squash: String,
    },
    UnknownSquash {
        uuid: String,
        squash: String,
    },
    SelfLoop {
        uuid: String,
    },
    UnknownNeuron(String),
    Invalid(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotHidden { uuid, neuron_type } => {
                write!(
                    f,
                    "`{uuid}` is {neuron_type}, not hidden; only hidden neurons are removal targets"
                )
            }
            Self::NonFiniteMean(m) => write!(f, "recorded mean {m} is not finite"),
            Self::AggregateNeuron { uuid, squash } => {
                write!(
                    f,
                    "`{uuid}` squash `{squash}` is aggregate; its removal is not a bias fold"
                )
            }
            Self::NotIdentity { uuid, squash } => write!(
                f,
                "`{uuid}` squash `{squash}` is not IDENTITY, so the recorded identityCollapse cannot be reproduced"
            ),
            Self::TypedSynapse { from, to, role } => {
                write!(
                    f,
                    "typed synapse `{from}`->`{to}` ({role}) is incident to the transform"
                )
            }
            Self::AggregateTarget { uuid, squash } => {
                write!(
                    f,
                    "downstream `{uuid}` (`{squash}`) is aggregate; a bias fold is not a sum there"
                )
            }
            Self::UnknownSquash { uuid, squash } => {
                write!(f, "`{uuid}` squash `{squash}` is unknown")
            }
            Self::SelfLoop { uuid } => {
                write!(f, "the collapse would connect `{uuid}` to itself")
            }
            Self::UnknownNeuron(u) => write!(f, "no neuron `{u}`"),
            Self::Invalid(m) => write!(f, "candidate failed validation: {m}"),
        }
    }
}

impl From<Refusal> for Incompatibility {
    fn from(r: Refusal) -> Self {
        Self::Precondition(r.to_string())
    }
}

/// `true` when `target` already has this removal — that is, when the neuron is
/// already absent.
pub fn is_present(removal: &OckhamRemoval, target: &CreatureExport) -> bool {
    !target.neurons.iter().any(|n| n.uuid == removal.neuron_uuid)
}

/// Replay `removal` onto a clone of `target`.
///
/// # Errors
///
/// [`Incompatibility::Precondition`] when the recorded transformation cannot be
/// reproduced safely on this champion. Nothing partial escapes.
pub fn apply(
    removal: &OckhamRemoval,
    target: &CreatureExport,
) -> Result<Application, Incompatibility> {
    if is_present(removal, target) {
        return Ok(Application::AlreadyPresent);
    }
    let (creature, removed_uuids) = match removal.strategy {
        RemovalStrategy::MeanAblation { mean } => ablate_mean(target, &removal.neuron_uuid, mean)?,
        RemovalStrategy::IdentityCollapse => collapse_identity(target, &removal.neuron_uuid)?,
    };
    Ok(Application::Applied {
        creature: Box::new(creature),
        added_uuids: Vec::new(),
        removed_uuids,
    })
}

/// Mean-activation ablation: `bias_j += mean · w_ij`, then cascade cleanup.
fn ablate_mean(
    target: &CreatureExport,
    uuid: &str,
    mean: f64,
) -> Result<(CreatureExport, Vec<String>), Refusal> {
    if !mean.is_finite() {
        return Err(Refusal::NonFiniteMean(mean));
    }
    let requested =
        neuron_by_uuid(target, uuid).ok_or_else(|| Refusal::UnknownNeuron(uuid.into()))?;
    if requested.neuron_type != "hidden" {
        return Err(Refusal::NotHidden {
            uuid: uuid.into(),
            neuron_type: requested.neuron_type.clone(),
        });
    }
    if squash_of(requested)?.is_aggregate() {
        return Err(Refusal::AggregateNeuron {
            uuid: uuid.into(),
            squash: squash_name(requested),
        });
    }

    let mut working = target.clone();
    working.memetic = None;

    let outgoing = synapses_from(&working, uuid);
    for syn in &outgoing {
        require_ordinary(syn)?;
        let downstream = neuron_by_uuid(&working, &syn.to_uuid)
            .ok_or_else(|| Refusal::UnknownNeuron(syn.to_uuid.clone()))?;
        reject_aggregate(downstream)?;
    }
    for syn in &synapses_to(&working, uuid) {
        require_ordinary(syn)?;
    }

    for syn in outgoing {
        fold_bias(&mut working, &syn, mean)?;
    }

    let mut removed = vec![uuid.to_string()];
    remove_neuron(&mut working, uuid);
    cleanup_cascade(&mut working, &mut removed)?;
    sort_synapses_canonically(&mut working);
    validate_creature(&working).map_err(|e| Refusal::Invalid(e.to_string()))?;
    Ok((working, removed))
}

/// Exact IDENTITY collapse: fold the bias downstream and bypass `x → y → z`.
fn collapse_identity(
    target: &CreatureExport,
    uuid: &str,
) -> Result<(CreatureExport, Vec<String>), Refusal> {
    let neuron = neuron_by_uuid(target, uuid).ok_or_else(|| Refusal::UnknownNeuron(uuid.into()))?;
    if neuron.neuron_type != "hidden" {
        return Err(Refusal::NotHidden {
            uuid: uuid.into(),
            neuron_type: neuron.neuron_type.clone(),
        });
    }
    if squash_of(neuron)? != SquashType::Identity {
        return Err(Refusal::NotIdentity {
            uuid: uuid.into(),
            squash: squash_name(neuron),
        });
    }
    let bias_y = neuron.bias;

    let incoming = synapses_to(target, uuid);
    let outgoing = synapses_from(target, uuid);
    for syn in incoming.iter().chain(&outgoing) {
        require_ordinary(syn)?;
    }
    for syn in &outgoing {
        let downstream = neuron_by_uuid(target, &syn.to_uuid)
            .ok_or_else(|| Refusal::UnknownNeuron(syn.to_uuid.clone()))?;
        reject_aggregate(downstream)?;
        for src in &incoming {
            if src.from_uuid == syn.to_uuid {
                return Err(Refusal::SelfLoop {
                    uuid: src.from_uuid.clone(),
                });
            }
        }
    }

    let mut working = target.clone();
    working.memetic = None;
    for out in &outgoing {
        {
            let downstream = working
                .neurons
                .iter_mut()
                .find(|n| n.uuid == out.to_uuid)
                .ok_or_else(|| Refusal::UnknownNeuron(out.to_uuid.clone()))?;
            downstream.bias += bias_y * out.weight;
        }
        for src in &incoming {
            add_or_merge(
                &mut working,
                &src.from_uuid,
                &out.to_uuid,
                src.weight * out.weight,
            )?;
        }
    }

    let mut removed = vec![uuid.to_string()];
    remove_neuron(&mut working, uuid);
    cleanup_cascade(&mut working, &mut removed)?;
    sort_synapses_canonically(&mut working);
    validate_creature(&working).map_err(|e| Refusal::Invalid(e.to_string()))?;
    Ok((working, removed))
}

/// Remove whatever the transform left dead: a non-output neuron with no
/// outgoing synapse, and a hidden neuron with no incoming synapse (which has
/// become a constant, so it folds exactly into its downstream biases).
fn cleanup_cascade(working: &mut CreatureExport, removed: &mut Vec<String>) -> Result<(), Refusal> {
    loop {
        if let Some(uuid) = first_dead_non_output(working) {
            removed.push(uuid.clone());
            remove_neuron(working, &uuid);
            continue;
        }
        if let Some(uuid) = first_hidden_without_incoming(working) {
            let neuron = neuron_by_uuid(working, &uuid)
                .cloned()
                .ok_or_else(|| Refusal::UnknownNeuron(uuid.clone()))?;
            let squash = squash_of(&neuron)?;
            if squash.is_aggregate() {
                return Err(Refusal::AggregateNeuron {
                    uuid,
                    squash: squash_name(&neuron),
                });
            }
            let constant = f64::from(apply_squash(squash, neuron.bias as f32));
            if !constant.is_finite() {
                return Err(Refusal::UnknownSquash {
                    uuid,
                    squash: squash_name(&neuron),
                });
            }
            let outgoing = synapses_from(working, &uuid);
            for syn in &outgoing {
                require_ordinary(syn)?;
                let downstream = neuron_by_uuid(working, &syn.to_uuid)
                    .ok_or_else(|| Refusal::UnknownNeuron(syn.to_uuid.clone()))?;
                reject_aggregate(downstream)?;
            }
            for syn in outgoing {
                fold_bias(working, &syn, constant)?;
            }
            removed.push(uuid.clone());
            remove_neuron(working, &uuid);
            continue;
        }
        break;
    }
    Ok(())
}

fn first_dead_non_output(working: &CreatureExport) -> Option<String> {
    working.neurons.iter().find_map(|n| {
        if n.neuron_type == "output" {
            return None;
        }
        let out = working
            .synapses
            .iter()
            .filter(|s| s.from_uuid == n.uuid)
            .count();
        (out == 0).then(|| n.uuid.clone())
    })
}

fn first_hidden_without_incoming(working: &CreatureExport) -> Option<String> {
    working.neurons.iter().find_map(|n| {
        if n.neuron_type != "hidden" {
            return None;
        }
        let incoming = working
            .synapses
            .iter()
            .filter(|s| s.to_uuid == n.uuid)
            .count();
        (incoming == 0).then(|| n.uuid.clone())
    })
}

fn fold_bias(
    working: &mut CreatureExport,
    syn: &SynapseExport,
    source_value: f64,
) -> Result<(), Refusal> {
    let downstream = working
        .neurons
        .iter_mut()
        .find(|n| n.uuid == syn.to_uuid)
        .ok_or_else(|| Refusal::UnknownNeuron(syn.to_uuid.clone()))?;
    downstream.bias += source_value * syn.weight;
    Ok(())
}

fn add_or_merge(
    working: &mut CreatureExport,
    from_uuid: &str,
    to_uuid: &str,
    added: f64,
) -> Result<(), Refusal> {
    if let Some(existing) = working
        .synapses
        .iter_mut()
        .find(|s| s.from_uuid == from_uuid && s.to_uuid == to_uuid && s.synapse_type.is_none())
    {
        existing.weight += added;
        return Ok(());
    }
    if let Some(typed) = working
        .synapses
        .iter()
        .find(|s| s.from_uuid == from_uuid && s.to_uuid == to_uuid && s.synapse_type.is_some())
    {
        return Err(Refusal::TypedSynapse {
            from: from_uuid.into(),
            to: to_uuid.into(),
            role: typed.synapse_type.clone().unwrap_or_default(),
        });
    }
    working.synapses.push(SynapseExport {
        from_uuid: from_uuid.into(),
        to_uuid: to_uuid.into(),
        weight: added,
        synapse_type: None,
    });
    Ok(())
}

fn remove_neuron(working: &mut CreatureExport, uuid: &str) {
    working.neurons.retain(|n| n.uuid != uuid);
    working
        .synapses
        .retain(|s| s.from_uuid != uuid && s.to_uuid != uuid);
}

fn synapses_from(working: &CreatureExport, uuid: &str) -> Vec<SynapseExport> {
    working
        .synapses
        .iter()
        .filter(|s| s.from_uuid == uuid)
        .cloned()
        .collect()
}

fn synapses_to(working: &CreatureExport, uuid: &str) -> Vec<SynapseExport> {
    working
        .synapses
        .iter()
        .filter(|s| s.to_uuid == uuid)
        .cloned()
        .collect()
}

fn neuron_by_uuid<'a>(working: &'a CreatureExport, uuid: &str) -> Option<&'a NeuronExport> {
    working.neurons.iter().find(|n| n.uuid == uuid)
}

fn require_ordinary(syn: &SynapseExport) -> Result<(), Refusal> {
    match &syn.synapse_type {
        Some(role) => Err(Refusal::TypedSynapse {
            from: syn.from_uuid.clone(),
            to: syn.to_uuid.clone(),
            role: role.clone(),
        }),
        None => Ok(()),
    }
}

fn reject_aggregate(neuron: &NeuronExport) -> Result<(), Refusal> {
    if squash_of(neuron)?.is_aggregate() {
        Err(Refusal::AggregateTarget {
            uuid: neuron.uuid.clone(),
            squash: squash_name(neuron),
        })
    } else {
        Ok(())
    }
}

fn squash_of(neuron: &NeuronExport) -> Result<SquashType, Refusal> {
    let name = neuron.squash.as_deref().unwrap_or("IDENTITY");
    parse_squash_name(name).map_err(|_| Refusal::UnknownSquash {
        uuid: neuron.uuid.clone(),
        squash: name.to_string(),
    })
}

fn squash_name(neuron: &NeuronExport) -> String {
    neuron.squash.clone().unwrap_or_else(|| "IDENTITY".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::creature::validate_source_creature;
    use crate::fixtures::{creature, neuron, synapse, typed_synapse};
    use neat_core::creature_to_json;

    fn removal(uuid: &str, strategy: RemovalStrategy) -> OckhamRemoval {
        OckhamRemoval {
            neuron_uuid: uuid.into(),
            strategy,
        }
    }

    /// `input-0 → h_a →(3) output`, `input-0 → h_b →(1) output`. Removing
    /// `h_a` folds `mean·3` into the output bias and leaves `h_b` alone.
    fn two_hidden() -> CreatureExport {
        creature(
            1,
            1,
            vec![
                neuron("hidden", "h_a", 0.0, Some("IDENTITY")),
                neuron("hidden", "h_b", 0.0, Some("IDENTITY")),
                neuron("output", "output-0", 0.25, Some("IDENTITY")),
            ],
            vec![
                synapse("input-0", "h_a", 1.0),
                synapse("input-0", "h_b", 1.0),
                synapse("h_a", "output-0", 3.0),
                synapse("h_b", "output-0", 1.0),
            ],
        )
    }

    /// `input-0 → h_up → h_leaf →(2) output` plus an untouched `h_keep`.
    /// Removing `h_leaf` leaves `h_up` with nothing downstream, so the cascade
    /// takes it too.
    fn chain_plus_keep() -> CreatureExport {
        creature(
            1,
            1,
            vec![
                neuron("hidden", "h_up", 0.1, Some("IDENTITY")),
                neuron("hidden", "h_leaf", 0.0, Some("IDENTITY")),
                neuron("hidden", "h_keep", 0.0, Some("IDENTITY")),
                neuron("output", "output-0", 0.0, Some("IDENTITY")),
            ],
            vec![
                synapse("input-0", "h_up", 1.0),
                synapse("h_up", "h_leaf", 1.0),
                synapse("h_leaf", "output-0", 2.0),
                synapse("input-0", "h_keep", 1.0),
                synapse("h_keep", "output-0", 1.0),
            ],
        )
    }

    fn applied(r: &OckhamRemoval, target: &CreatureExport) -> CreatureExport {
        match apply(r, target).unwrap() {
            Application::Applied { creature, .. } => *creature,
            Application::AlreadyPresent => panic!("expected the removal to apply"),
        }
    }

    #[test]
    fn mean_ablation_reproduces_the_expected_transformation() {
        let parent = two_hidden();
        let out = applied(
            &removal("h_a", RemovalStrategy::MeanAblation { mean: 0.5 }),
            &parent,
        );
        validate_source_creature(&out).unwrap();
        assert!(!out.neurons.iter().any(|n| n.uuid == "h_a"));
        let output = out.neurons.iter().find(|n| n.uuid == "output-0").unwrap();
        // 0.25 + 0.5 * 3.0
        assert!((output.bias - 1.75).abs() < 1e-12, "{}", output.bias);
        assert!(out.neurons.iter().any(|n| n.uuid == "h_b"));
    }

    #[test]
    fn the_cascade_takes_structure_the_removal_left_dead() {
        let out = applied(
            &removal("h_leaf", RemovalStrategy::MeanAblation { mean: 0.25 }),
            &chain_plus_keep(),
        );
        validate_source_creature(&out).unwrap();
        assert!(!out.neurons.iter().any(|n| n.uuid == "h_leaf"));
        assert!(
            !out.neurons.iter().any(|n| n.uuid == "h_up"),
            "`h_up` now feeds nothing and must be cleaned up"
        );
        assert!(out.neurons.iter().any(|n| n.uuid == "h_keep"));
    }

    #[test]
    fn a_compatible_evolved_descendant_can_receive_the_removal() {
        // The fleet added `h_new` while Ockham was working. The prune still
        // replays, and the unrelated addition survives.
        let mut descendant = two_hidden();
        descendant
            .neurons
            .insert(2, neuron("hidden", "h_new", 0.0, Some("IDENTITY")));
        descendant.synapses.push(synapse("input-0", "h_new", 0.75));
        descendant.synapses.push(synapse("h_new", "output-0", 1.0));
        sort_synapses_canonically(&mut descendant);
        validate_source_creature(&descendant).unwrap();

        let out = applied(
            &removal("h_a", RemovalStrategy::MeanAblation { mean: 0.5 }),
            &descendant,
        );
        validate_source_creature(&out).unwrap();
        assert!(!out.neurons.iter().any(|n| n.uuid == "h_a"));
        assert!(out.neurons.iter().any(|n| n.uuid == "h_new"));
    }

    #[test]
    fn an_already_removed_uuid_is_idempotently_skipped() {
        let out = applied(
            &removal("h_a", RemovalStrategy::MeanAblation { mean: 0.5 }),
            &two_hidden(),
        );
        assert_eq!(
            apply(
                &removal("h_a", RemovalStrategy::MeanAblation { mean: 0.5 }),
                &out
            )
            .unwrap(),
            Application::AlreadyPresent
        );
        // And on a champion that never had it at all.
        assert_eq!(
            apply(
                &removal("never-existed", RemovalStrategy::IdentityCollapse),
                &two_hidden()
            )
            .unwrap(),
            Application::AlreadyPresent
        );
    }

    #[test]
    fn identity_collapse_bypasses_exactly() {
        // input →(a=2) h_mid(bias 0.5) →(b=3) output(bias 0.25)
        let parent = creature(
            1,
            1,
            vec![
                neuron("hidden", "h_mid", 0.5, Some("IDENTITY")),
                neuron("hidden", "h_keep", 0.0, Some("IDENTITY")),
                neuron("output", "output-0", 0.25, Some("IDENTITY")),
            ],
            vec![
                synapse("input-0", "h_mid", 2.0),
                synapse("input-0", "h_keep", 1.0),
                synapse("h_mid", "output-0", 3.0),
                synapse("h_keep", "output-0", 1.0),
            ],
        );
        let out = applied(
            &removal("h_mid", RemovalStrategy::IdentityCollapse),
            &parent,
        );
        validate_source_creature(&out).unwrap();
        let output = out.neurons.iter().find(|n| n.uuid == "output-0").unwrap();
        assert!((output.bias - (0.25 + 0.5 * 3.0)).abs() < 1e-12);
        let bypass = out
            .synapses
            .iter()
            .find(|s| s.from_uuid == "input-0" && s.to_uuid == "output-0")
            .expect("x -> z bypass");
        assert!((bypass.weight - 6.0).abs() < 1e-12, "{bypass:?}");
    }

    #[test]
    fn a_recorded_identity_collapse_is_never_silently_downgraded() {
        // The champion's `h_a` is TANH, not IDENTITY: the recorded exact
        // collapse cannot be reproduced, so it fails closed rather than
        // substituting the approximate ablation.
        let mut parent = two_hidden();
        parent.neurons[0].squash = Some("TANH".into());
        let err = apply(&removal("h_a", RemovalStrategy::IdentityCollapse), &parent).unwrap_err();
        assert!(err.to_string().contains("not IDENTITY"), "{err}");
    }

    #[test]
    fn a_structural_conflict_fails_closed_with_a_reason() {
        // A typed synapse incident to the neuron: the `IF` role arithmetic is
        // not a plain sum, so neither strategy can fold it.
        let parent = creature(
            1,
            1,
            vec![
                neuron("constant", "one", 1.0, None),
                neuron("hidden", "h_cond", 0.0, Some("IDENTITY")),
                neuron("hidden", "h_if", 0.0, Some("IF")),
                neuron("output", "output-0", 0.0, Some("IDENTITY")),
            ],
            vec![
                synapse("input-0", "h_cond", 1.0),
                typed_synapse("h_cond", "h_if", 1.0, "condition"),
                typed_synapse("one", "h_if", 1.0, "positive"),
                typed_synapse("one", "h_if", -1.0, "negative"),
                synapse("h_if", "output-0", 1.0),
            ],
        );
        validate_source_creature(&parent).unwrap();
        let err = apply(
            &removal("h_cond", RemovalStrategy::MeanAblation { mean: 0.5 }),
            &parent,
        )
        .unwrap_err();
        assert!(err.to_string().contains("typed synapse"), "{err}");
    }

    #[test]
    fn a_non_hidden_target_and_a_non_finite_mean_fail_closed() {
        let parent = two_hidden();
        let err = apply(
            &removal("output-0", RemovalStrategy::MeanAblation { mean: 0.5 }),
            &parent,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not hidden"), "{err}");

        let err = apply(
            &removal(
                "h_a",
                RemovalStrategy::MeanAblation {
                    mean: f64::INFINITY,
                },
            ),
            &parent,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not finite"), "{err}");
    }

    #[test]
    fn an_aggregate_downstream_neighbour_fails_closed() {
        let parent = creature(
            1,
            1,
            vec![
                neuron("constant", "one", 1.0, None),
                neuron("hidden", "h_a", 0.0, Some("IDENTITY")),
                neuron("output", "output-0", 0.0, Some("MINIMUM")),
            ],
            vec![
                synapse("input-0", "h_a", 1.0),
                synapse("h_a", "output-0", 1.0),
                synapse("one", "output-0", 1.0),
            ],
        );
        let err = apply(
            &removal("h_a", RemovalStrategy::MeanAblation { mean: 0.5 }),
            &parent,
        )
        .unwrap_err();
        assert!(err.to_string().contains("aggregate"), "{err}");
    }

    #[test]
    fn the_source_champion_is_never_modified() {
        let parent = two_hidden();
        let before = creature_to_json(&parent).unwrap();
        let _ = applied(
            &removal("h_a", RemovalStrategy::MeanAblation { mean: 0.5 }),
            &parent,
        );
        assert_eq!(creature_to_json(&parent).unwrap(), before);
    }

    #[test]
    fn removed_uuids_report_the_cascade_too() {
        let out = apply(
            &removal("h_leaf", RemovalStrategy::MeanAblation { mean: 0.25 }),
            &chain_plus_keep(),
        )
        .unwrap();
        match out {
            Application::Applied { removed_uuids, .. } => {
                assert!(removed_uuids.contains(&"h_leaf".to_string()));
                assert!(removed_uuids.contains(&"h_up".to_string()));
            }
            Application::AlreadyPresent => panic!("expected the removal to apply"),
        }
    }
}
