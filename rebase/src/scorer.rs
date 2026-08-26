//! NEAT-AI-scorer as the authoritative rebase judge (Issue #5).
//!
//! Rebase has opinions — a patch that helped before, a prune that was proven,
//! a producer that measured a gain. None of them promote anything. The only
//! thing that promotes a candidate is a full-corpus NEAT-AI-scorer result that
//! beats the **current champion**, measured in the same call as the champion so
//! the two numbers are comparable.
//!
//! ## The contract
//!
//! * The champion is scored as the reserved stem `baseline`, in the same
//!   directory pass as every candidate. A verdict with no baseline result is
//!   not a verdict — it fails closed.
//! * Corpus identity and dimensions are checked before the scorer is spawned.
//! * The improvement must exceed a configured threshold. A tie is not a win:
//!   replacing the champion with an equal-scoring creature costs a population
//!   slot and buys nothing.
//! * Every failure mode — spawn failure, non-zero exit, malformed output, a
//!   missing entry, a non-finite number — yields no candidate. There is no
//!   "assume it was fine" path.
//!
//! ## Why a candidate proven on an ancestor can still lose
//!
//! It was proven against `A`. The champion is `B`. `B` may already contain
//! something that does the same job, may have moved the residuals the patch
//! corrects, or may simply be far enough ahead that the patch no longer pays
//! for its complexity. That is a normal, healthy outcome, and
//! [`Verdict::improved`] is `false` for it.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::engine::{BASELINE_LABEL, RebaseOutcome};

/// One scorer result. `score` is the acceptance metric — **larger is better**.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScoreResult {
    /// `1 - error - complexityPenalty - versionPenalty`.
    pub score: f64,
    /// Mean cost over scored records.
    pub error: f64,
    /// Structural penalty.
    #[serde(default)]
    pub complexity_penalty: f64,
    /// Records scored.
    #[serde(default)]
    pub record_count: u64,
    /// Sample rate when the scorer ran in sample mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<f64>,
    /// Scorer-reported backend label (`cpu-fallback`, `metal`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_backend: Option<String>,
    /// Scorer-reported cost function name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_name: Option<String>,
    /// Scorer wall time in seconds.
    #[serde(default, rename = "timeTaken")]
    pub time_taken: f64,
}

/// How the scorer is asked to run.
///
/// Only [`ScorerMode::Full`] is authoritative. Rebase always judges on the
/// full corpus; the sampled mode exists so a caller can screen cheaply before
/// paying for the real pass, and its result may never promote anything.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScorerMode {
    /// Full canonical corpus — the only authoritative mode.
    Full,
    /// Record sub-sampling — a cheap, explicitly non-authoritative screen.
    Sample {
        /// Rate in `(0, 1)`.
        rate: f64,
        /// Stride phase so successive screens see different records.
        phase: u64,
    },
}

impl ScorerMode {
    /// `true` only for [`ScorerMode::Full`].
    pub fn is_authoritative(self) -> bool {
        matches!(self, Self::Full)
    }

    /// Label used in the journal.
    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Sample { .. } => "sample",
        }
    }
}

/// Scorer failure — always fail closed.
#[derive(Debug, Clone, PartialEq)]
pub enum ScorerError {
    /// Could not spawn the binary.
    Spawn(String),
    /// Non-zero exit.
    Failed {
        /// Exit status description.
        status: String,
        /// Tail of stderr.
        stderr: String,
    },
    /// Output was not the expected JSON.
    Malformed(String),
    /// The reserved `baseline` stem is missing from the output.
    MissingBaseline,
    /// A candidate the scorer was given came back without a result.
    MissingCandidate(String),
    /// A score or error was NaN/∞.
    NonFinite(String),
    /// Writing the creature files failed.
    Io(String),
    /// The cohort's baseline is not the champion the caller believes it is.
    BaselineDrift {
        /// Checksum the cohort recorded for the champion.
        expected: String,
        /// Checksum of the creature actually written as `baseline`.
        observed: String,
    },
    /// A non-authoritative mode was offered as the final judge.
    NotAuthoritative(&'static str),
}

impl fmt::Display for ScorerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(m) => write!(f, "cannot run scorer: {m}"),
            Self::Failed { status, stderr } => write!(f, "scorer failed ({status}): {stderr}"),
            Self::Malformed(m) => write!(f, "scorer output malformed: {m}"),
            Self::MissingBaseline => write!(f, "scorer output has no `baseline` entry"),
            Self::MissingCandidate(k) => {
                write!(f, "scorer output has no entry for candidate `{k}`")
            }
            Self::NonFinite(k) => write!(f, "scorer returned a non-finite result for `{k}`"),
            Self::Io(m) => write!(f, "cannot stage creatures for scoring: {m}"),
            Self::BaselineDrift { expected, observed } => write!(
                f,
                "baseline checksum drifted: cohort recorded {expected}, staged {observed}"
            ),
            Self::NotAuthoritative(mode) => write!(
                f,
                "`{mode}` scoring is not authoritative and may not decide population re-entry"
            ),
        }
    }
}

