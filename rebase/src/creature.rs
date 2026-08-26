//! Creature helpers shared by every adapter: identity, canonical ordering and
//! the validation gate a rebased candidate has to clear.
//!
//! Rebase never mutates a creature it is given. Every adapter works on a clone
//! and returns a new [`CreatureExport`]; the champion the caller handed in is
//! still byte-for-byte what it was, which
//! `champion_bytes_unchanged_after_a_full_rebase` pins.

use std::collections::HashMap;
use std::fmt;

use neat_core::{
    CreatureExport, ValidateOptions, compile_creature, creature_to_json, creature_validate,
    parse_creature_json, parse_synapse_type, validate_no_duplicate_synapses,
};
use sha2::{Digest, Sha256};

/// SHA-256 hex digest of a byte slice.
///
/// The fleet's creature checksum: NEAT-AI-Forests, NEAT-AI-Ockham and Rebase
/// all identify a creature by the SHA-256 of its JSON bytes, so a
/// `baseChecksum` recorded by a producer is comparable here without conversion.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// Checksum of a creature's canonical JSON serialisation.
///
/// Used to de-duplicate a candidate cohort: two enhancement orderings that
/// produce the same creature produce the same checksum and only one candidate
/// is scored.
///
/// # Errors
///
/// Returns the serialisation failure text when `neat_core` cannot emit the
/// creature.
pub fn creature_checksum(creature: &CreatureExport) -> Result<String, String> {
    let json = creature_to_json(creature).map_err(|e| e.to_string())?;
    Ok(sha256_hex(json.as_bytes()))
}

/// Why a creature was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum CreatureFault {
    /// NEAT-AI-core could not compile it.
    Compile(String),
    /// A repeated `(from, to, type)` synapse triple.
    DuplicateSynapse(String),
    /// The shared `creature_validate` contract rejected it.
    Invalid(String),
    /// Serialise → parse did not reproduce the creature.
    RoundTrip(String),
    /// Output neurons are not the trailing entries of `neurons`.
    OutputsNotLast,
}

impl fmt::Display for CreatureFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compile(m) => write!(f, "creature does not compile: {m}"),
            Self::DuplicateSynapse(m) => write!(f, "duplicate synapse: {m}"),
            Self::Invalid(m) => write!(f, "creature failed validation: {m}"),
            Self::RoundTrip(m) => write!(f, "creature does not round-trip: {m}"),
            Self::OutputsNotLast => {
                write!(
                    f,
                    "output neurons are not the trailing entries of `neurons`"
                )
            }
        }
    }
}

impl std::error::Error for CreatureFault {}

/// The [`ValidateOptions`] Rebase gates a candidate with.
///
/// `neurons` / `connections` stay `None`: a rebase *changes* both counts by
/// construction, so pinning them would only restate what it just built.
/// `feedback_loop` stays `None` — the creature's own `forwardOnly` declaration
/// decides, via `forward_only`. A creature that declares itself recurrent is
/// not failed for recursion the rebase did not introduce.
fn validate_options(creature: &CreatureExport) -> ValidateOptions {
    ValidateOptions {
        neurons: None,
        connections: None,
        feedback_loop: None,
        forward_only: creature.forward_only,
    }
}

/// Gate a creature on the shared definition of valid, before it can escape an
/// adapter or reach the scorer.
///
/// Three rules, one gate — the same order Forests and Ockham apply so a
/// candidate refused here is refused there for the same stated reason:
///
/// 1. no repeated `(from, to, type)` synapse triple — NEAT-AI's TypeScript
///    loader keys synapses by that triple and silently collapses duplicates,
///    which `rust_scorer` does not, so a creature carrying them means two
///    different creatures to the two judges;
/// 2. `neat_core::creature_validate` — every structural rule;
/// 3. `compile_creature` — the runtime the scorer will use.
///
/// # Errors
///
/// Returns the first [`CreatureFault`] the creature trips.
pub fn validate_creature(creature: &CreatureExport) -> Result<(), CreatureFault> {
    validate_no_duplicate_synapses(creature)
        .map_err(|e| CreatureFault::DuplicateSynapse(e.to_string()))?;
    creature_validate(creature, &validate_options(creature))
        .map(|_stats| ())
        .map_err(|e| CreatureFault::Invalid(e.to_string()))?;
    compile_creature(creature).map_err(|e| CreatureFault::Compile(e.to_string()))?;
    Ok(())
}

/// [`validate_creature`] plus a JSON round trip, for a creature that arrived
/// from outside — a champion read from disk, or an enhancement's own fixture.
///
/// serde_json's default float parser is not correctly rounded, so a round trip
/// may move a weight by one ulp; the contract is therefore structural equality
/// with a `1e-12` relative tolerance on weights and biases, the same tolerance
/// Forests and Ockham apply.
///
/// # Errors
///
/// Returns the first [`CreatureFault`] the creature trips.
pub fn validate_source_creature(creature: &CreatureExport) -> Result<(), CreatureFault> {
    validate_creature(creature)?;
    let json = creature_to_json(creature).map_err(|e| CreatureFault::RoundTrip(e.to_string()))?;
    let again = parse_creature_json(&json).map_err(|e| CreatureFault::RoundTrip(e.to_string()))?;
    equivalent(creature, &again).map_err(CreatureFault::RoundTrip)?;
    outputs_are_last(creature)?;
    Ok(())
}

