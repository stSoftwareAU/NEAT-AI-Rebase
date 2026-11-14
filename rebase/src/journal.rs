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

/// Label prefix of the [`Record::Dropped`] record screen phases wrote before
/// [`Record::Screen`] replaced it (Issue #43).
///
/// Still recognised on the *read* side: `neat_ai_rebase report` is pointed at
/// journals older runs wrote, and a soak whose phases are all in this shape has
/// to keep counting as a screened run.
pub const SCREEN_PHASE_LABEL_PREFIX: &str = "screen-phase-";

/// `record` tag of [`Record::Screen`], as it appears on the wire.
///
/// Shared between the writer and `neat_ai_rebase report` so a reader can tell
/// that a run screened without matching a string the writer is free to change.
pub const SCREEN_RECORD: &str = "screen";

/// Label of the [`Record::Dropped`] record written when the screen was asked
/// for but could not pay for itself (Issue #42).
///
/// Deliberately not prefixed with [`SCREEN_PHASE_LABEL_PREFIX`]: a run that
/// skipped the screen did not screen, and `neat_ai_rebase report` must not
/// count it among the runs whose screen agreed or disagreed with the corpus.
pub const SCREEN_SKIPPED_LABEL: &str = "screen-skipped";

/// What one screen phase's stratum said about one enhancement (Issue #43).
///
/// The distinction that matters is between [`Self::Worse`] — the stratum saw a
/// loss — and [`Self::Indistinguishable`] — the stratum saw nothing. A count of
/// survivors conflates the two, and they call for opposite responses: the first
/// is the screen working, the second is a stratum with no power over this
/// candidate. Only [`Self::Worse`] eliminates; see [`Self::survives`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScreenVerdict {
    /// Above the baseline by more than the run's resolution.
    Better,
    /// Within the run's resolution of the baseline, an exact tie included: the
    /// stratum could not tell the candidate and the champion apart.
    Indistinguishable,
    /// Below the baseline by more than the run's resolution — the only verdict
    /// that drops anything.
    Worse,
    /// The screen returned no score for it. No evidence, so no elimination.
    NotScored,
    /// No candidate was built for it, so the stratum never saw it at all.
    NotBuilt,
}

impl ScreenVerdict {
    /// Classify one sampled `score` against the stratum's `baseline`.
    ///
    /// `resolution` is the run's `--min-improvement`: a difference smaller than
    /// the margin the run would promote on is not a difference. [`Self::NotBuilt`]
    /// never comes from here — nothing was scored to classify.
    pub fn classify(score: Option<f64>, baseline: f64, resolution: f64) -> Self {
        match score {
            None => Self::NotScored,
            Some(score) if baseline - score > resolution => Self::Worse,
            Some(score) if score - baseline > resolution => Self::Better,
            Some(_) => Self::Indistinguishable,
        }
    }

    /// Whether the enhancement survives the phase.
    ///
    /// Elimination is one-sided (Issue #42): a graft is an `IF` subtree firing
    /// on a subset of records, so a stratum holding none of them reports the
    /// baseline exactly — that is the stratum failing to resolve the candidate,
    /// not the candidate failing. Racing methods eliminate an arm only once it
    /// is behind, so everything undecided is carried to the authoritative pass
    /// that decides.
    pub fn survives(self) -> bool {
        !matches!(self, Self::Worse | Self::NotBuilt)
    }

    /// Label used in the journal and on stderr.
    pub fn label(self) -> &'static str {
        match self {
            Self::Better => "better",
            Self::Indistinguishable => "indistinguishable",
            Self::Worse => "worse",
            Self::NotScored => "notScored",
            Self::NotBuilt => "notBuilt",
        }
    }
}