impl std::error::Error for ScorerError {}

/// Scores a directory of creature JSON files in one corpus pass.
///
/// The trait exists so the race-condition suite can drive the whole pipeline
/// with a scripted scorer, deterministically and without a corpus.
pub trait DirectoryScorer {
    /// Score every `*.json` in `creature_dir` against `training_dir`.
    ///
    /// # Errors
    ///
    /// Any [`ScorerError`]; the caller treats all of them as fail-closed.
    fn score_directory(
        &self,
        creature_dir: &Path,
        training_dir: &Path,
        mode: ScorerMode,
    ) -> Result<BTreeMap<String, ScoreResult>, ScorerError>;

    /// Stable identity of the scorer (binary path or test label), recorded in
    /// the verdict so a surprising result can be traced to what produced it.
    fn identity(&self) -> String;
}

/// The real `rust_scorer` binary.
#[derive(Debug, Clone)]
pub struct ExternalScorer {
    /// Path (or `$PATH` name) of the scorer binary.
    pub binary: PathBuf,
    /// Extra arguments appended verbatim (e.g. `--cost MSE`, `--gpu off`).
    pub extra_args: Vec<String>,
}

impl ExternalScorer {
    /// Scorer at `binary` with no extra arguments.
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            extra_args: Vec::new(),
        }
    }

    /// Scorer at `binary` with `extra_args` appended to every invocation.
    pub fn with_args(binary: impl Into<PathBuf>, extra_args: Vec<String>) -> Self {
        Self {
            binary: binary.into(),
            extra_args,
        }
    }
}

/// Parse scorer stdout; every entry must be finite and `baseline` must exist.
///
/// # Errors
///
/// [`ScorerError::Malformed`], [`ScorerError::MissingBaseline`] or
/// [`ScorerError::NonFinite`].
pub fn parse_scorer_output(stdout: &str) -> Result<BTreeMap<String, ScoreResult>, ScorerError> {
    let parsed: BTreeMap<String, ScoreResult> =
        serde_json::from_str(stdout.trim()).map_err(|e| ScorerError::Malformed(e.to_string()))?;
    if !parsed.contains_key(BASELINE_LABEL) {
        return Err(ScorerError::MissingBaseline);
    }
    check_finite(&parsed)?;
    Ok(parsed)
}

/// Reject any non-finite score or error.
///
/// JSON has no `NaN` or `Infinity` literal, so a scorer cannot normally hand
/// one over — serde refuses an out-of-range number first. This is the gate for
/// the cases that get past that: an in-band overflow inside the scorer, or a
/// future transport that does carry them. A number Rebase cannot compare is
/// never treated as a score.
///
/// # Errors
///
/// [`ScorerError::NonFinite`] naming the first offending stem.
pub fn check_finite(results: &BTreeMap<String, ScoreResult>) -> Result<(), ScorerError> {
    for (k, v) in results {
        if !v.score.is_finite() || !v.error.is_finite() {
            return Err(ScorerError::NonFinite(k.clone()));
        }
    }
    Ok(())
}

impl DirectoryScorer for ExternalScorer {
    fn score_directory(
        &self,
        creature_dir: &Path,
        training_dir: &Path,
        mode: ScorerMode,
    ) -> Result<BTreeMap<String, ScoreResult>, ScorerError> {
        let mut cmd = Command::new(&self.binary);
        if let ScorerMode::Sample { rate, phase } = mode {
            cmd.arg("--sample-rate")
                .arg(format!("{rate}"))
                .arg("--sample-phase")
                .arg(phase.to_string());
        }
        cmd.args(&self.extra_args);
        cmd.arg(creature_dir).arg(training_dir);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let out = cmd
            .output()
            .map_err(|e| ScorerError::Spawn(format!("{}: {e}", self.binary.display())))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let tail: String = stderr
                .chars()
                .rev()
                .take(2000)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            return Err(ScorerError::Failed {
                status: out.status.to_string(),
                stderr: tail.trim().to_string(),
            });
        }
        parse_scorer_output(&String::from_utf8_lossy(&out.stdout))
    }

    fn identity(&self) -> String {
        self.binary.display().to_string()
    }
}