/// `Ok(())` when the output neurons are the trailing entries of `neurons` —
/// the layout every output index in this crate assumes.
///
/// # Errors
///
/// [`CreatureFault::OutputsNotLast`] otherwise.
pub fn outputs_are_last(creature: &CreatureExport) -> Result<(), CreatureFault> {
    let n = creature.neurons.len();
    if n < creature.output
        || creature.neurons[n - creature.output..]
            .iter()
            .any(|x| x.neuron_type != "output")
    {
        return Err(CreatureFault::OutputsNotLast);
    }
    Ok(())
}

/// UUID of output neuron `index`, given [`outputs_are_last`] holds.
///
/// # Errors
///
/// [`CreatureFault::OutputsNotLast`] when the layout does not hold; `None` is
/// impossible once it does, so an out-of-range `index` is the caller's own
/// precondition to check.
pub fn output_uuid(creature: &CreatureExport, index: usize) -> Result<String, CreatureFault> {
    outputs_are_last(creature)?;
    let n = creature.neurons.len();
    Ok(creature.neurons[n - creature.output + index].uuid.clone())
}

/// Structural equality with a `1e-12` relative tolerance on weights/biases.
fn equivalent(a: &CreatureExport, b: &CreatureExport) -> Result<(), String> {
    if a.input != b.input || a.output != b.output {
        return Err("input/output width changed".into());
    }
    if a.neurons.len() != b.neurons.len() || a.synapses.len() != b.synapses.len() {
        return Err("neuron/synapse count changed".into());
    }
    for (x, y) in a.neurons.iter().zip(&b.neurons) {
        if x.uuid != y.uuid || x.neuron_type != y.neuron_type || x.squash != y.squash {
            return Err(format!("neuron `{}` changed", x.uuid));
        }
        if !close(x.bias, y.bias) {
            return Err(format!(
                "bias of `{}` moved {} -> {}",
                x.uuid, x.bias, y.bias
            ));
        }
    }
    for (x, y) in a.synapses.iter().zip(&b.synapses) {
        if x.from_uuid != y.from_uuid || x.to_uuid != y.to_uuid || x.synapse_type != y.synapse_type
        {
            return Err(format!(
                "synapse `{}`->`{}` changed",
                x.from_uuid, x.to_uuid
            ));
        }
        if !close(x.weight, y.weight) {
            return Err(format!(
                "weight of `{}`->`{}` moved {} -> {}",
                x.from_uuid, x.to_uuid, x.weight, y.weight
            ));
        }
    }
    Ok(())
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-12 * a.abs().max(b.abs()).max(1.0)
}

/// Sort synapses by `(from index, to index, type)` — `creature_validate`
/// rule 25.
///
/// An endpoint naming no neuron sorts last (`usize::MAX`) rather than being
/// dropped, so the validator still reports it instead of the sort quietly
/// reordering a broken creature.
pub fn sort_synapses_canonically(creature: &mut CreatureExport) {
    let mut index: HashMap<String, usize> =
        HashMap::with_capacity(creature.input + creature.neurons.len());
    for i in 0..creature.input {
        index.insert(format!("input-{i}"), i);
    }
    for (j, neuron) in creature.neurons.iter().enumerate() {
        index.insert(neuron.uuid.clone(), creature.input + j);
    }
    let resolve = |uuid: &str| index.get(uuid).copied().unwrap_or(usize::MAX);
    creature.synapses.sort_by_key(|s| {
        (
            resolve(&s.from_uuid),
            resolve(&s.to_uuid),
            parse_synapse_type(s.synapse_type.as_deref()) as u8,
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{identity_creature, linear_hidden_creature};

    #[test]
    fn checksum_is_stable_and_distinguishes_creatures() {
        let a = identity_creature(2, 1);
        let b = identity_creature(2, 1);
        assert_eq!(
            creature_checksum(&a).unwrap(),
            creature_checksum(&b).unwrap()
        );
        let c = linear_hidden_creature(2.0);
        assert_ne!(
            creature_checksum(&a).unwrap(),
            creature_checksum(&c).unwrap()
        );
    }

    #[test]
    fn fixtures_pass_the_source_gate() {
        validate_source_creature(&identity_creature(3, 1)).unwrap();
        validate_source_creature(&linear_hidden_creature(2.0)).unwrap();
    }

    #[test]
    fn output_uuid_resolves_the_trailing_block() {
        let c = identity_creature(2, 2);
        assert_eq!(output_uuid(&c, 0).unwrap(), "output-0");
        assert_eq!(output_uuid(&c, 1).unwrap(), "output-1");
    }

    #[test]
    fn outputs_not_last_is_refused() {
        let mut c = identity_creature(1, 1);
        c.neurons.push(neat_core::NeuronExport {
            id: None,
            neuron_type: "hidden".into(),
            uuid: "trailing-hidden".into(),
            bias: 0.0,
            squash: Some("IDENTITY".into()),
        });
        assert_eq!(outputs_are_last(&c), Err(CreatureFault::OutputsNotLast));
    }
}
