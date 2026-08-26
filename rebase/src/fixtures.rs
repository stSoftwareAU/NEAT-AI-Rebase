//! Creature fixtures shared by unit tests, the race-condition suite and the
//! CLI's end-to-end test.
//!
//! They are ordinary public API rather than `#[cfg(test)]` helpers so an
//! integration test in `tests/` can build the same shapes the unit tests use,
//! and so a producer writing its own adapter has a working example to copy.

use neat_core::{CreatureExport, NeuronExport, SynapseExport};

/// Listed neuron constructor.
pub fn neuron(neuron_type: &str, uuid: &str, bias: f64, squash: Option<&str>) -> NeuronExport {
    NeuronExport {
        id: None,
        neuron_type: neuron_type.into(),
        uuid: uuid.into(),
        bias,
        squash: squash.map(str::to_string),
    }
}

/// Ordinary (untyped) synapse constructor.
pub fn synapse(from_uuid: &str, to_uuid: &str, weight: f64) -> SynapseExport {
    SynapseExport {
        from_uuid: from_uuid.into(),
        to_uuid: to_uuid.into(),
        weight,
        synapse_type: None,
    }
}

/// Typed synapse constructor (`condition` / `positive` / `negative`).
pub fn typed_synapse(
    from_uuid: &str,
    to_uuid: &str,
    weight: f64,
    synapse_type: &str,
) -> SynapseExport {
    SynapseExport {
        from_uuid: from_uuid.into(),
        to_uuid: to_uuid.into(),
        weight,
        synapse_type: Some(synapse_type.into()),
    }
}

/// Forward-only creature wrapping the supplied neurons and synapses, with the
/// synapse list left in canonical order (`creature_validate` rule 25).
pub fn creature(
    input: usize,
    output: usize,
    neurons: Vec<NeuronExport>,
    synapses: Vec<SynapseExport>,
) -> CreatureExport {
    let mut creature = CreatureExport {
        input,
        output,
        neurons,
        synapses,
        semantic_version: Some("4.0.0".into()),
        forward_only: true,
        memetic: None,
    };
    crate::creature::sort_synapses_canonically(&mut creature);
    creature
}

/// Minimal forward-only creature: each output is the identity of `input-j`
/// (or `input-0` when there are fewer inputs than outputs).
///
/// # Panics
///
/// Panics when `inputs` or `outputs` is zero — a creature of either shape has
/// no meaning and `neat_core` refuses it anyway.
pub fn identity_creature(inputs: usize, outputs: usize) -> CreatureExport {
    assert!(inputs >= 1 && outputs >= 1);
    let neurons = (0..outputs)
        .map(|j| neuron("output", &format!("output-{j}"), 0.0, Some("IDENTITY")))
        .collect();
    let synapses = (0..outputs)
        .map(|j| {
            synapse(
                &format!("input-{}", j.min(inputs - 1)),
                &format!("output-{j}"),
                1.0,
            )
        })
        .collect();
    creature(inputs, outputs, neurons, synapses)
}

/// `output-0 = IDENTITY(weight * input-0)` through one hidden IDENTITY neuron
/// `h1` — the "creature A" of the race fixtures, and the shape an Ockham
/// removal targets.
pub fn linear_hidden_creature(weight: f64) -> CreatureExport {
    creature(
        2,
        1,
        vec![
            neuron("hidden", "h1", 0.0, Some("IDENTITY")),
            neuron("output", "output-0", 0.0, Some("IDENTITY")),
        ],
        vec![
            synapse("input-0", "h1", weight),
            synapse("h1", "output-0", 1.0),
        ],
    )
}

/// [`linear_hidden_creature`] with a second, independently evolved hidden
/// neuron `h2` reading `input-1` — "creature B", the champion the fleet moved
/// on to while an optimiser was still working on A.
///
/// The point of the shape: `h2` is the unrelated fleet improvement a stale
/// `A + Δ` republish would destroy, and it is untouched by either adapter.
pub fn evolved_descendant(weight: f64, second_weight: f64) -> CreatureExport {
    creature(
        2,
        1,
        vec![
            neuron("hidden", "h1", 0.0, Some("IDENTITY")),
            neuron("hidden", "h2", 0.0, Some("IDENTITY")),
            neuron("output", "output-0", 0.0, Some("IDENTITY")),
        ],
        vec![
            synapse("input-0", "h1", weight),
            synapse("input-1", "h2", second_weight),
            synapse("h1", "output-0", 1.0),
            synapse("h2", "output-0", 1.0),
        ],
    )
}

/// A creature whose single output is a `MINIMUM` clamp over a hidden IDENTITY
/// body — the shape that forces the Forest adapter's anchor walk to step past
/// the clamp instead of adding to the output directly.
pub fn clamped_output_creature() -> CreatureExport {
    creature(
        2,
        1,
        vec![
            neuron("constant", "clamp-const", 5.0, None),
            neuron("hidden", "body", 0.0, Some("IDENTITY")),
            neuron("output", "output-0", 0.0, Some("MINIMUM")),
        ],
        vec![
            synapse("input-0", "body", 1.0),
            synapse("body", "output-0", 2.0),
            synapse("clamp-const", "output-0", 1.0),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::creature::validate_source_creature;

    #[test]
    fn every_fixture_passes_the_source_gate() {
        validate_source_creature(&identity_creature(2, 1)).unwrap();
        validate_source_creature(&linear_hidden_creature(2.0)).unwrap();
        validate_source_creature(&evolved_descendant(2.0, 0.5)).unwrap();
        validate_source_creature(&clamped_output_creature()).unwrap();
    }

    #[test]
    fn typed_synapse_carries_its_role() {
        let s = typed_synapse("a", "b", 1.0, "condition");
        assert_eq!(s.synapse_type.as_deref(), Some("condition"));
    }
}