/// One scored member of the cohort.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoredCandidate {
    /// Cohort label / scorer file stem.
    pub label: String,
    /// Checksum of the creature scored.
    pub checksum: String,
    /// Enhancement ids this candidate applied.
    pub applied_ids: Vec<String>,
    /// The authoritative result.
    pub result: ScoreResult,
    /// `score - baseline.score`.
    pub delta: f64,
}

/// The authoritative decision.
///
/// The baseline is always represented explicitly, whatever the outcome: a
/// verdict a human cannot check against the champion's own number is not
/// worth reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    /// Checksum of the champion that was judged against.
    pub champion_checksum: String,
    /// The champion's own authoritative result.
    pub baseline: ScoreResult,
    /// Every candidate scored, best first.
    pub candidates: Vec<ScoredCandidate>,
    /// The producer's own descendant, when one was supplied. Scored in the
    /// same call as everything else and deliberately absent from `candidates`:
    /// it is evidence about what publishing it would have cost, never
    /// something this run may promote.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<ScoredCandidate>,
    /// The winner, when one beat the champion by more than the threshold.
    pub winner: Option<ScoredCandidate>,
    /// Minimum improvement required to declare a winner.
    pub min_improvement: f64,
    /// Scoring mode used.
    pub mode: String,
    /// Stable identity of the scorer that produced the numbers.
    pub scorer_identity: String,
    /// Backend the scorer reported for the baseline, when it reported one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scorer_backend: Option<String>,
}

impl Verdict {
    /// `true` when a candidate beat the champion by more than the threshold.
    pub fn improved(&self) -> bool {
        self.winner.is_some()
    }
}

/// Score the champion, the whole cohort and the producer's reference creature
/// in one authoritative pass, and return the verdict.
///
/// `staging_dir` is created if needed and filled with one JSON file per cohort
/// member plus `outcome.reference`, named by its label. Nothing outside it is
/// written, and neither the champion nor the enhancement files are touched.
///
/// One call, so every number in the verdict comes from the same scorer, the
/// same corpus and the same backend — including the reference, which is what
/// makes "publishing my own descendant would have cost X" a measurement rather
/// than an assertion.
///
/// # Errors
///
/// Any [`ScorerError`]. Every one of them means no winner: a scorer that
/// cannot be trusted cannot promote anything.
pub fn judge(
    scorer: &dyn DirectoryScorer,
    outcome: &RebaseOutcome,
    training_dir: &Path,
    staging_dir: &Path,
    min_improvement: f64,
    mode: ScorerMode,
) -> Result<Verdict, ScorerError> {
    if !mode.is_authoritative() {
        return Err(ScorerError::NotAuthoritative(mode.label()));
    }
    std::fs::create_dir_all(staging_dir)
        .map_err(|e| ScorerError::Io(format!("{}: {e}", staging_dir.display())))?;
    stage(outcome, staging_dir)?;

    let results = scorer.score_directory(staging_dir, training_dir, mode)?;
    let baseline = results
        .get(BASELINE_LABEL)
        .ok_or(ScorerError::MissingBaseline)?
        .clone();

    let mut candidates = Vec::new();
    for candidate in outcome.candidates() {
        let result = results
            .get(&candidate.label)
            .ok_or_else(|| ScorerError::MissingCandidate(candidate.label.clone()))?
            .clone();
        let delta = result.score - baseline.score;
        candidates.push(ScoredCandidate {
            label: candidate.label.clone(),
            checksum: candidate.checksum.clone(),
            applied_ids: candidate.applied_ids.clone(),
            result,
            delta,
        });
    }
    // Best first; ties broken by label so the ordering is reproducible.
    candidates.sort_by(|a, b| {
        b.delta
            .partial_cmp(&a.delta)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.label.cmp(&b.label))
    });
    let winner = candidates
        .first()
        .filter(|c| c.delta > min_improvement)
        .cloned();

    let reference = match &outcome.reference {
        Some(reference) => {
            let result = results
                .get(&reference.label)
                .ok_or_else(|| ScorerError::MissingCandidate(reference.label.clone()))?
                .clone();
            let delta = result.score - baseline.score;
            Some(ScoredCandidate {
                label: reference.label.clone(),
                checksum: reference.checksum.clone(),
                applied_ids: reference.applied_ids.clone(),
                result,
                delta,
            })
        }
        None => None,
    };

    Ok(Verdict {
        champion_checksum: outcome.champion_checksum.clone(),
        scorer_backend: baseline.gpu_backend.clone(),
        baseline,
        candidates,
        reference,
        winner,
        min_improvement,
        mode: mode.label().to_string(),
        scorer_identity: scorer.identity(),
    })
}

