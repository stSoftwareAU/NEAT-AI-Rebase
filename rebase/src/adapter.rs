//! What an adapter returns, and the dispatch from a payload kind to the
//! adapter that understands it.
//!
//! An adapter has exactly two jobs: decide whether an enhancement is *already
//! present* on the target, and, when it is not, *construct* the rebased
//! creature. It never scores, never accepts and never mutates the target.

use neat_core::CreatureExport;

use crate::compat::{Incompatibility, Target};
use crate::enhancement::{Enhancement, Payload};

/// The result of applying one enhancement to one target.
#[derive(Debug, Clone, PartialEq)]
pub enum Application {
    /// The enhancement was applied; here is the new creature.
    Applied {
        /// The rebased creature. The target is unchanged.
        creature: Box<CreatureExport>,
        /// Neuron UUIDs the enhancement added, in listed order.
        added_uuids: Vec<String>,
        /// Neuron UUIDs the enhancement removed.
        removed_uuids: Vec<String>,
    },
    /// The target already carries this enhancement. A clean no-op, not an
    /// error: an optimiser whose own result has already reached the population
    /// must not graft a second copy of its own work.
    AlreadyPresent,
}

impl Application {
    /// The rebased creature, or `None` when the enhancement was already present.
    pub fn creature(&self) -> Option<&CreatureExport> {
        match self {
            Self::Applied { creature, .. } => Some(creature),
            Self::AlreadyPresent => None,
        }
    }
}

/// `true` when `target` already carries `enhancement`.
///
/// Cheap and side-effect free — the engine calls it before attempting
/// anything, and a producer can call it to decide whether a rebase is worth
/// starting at all.
pub fn is_present(enhancement: &Enhancement, target: &CreatureExport) -> bool {
    match &enhancement.payload {
        Payload::ForestPatch { patch } => crate::forest::is_present(patch, target),
        Payload::OckhamRemoval { removal } => crate::ockham::is_present(removal, target),
    }
}

/// Apply one enhancement to a clone of `target.creature`.
///
/// Common compatibility ([`crate::compat::check_common`]) is the engine's
/// responsibility and is **not** repeated here; what this adds is the
/// operation-specific preconditions and the construction itself.
///
/// # Errors
///
/// [`Incompatibility::Precondition`] when the adapter cannot reproduce the
/// change safely on this target. Nothing partial escapes.
pub fn apply(
    enhancement: &Enhancement,
    target: &Target<'_>,
) -> Result<Application, Incompatibility> {
    match &enhancement.payload {
        Payload::ForestPatch { patch } => crate::forest::apply(patch, target.creature),
        Payload::OckhamRemoval { removal } => crate::ockham::apply(removal, target.creature),
    }
}
