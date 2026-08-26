//! NEAT-AI-Rebase
//!
//! Rebase portable, scorer-proven enhancements from a stale ancestor onto the
//! latest champion. The authoritative scorer remains the final judge.

use serde::{Deserialize, Serialize};

/// Version of the portable enhancement envelope.
pub const ENHANCEMENT_FORMAT_VERSION: u32 = 1;

/// Provenance shared by every enhancement kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnhancementMeta {
    pub version: u32,
    pub id: String,
    pub producer: String,
    pub base_checksum: String,
    pub base_score: f64,
    pub improved_score: f64,
    pub corpus_identity: String,
}

/// Version-1 enhancement types.
///
/// Concrete Forest and Ockham payloads are added by the implementation issues;
/// the envelope starts intentionally small rather than guessing their formats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Enhancement {
    ForestPatch {
        meta: EnhancementMeta,
        payload: serde_json::Value,
    },
    OckhamRemoval {
        meta: EnhancementMeta,
        neuron_uuid: String,
        removal_kind: String,
    },
}

impl Enhancement {
    pub fn meta(&self) -> &EnhancementMeta {
        match self {
            Self::ForestPatch { meta, .. } | Self::OckhamRemoval { meta, .. } => meta,
        }
    }
}