/// Write every cohort member into `dir` as `<label>.json`, re-checking the
/// baseline's checksum on the way out.
fn stage(outcome: &RebaseOutcome, dir: &Path) -> Result<(), ScorerError> {
    for candidate in outcome.cohort.iter().chain(outcome.reference.iter()) {
        let json = neat_core::creature_to_json(&candidate.creature)
            .map_err(|e| ScorerError::Io(e.to_string()))?;
        // The champion is what everything is measured against; if what lands
        // on disk is not the creature the cohort was built from, no number
        // produced from it means anything.
        if candidate.is_baseline() {
            let observed = crate::creature::sha256_hex(json.as_bytes());
            if observed != outcome.champion_checksum {
                return Err(ScorerError::BaselineDrift {
                    expected: outcome.champion_checksum.clone(),
                    observed,
                });
            }
        }
        let path = dir.join(format!("{}.json", candidate.label));
        std::fs::write(&path, json)
            .map_err(|e| ScorerError::Io(format!("{}: {e}", path.display())))?;
    }
    Ok(())
}

/// A scripted scorer for tests: every stem's score is looked up by name.
///
/// Public so the race-condition suite in `tests/` can drive the real pipeline
/// end to end without a corpus or a binary.
#[derive(Debug, Clone, Default)]
pub struct ScriptedScorer {
    /// Score per file stem. A stem with no entry gets [`Self::fallback`].
    pub stem_scores: BTreeMap<String, f64>,
    /// Score for any stem not named in [`Self::stem_scores`].
    pub fallback: f64,
    /// When set, every call fails with this error instead of scoring.
    pub failure: Option<ScorerError>,
    /// Stems to omit from the output, to exercise the missing-entry paths.
    pub omit: Vec<String>,
}

impl ScriptedScorer {
    /// A scorer that returns `fallback` for every stem.
    pub fn flat(fallback: f64) -> Self {
        Self {
            fallback,
            ..Self::default()
        }
    }

    /// Set one stem's score.
    #[must_use]
    pub fn with(mut self, stem: &str, score: f64) -> Self {
        self.stem_scores.insert(stem.to_string(), score);
        self
    }

    /// Make every call fail.
    #[must_use]
    pub fn failing(mut self, error: ScorerError) -> Self {
        self.failure = Some(error);
        self
    }

    /// Omit `stem` from the output.
    #[must_use]
    pub fn omitting(mut self, stem: &str) -> Self {
        self.omit.push(stem.to_string());
        self
    }
}

impl DirectoryScorer for ScriptedScorer {
    fn score_directory(
        &self,
        creature_dir: &Path,
        _training_dir: &Path,
        _mode: ScorerMode,
    ) -> Result<BTreeMap<String, ScoreResult>, ScorerError> {
        if let Some(err) = &self.failure {
            return Err(err.clone());
        }
        let mut out = BTreeMap::new();
        let entries = std::fs::read_dir(creature_dir)
            .map_err(|e| ScorerError::Io(format!("{}: {e}", creature_dir.display())))?;
        for entry in entries {
            let entry = entry.map_err(|e| ScorerError::Io(e.to_string()))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if self.omit.iter().any(|o| o == stem) {
                continue;
            }
            let score = self.stem_scores.get(stem).copied().unwrap_or(self.fallback);
            out.insert(
                stem.to_string(),
                ScoreResult {
                    score,
                    error: 1.0 - score,
                    complexity_penalty: 0.0,
                    record_count: 1000,
                    sample_rate: None,
                    gpu_backend: Some("scripted".into()),
                    cost_name: Some("MSE".into()),
                    time_taken: 0.0,
                },
            );
        }
        Ok(out)
    }

