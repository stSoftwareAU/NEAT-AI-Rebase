//! NEAT-AI-Rebase — rebase the improvement, don't replace the champion.
//!
//! A long-running optimiser starts from creature **A**, discovers an
//! improvement **Δ**, and finishes after the fleet has already evolved to
//! creature **B**. Publishing `A + Δ` throws away everything that made B better
//! than A. Rebase treats Δ as the artefact worth keeping:
//!
//! ```text
//! A ── optimiser ──▶ A + Δ
//! │
//! └──────── fleet evolves ────────▶ B
//!                                   │
//!                                   └─ reapply Δ ─▶ B + Δ
//!                                                  │
//!                                                  └─ authoritative scorer decides
//! ```
//!
//! ## The pipeline
//!
//! | Stage | Module | Contract |
//! | --- | --- | --- |
//! | file the change | [`prune_log`] | producer side: each accepted Ockham prune becomes a v1 enhancement, stamped with the opening facts |
//! | portable change | [`enhancement`] | versioned envelope; unknown versions and kinds fail closed |
//! | can it be attempted? | [`compat`] | version, identity, corpus, dimensions |
//! | is it already there? | [`adapter`] | idempotence, per kind |
//! | construct the candidate | [`forest`], [`ockham`] | never mutate the champion |
//! | build a cohort | [`engine`] | baseline, singles, prefixes, full bundle; de-duplicated |
//! | decide | [`scorer`] | NEAT-AI-scorer, one call, fail closed |
//!
//! ## The rules that do not bend
//!
//! * **The scorer has the final say.** Previous success is evidence, never
//!   permission. A candidate proven on an old ancestor is still rejected on a
//!   new champion when it does not beat it.
//! * **The champion is never modified.** Every adapter clones.
//! * **Idempotence beats host exclusion.** If the champion already carries an
//!   enhancement, that is recognised — Rebase needs no special case for "the
//!   creature this host just published".
//! * **No assumption of additivity.** Two changes that each helped separately
//!   may interact badly, so combinations are scored, not trusted.
//! * **A no-improvement verdict is a normal result**, not an operational
//!   failure.
//!
//! ## Quick start
//!
//! ```no_run
//! use neat_ai_rebase::{
//!     engine::{RebaseRequest, rebase},
//!     enhancement::EnhancementBundle,
//! };
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let champion = neat_ai_rebase::fixtures::linear_hidden_creature(2.0);
//! # let bundle: EnhancementBundle = serde_json::from_str("{}")?;
//! let outcome = rebase(&RebaseRequest {
//!     champion: &champion,
//!     enhancements: &bundle.enhancements,
//!     corpus_identity: "3f2a1b0c9d8e7f65",
//!     max_candidates: 8,
//! })?;
//! for candidate in &outcome.cohort {
//!     println!("{} <- {:?}", candidate.label, candidate.applied_ids);
//! }
//! # Ok(())
//! # }
//! ```

#![doc(html_root_url = "https://docs.rs/neat-ai-rebase")]
#![warn(missing_docs)]

pub mod adapter;
pub mod cli;
pub mod compat;
pub mod corpus;
pub mod creature;
pub mod engine;
pub mod enhancement;
pub mod fixtures;
pub mod forest;
pub mod harvest;
pub mod journal;
pub mod ockham;
pub mod patch;
pub mod prune_log;
pub mod scorer;
pub mod tags;

pub use adapter::Application;
pub use compat::{Incompatibility, Target};
pub use engine::{Candidate, RebaseOutcome, RebaseRequest, rebase};
pub use enhancement::{
    ENHANCEMENT_FORMAT_VERSION, Enhancement, EnhancementBundle, EnhancementMeta, OckhamRemoval,
    Payload, ProducerContext, RemovalStrategy,
};
pub use patch::Patch;
pub use prune_log::{PruneLog, PruneLogError};
pub use scorer::{DirectoryScorer, ExternalScorer, ScoreResult, ScorerError, Verdict, judge};
