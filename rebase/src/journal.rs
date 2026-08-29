//! Append-only run journal for unattended diagnostics (Issue #6).
//!
//! Every rebase writes one JSON object per line to `experiments.jsonl`. The
//! point is that a machine nobody is watching can be asked, days later, *why*
//! it did or did not publish a candidate — and the answer has to name the
//! opening ancestor, the fresh champion, what happened to each enhancement,
//! and the scorer's own numbers.
//!
//! Records are appended, never rewritten, so a crash mid-run leaves everything
//! written before it intact.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::engine::{EnhancementOutcome, RebaseOutcome};
use crate::scorer::Verdict;

/// Label prefix of the [`Record::Dropped`] record each screen phase writes.
///
/// Shared between the writer and `neat_ai_rebase report`, so a reader can tell
/// that a run screened without matching a string the writer is free to change.
pub const SCREEN_PHASE_LABEL_PREFIX: &str = "screen-phase-";

/// One line of the journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "record"
)]
pub enum Record {
    /// What the run was asked to do.
    Opening {
        /// Producer of the bundle.
        producer: String,
        /// Checksum of the creature the producer opened on — the stale
        /// ancestor.
        opening_checksum: String,
        /// Checksum of the champion freshly fetched for this rebase.
        champion_checksum: String,
        /// Corpus the decision is being made on.
        corpus_identity: String,
        /// Enhancements in the bundle.
        enhancement_count: usize,
    },
    /// What happened to one enhancement.
    Enhancement {
        /// Stable enhancement id.
        id: String,
        /// Payload kind.
        kind: String,
        /// Producer.
        producer: String,
        /// `applied` / `alreadyPresent` / `incompatible`.
        outcome: String,
        /// Reason, when the outcome is `incompatible`.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The gain the producer measured on its own opening creature.
        claimed_gain: f64,
    },
    /// A candidate that was constructed.
    Candidate {
        /// Cohort label.
        label: String,
        /// Checksum of the candidate creature.
        checksum: String,
        /// Enhancement ids applied.
        applied_ids: Vec<String>,
    },
    /// A candidate that was constructed and then dropped to honour the cap, or
    /// a combination that could not be constructed. Never silent.
    Dropped {
        /// What was dropped.
        label: String,
        /// Why.
        reason: String,
    },
    /// The authoritative verdict.
    Verdict(Box<Verdict>),
    /// The final outcome of the run, and whether anything was emitted.
    Result {
        /// `improved` / `noImprovement` / `nothingToDo` / `failed`.
        status: String,
        /// Detail, for a failure or a refusal.
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// Checksum of the creature written as `population-candidate.json`.
        #[serde(skip_serializing_if = "Option::is_none")]
        emitted_checksum: Option<String>,
    },
}

/// An append-only journal file.
#[derive(Debug, Clone)]
pub struct Journal {
    path: std::path::PathBuf,
}

impl Journal {
    /// A journal appending to `path`. The file is created on first write.
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Where the journal is being written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record.
    ///
    /// # Errors
    ///
    /// Returns a message when the file cannot be opened or written. A journal
    /// failure is reported, never swallowed: an unattended run whose journal is
    /// silently missing is an unattended run nobody can debug.
    pub fn append(&self, record: &Record) -> Result<(), String> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let line = serde_json::to_string(record).map_err(|e| e.to_string())?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("{}: {e}", self.path.display()))?;
        writeln!(file, "{line}").map_err(|e| format!("{}: {e}", self.path.display()))
    }

    /// Append the per-enhancement, per-candidate and dropped records for one
    /// engine outcome.
    ///
    /// # Errors
    ///
    /// The first write failure.
    pub fn append_outcome(&self, outcome: &RebaseOutcome) -> Result<(), String> {
        for report in &outcome.reports {
            let reason = match &report.outcome {
                EnhancementOutcome::Incompatible(r) => Some(r.clone()),
                _ => None,
            };
            self.append(&Record::Enhancement {
                id: report.id.clone(),
                kind: report.kind.to_string(),
                producer: report.producer.clone(),
                outcome: report.outcome.label().to_string(),
                reason,
                claimed_gain: report.claimed_gain,
            })?;
        }
        for candidate in &outcome.cohort {
            self.append(&Record::Candidate {
                label: candidate.label.clone(),
                checksum: candidate.checksum.clone(),
                applied_ids: candidate.applied_ids.clone(),
            })?;
        }
        for label in &outcome.dropped_for_cap {
            self.append(&Record::Dropped {
                label: label.clone(),
                reason: "candidate cap reached".into(),
            })?;
        }
        for failure in &outcome.combination_failures {
            self.append(&Record::Dropped {
                label: "combination".into(),
                reason: failure.clone(),
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{RebaseRequest, rebase};
    use crate::enhancement::{Enhancement, Payload, ProducerContext};
    use crate::fixtures::linear_hidden_creature;
    use crate::patch::{Node, Patch, Provenance};

    fn forest(feature: usize) -> Enhancement {
        Enhancement::new(
            Payload::ForestPatch {
                patch: Patch::new(
                    0,
                    Node::stump(feature, 0.5, 0.0, 0.25),
                    Provenance::default(),
                ),
            },
            &ProducerContext {
                producer: "neat-ai-forests/test".into(),
                base_checksum: "base".into(),
                base_score: 0.5,
                improved_score: 0.6,
                corpus_identity: "corpus-1".into(),
                input_count: 2,
                output_count: 1,
            },
        )
    }

    #[test]
    fn records_append_one_json_object_per_line() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = Journal::new(tmp.path().join("runs").join("experiments.jsonl"));
        journal
            .append(&Record::Opening {
                producer: "neat-ai-forests/test".into(),
                opening_checksum: "a".into(),
                champion_checksum: "b".into(),
                corpus_identity: "corpus-1".into(),
                enhancement_count: 2,
            })
            .unwrap();

        let champion = linear_hidden_creature(2.0);
        // One good, one that names a feature the champion does not have.
        let outcome = rebase(&RebaseRequest {
            champion: &champion,
            enhancements: &[forest(0), forest(9)],
            corpus_identity: "corpus-1",
            max_candidates: 0,
        })
        .unwrap();
        journal.append_outcome(&outcome).unwrap();
        journal
            .append(&Record::Result {
                status: "noImprovement".into(),
                detail: None,
                emitted_checksum: None,
            })
            .unwrap();

        let text = std::fs::read_to_string(journal.path()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.len() >= 5);
        for line in &lines {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(value.get("record").is_some(), "{line}");
        }
        assert!(text.contains(r#""outcome":"incompatible""#), "{text}");
        assert!(text.contains(r#""reason""#), "{text}");
        assert!(text.contains(r#""record":"opening""#), "{text}");
        assert!(text.contains(r#""claimedGain""#), "{text}");
    }

    #[test]
    fn a_second_run_appends_rather_than_replacing() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = Journal::new(tmp.path().join("experiments.jsonl"));
        let record = Record::Result {
            status: "noImprovement".into(),
            detail: None,
            emitted_checksum: None,
        };
        journal.append(&record).unwrap();
        journal.append(&record).unwrap();
        assert_eq!(
            std::fs::read_to_string(journal.path())
                .unwrap()
                .lines()
                .count(),
            2
        );
    }
}