    fn identity(&self) -> String {
        "scripted-scorer".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{RebaseRequest, rebase};
    use crate::enhancement::{Enhancement, Payload, ProducerContext};
    use crate::fixtures::linear_hidden_creature;
    use crate::patch::{Node, Patch, Provenance};

    const CORPUS: &str = "corpus-1";

    fn forest(feature: usize, right: f32) -> Enhancement {
        Enhancement::new(
            Payload::ForestPatch {
                patch: Patch::new(
                    0,
                    Node::stump(feature, 0.5, 0.0, right),
                    Provenance::default(),
                ),
            },
            &ProducerContext {
                producer: "neat-ai-forests/test".into(),
                base_checksum: "base".into(),
                base_score: 0.5,
                improved_score: 0.6,
                corpus_identity: CORPUS.into(),
                input_count: 2,
                output_count: 1,
            },
        )
    }

    fn cohort_of(enhancements: &[Enhancement]) -> RebaseOutcome {
        let champion = linear_hidden_creature(2.0);
        rebase(&RebaseRequest {
            champion: &champion,
            enhancements,
            corpus_identity: CORPUS,
            max_candidates: 0,
        })
        .unwrap()
    }

    fn verdict(
        scorer: &dyn DirectoryScorer,
        outcome: &RebaseOutcome,
    ) -> Result<Verdict, ScorerError> {
        let tmp = tempfile::tempdir().unwrap();
        judge(
            scorer,
            outcome,
            tmp.path(),
            &tmp.path().join("staging"),
            1e-9,
            ScorerMode::Full,
        )
    }

    #[test]
    fn a_better_candidate_wins_and_the_baseline_is_always_recorded() {
        let outcome = cohort_of(&[forest(1, 0.25)]);
        let scorer = ScriptedScorer::flat(0.50).with("single-00", 0.60);
        let v = verdict(&scorer, &outcome).unwrap();
        assert!(v.improved());
        let winner = v.winner.unwrap();
        assert_eq!(winner.label, "single-00");
        assert!((winner.delta - 0.10).abs() < 1e-12);
        assert!((v.baseline.score - 0.50).abs() < 1e-12);
        assert_eq!(v.scorer_identity, "scripted-scorer");
        assert_eq!(v.scorer_backend.as_deref(), Some("scripted"));
    }

    #[test]
    fn a_tie_is_not_an_improvement() {
        let outcome = cohort_of(&[forest(1, 0.25)]);
        let scorer = ScriptedScorer::flat(0.50);
        let v = verdict(&scorer, &outcome).unwrap();
        assert!(!v.improved());
        // The baseline is still there, and so is the candidate's own number.
        assert_eq!(v.candidates.len(), 1);
        assert!((v.candidates[0].delta).abs() < 1e-12);
    }

    #[test]
    fn a_candidate_proven_on_an_ancestor_can_still_lose_on_the_new_champion() {
        // The producer measured +0.1 on its own opening creature. On this
        // champion the same change scores worse — and loses.
        let outcome = cohort_of(&[forest(1, 0.25)]);
        assert!(outcome.reports[0].claimed_gain > 0.0);
        let scorer = ScriptedScorer::flat(0.80).with("single-00", 0.79);
        let v = verdict(&scorer, &outcome).unwrap();
        assert!(!v.improved());
        assert!(v.candidates[0].delta < 0.0);
    }

    #[test]
    fn an_improvement_below_the_threshold_is_refused() {
        let outcome = cohort_of(&[forest(1, 0.25)]);
        let scorer = ScriptedScorer::flat(0.50).with("single-00", 0.5000001);
        let tmp = tempfile::tempdir().unwrap();
        let v = judge(
            &scorer,
            &outcome,
            tmp.path(),
            &tmp.path().join("staging"),
            1e-3,
            ScorerMode::Full,
        )
        .unwrap();
        assert!(!v.improved(), "a gain under the threshold is not a win");
    }

    #[test]
    fn a_scorer_failure_never_produces_a_winner() {
        let outcome = cohort_of(&[forest(1, 0.25)]);
        let scorer = ScriptedScorer::flat(0.5).failing(ScorerError::Failed {
            status: "exit status: 1".into(),
            stderr: "corpus unreadable".into(),
        });
        let err = verdict(&scorer, &outcome).unwrap_err();
        assert!(matches!(err, ScorerError::Failed { .. }), "{err}");
    }

    #[test]
    fn a_missing_baseline_result_fails_closed() {
        let outcome = cohort_of(&[forest(1, 0.25)]);
        let scorer = ScriptedScorer::flat(0.5).omitting("baseline");
        assert_eq!(
            verdict(&scorer, &outcome).unwrap_err(),
            ScorerError::MissingBaseline
        );
    }

    #[test]
    fn a_missing_candidate_result_fails_closed() {
        let outcome = cohort_of(&[forest(1, 0.25)]);
        let scorer = ScriptedScorer::flat(0.5).omitting("single-00");
        assert_eq!(
            verdict(&scorer, &outcome).unwrap_err(),
            ScorerError::MissingCandidate("single-00".into())
        );
    }

    #[test]
    fn baseline_drift_fails_closed() {
        let mut outcome = cohort_of(&[forest(1, 0.25)]);
        outcome.champion_checksum = "not-the-champion".into();
        let scorer = ScriptedScorer::flat(0.5);
        assert!(matches!(
            verdict(&scorer, &outcome).unwrap_err(),
            ScorerError::BaselineDrift { .. }
        ));
    }

    #[test]
    fn a_sampled_screen_may_not_decide() {
        let outcome = cohort_of(&[forest(1, 0.25)]);
        let tmp = tempfile::tempdir().unwrap();
        let err = judge(
            &ScriptedScorer::flat(0.5),
            &outcome,
            tmp.path(),
            &tmp.path().join("staging"),
            0.0,
            ScorerMode::Sample {
                rate: 0.05,
                phase: 0,
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, ScorerError::NotAuthoritative("sample")),
            "{err}"
        );
    }

    #[test]
    fn non_finite_and_malformed_output_fail_closed() {
        assert!(matches!(
            parse_scorer_output("not json").unwrap_err(),
            ScorerError::Malformed(_)
        ));
        assert_eq!(
            parse_scorer_output(r#"{"single-00":{"score":0.1,"error":0.9}}"#).unwrap_err(),
            ScorerError::MissingBaseline
        );
        assert!(matches!(
            parse_scorer_output(r#"{"baseline":{"score":null,"error":0.9}}"#).unwrap_err(),
            ScorerError::Malformed(_)
        ));
        // JSON carries no NaN literal — serde refuses an out-of-range number
        // before the finite gate ever sees it.
        assert!(matches!(
            parse_scorer_output(r#"{"baseline":{"score":1e999,"error":0.9}}"#).unwrap_err(),
            ScorerError::Malformed(_)
        ));
        // The finite gate itself, for anything that does get that far.
        let mut results = BTreeMap::new();
        results.insert(
            "baseline".to_string(),
            ScoreResult {
                score: f64::NAN,
                error: 0.5,
                complexity_penalty: 0.0,
                record_count: 1,
                sample_rate: None,
                gpu_backend: None,
                cost_name: None,
                time_taken: 0.0,
            },
        );
        assert_eq!(
            check_finite(&results).unwrap_err(),
            ScorerError::NonFinite("baseline".into())
        );
    }

    #[test]
    fn candidates_come_back_best_first() {
        let outcome = cohort_of(&[forest(0, 0.25), forest(1, -0.1)]);
        let scorer = ScriptedScorer::flat(0.50)
            .with("bundle", 0.70)
            .with("single-00", 0.55)
            .with("single-01", 0.60);
        let v = verdict(&scorer, &outcome).unwrap();
        let labels: Vec<&str> = v.candidates.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["bundle", "single-01", "single-00"]);
        assert_eq!(v.winner.unwrap().label, "bundle");
    }

    #[test]
    fn the_scorer_sees_one_file_per_cohort_member() {
        let outcome = cohort_of(&[forest(0, 0.25), forest(1, -0.1)]);
        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");
        judge(
            &ScriptedScorer::flat(0.5),
            &outcome,
            tmp.path(),
            &staging,
            1e-9,
            ScorerMode::Full,
        )
        .unwrap();
        let files = std::fs::read_dir(&staging).unwrap().count();
        assert_eq!(files, outcome.cohort.len());
        assert!(staging.join("baseline.json").exists());
    }
}