/// One enhancement's sampled result in one screen phase (Issue #43).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenedEnhancement {
    /// Stable enhancement id.
    pub id: String,
    /// Who produced it — `harvest/…`, Forests, Ockham.
    pub producer: String,
    /// Sampled score, absent when the screen returned none for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// Signed `score - baseline`, absent when there was no score. This is the
    /// number that separates a working screen from a blind stratum.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<f64>,
    /// What the stratum said.
    pub verdict: ScreenVerdict,
    /// Whether it was carried into the next phase.
    pub kept: bool,
}

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
    /// What one screen phase actually measured (Issue #43).
    ///
    /// `kept 0 of 3` is not diagnosable: three deltas of `-3e-4` is a working
    /// screen, three deltas of exactly `0.0` is a stratum that saw nothing, and
    /// the two call for opposite responses. The stratum's own size is recorded
    /// alongside so the power of that comparison is checkable after the fact —
    /// the working directory is gone by then.
    Screen {
        /// Stride phase; successive phases see different records.
        phase: u64,
        /// Sample rate the stratum was drawn at.
        sample_rate: f64,
        /// The run's `--min-improvement`: differences below it decide nothing.
        resolution: f64,
        /// The champion's own score on this stratum.
        baseline_score: f64,
        /// Records the stratum actually contained, as the scorer reported them
        /// for the baseline.
        record_count: u64,
        /// Enhancements offered to this phase, in cohort order.
        enhancements: Vec<ScreenedEnhancement>,
        /// How many were carried forward.
        kept: usize,
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
        /// What the run decided, in one line — the same message the
        /// emitted creature's `rebase` tag carries — or why it refused.
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

    /// Issue #43: a screen phase records the numbers, not a count.
    #[test]
    fn a_screen_phase_journals_every_signed_delta_and_the_stratum_it_saw() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = Journal::new(tmp.path().join("experiments.jsonl"));
        journal
            .append(&Record::Screen {
                phase: 0,
                sample_rate: 0.05,
                resolution: 1e-9,
                baseline_score: 0.5,
                record_count: 1234,
                kept: 1,
                enhancements: vec![
                    ScreenedEnhancement {
                        id: "blind".into(),
                        producer: "harvest/best.json".into(),
                        score: Some(0.5),
                        delta: Some(0.0),
                        verdict: ScreenVerdict::Indistinguishable,
                        kept: true,
                    },
                    ScreenedEnhancement {
                        id: "loser".into(),
                        producer: "neat-ai-forests/test".into(),
                        score: Some(0.4997),
                        delta: Some(-3e-4),
                        verdict: ScreenVerdict::Worse,
                        kept: false,
                    },
                ],
            })
            .unwrap();

        let text = std::fs::read_to_string(journal.path()).unwrap();
        let value: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(value["record"], "screen");
        assert_eq!(value["recordCount"], 1234);
        assert_eq!(value["sampleRate"], 0.05);
        assert_eq!(value["baselineScore"], 0.5);
        let measured = value["enhancements"].as_array().unwrap();
        assert_eq!(measured[0]["delta"], 0.0);
        assert_eq!(
            measured[0]["verdict"], "indistinguishable",
            "a stratum that saw nothing must not read as a loss"
        );
        assert_eq!(measured[1]["verdict"], "worse");
        assert!(measured[1]["delta"].as_f64().unwrap() < 0.0);
        assert_eq!(measured[1]["producer"], "neat-ai-forests/test");
    }

    /// The classifier the journal and the screen's own decision share.
    #[test]
    fn a_screen_verdict_separates_a_seen_loss_from_a_blind_stratum() {
        assert_eq!(
            ScreenVerdict::classify(Some(0.4997), 0.5, 1e-9),
            ScreenVerdict::Worse
        );
        assert_eq!(
            ScreenVerdict::classify(Some(0.5), 0.5, 1e-9),
            ScreenVerdict::Indistinguishable
        );
        assert_eq!(
            ScreenVerdict::classify(Some(0.6), 0.5, 1e-9),
            ScreenVerdict::Better
        );
        assert_eq!(
            ScreenVerdict::classify(None, 0.5, 1e-9),
            ScreenVerdict::NotScored
        );
        assert!(!ScreenVerdict::Worse.survives());
        assert!(!ScreenVerdict::NotBuilt.survives());
        for undecided in [
            ScreenVerdict::Better,
            ScreenVerdict::Indistinguishable,
            ScreenVerdict::NotScored,
        ] {
            assert!(
                undecided.survives(),
                "{undecided:?} must be carried forward"
            );
        }
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
