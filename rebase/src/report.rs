//! `neat_ai_rebase report` — read the run journals back (Issue #38).
//!
//! Every rebase writes `experiments.jsonl`, and until now nothing read it. The
//! questions an unattended soak has to answer are not "how many runs" but
//! *which kind* of non-win each run was: a fleet that had already absorbed the
//! work (`nothingToDo`) says the opposite of a corpus that rejected the
//! candidate (`noImprovement`), and collapsing the two is the mistake this
//! reporter exists to prevent.
//!
//! Three rules shape the reader:
//!
//! * **A partial last line is normal.** A run killed mid-write leaves one, and
//!   it must not make the journal unreadable. It is counted and shown, never
//!   silently dropped.
//! * **Absent is not zero.** Records are read leniently, field by field: a
//!   field an older record does not carry reads as absent and is left out of
//!   the numbers rather than counted as `0`.
//! * **Runs are segmented, not merged.** The journal is append-only and a
//!   directory can be reused, so several runs share one file. A `result`
//!   record closes a run, and an `opening` record starts one — a run with no
//!   `result` is reported as exactly that.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::journal::{SCREEN_PHASE_LABEL_PREFIX, SCREEN_RECORD};

/// Result statuses a run can record, always listed even at zero.
///
/// Listing them is the point: the counts are only useful when
/// `nothingToDo`, `noImprovement`, `incompatible` and `failed` are told apart.
pub const KNOWN_STATUSES: [&str; 6] = [
    "improved",
    "noImprovement",
    "nothingToDo",
    "incompatible",
    "dryRun",
    "failed",
];

/// One journal line, read leniently.
///
/// Every field is optional on purpose. The strict [`crate::journal::Record`]
/// would refuse a record written by an older Rebase that lacks a field added
/// since, and a reporter that drops those records reports the wrong numbers.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawLine {
    /// Record discriminant: `opening` / `enhancement` / … .
    record: Option<String>,
    /// `enhancement`: `applied` / `alreadyPresent` / `incompatible`.
    outcome: Option<String>,
    /// `enhancement`: why it was incompatible.
    reason: Option<String>,
    /// `dropped`: what was dropped.
    label: Option<String>,
    /// `verdict`: every candidate scored, with its delta over the champion.
    candidates: Option<Vec<RawCandidate>>,
    /// `verdict`: the winner, when one beat the champion.
    winner: Option<serde_json::Value>,
    /// `result`: the run's final status.
    status: Option<String>,
}

/// One scored candidate inside a `verdict` record.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawCandidate {
    /// `score - baseline.score`; absent in a record that never carried it.
    delta: Option<f64>,
}

/// What a run's verdict record said.
#[derive(Debug, Clone, Copy, PartialEq)]
struct VerdictFacts {
    /// Best candidate's delta over the champion, when any candidate carried
    /// one.
    best_delta: Option<f64>,
    /// Whether the authoritative pass promoted anything.
    won: bool,
}

/// How the cheap screen and the authoritative pass agreed.
///
/// A screen that never disagrees with the corpus is not earning its keep; one
/// that disagrees constantly is miscalibrated. Both readings need the three
/// counts kept apart.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScreenSummary {
    /// Runs in which the screen ran at all.
    pub screened_runs: usize,
    /// The screen left nothing for the authoritative pass to score.
    pub kept_nothing: usize,
    /// The authoritative pass promoted what the screen kept.
    pub confirmed: usize,
    /// The authoritative pass rejected what the screen kept.
    pub rejected: usize,
    /// The screen ran but the run recorded neither a verdict nor a `nothingToDo`
    /// result — killed mid-run, or failed before scoring.
    pub undecided: usize,
}

/// The aggregate over one or more journals.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    /// Journal files read.
    pub journals: usize,
    /// Non-empty lines seen.
    pub lines: usize,
    /// Lines that were not a readable record — a run killed mid-write leaves
    /// one, and it is shown rather than hidden.
    pub unreadable_lines: usize,
    /// Runs found across every journal.
    pub runs: usize,
    /// Runs by the status their `result` record recorded.
    pub runs_by_status: BTreeMap<String, usize>,
    /// Runs that recorded no `result` at all.
    pub runs_without_result: usize,
    /// Enhancements by fate: `applied` / `alreadyPresent` / `incompatible`.
    pub enhancements_by_outcome: BTreeMap<String, usize>,
    /// Reasons given for the `incompatible` ones.
    pub incompatible_reasons: BTreeMap<String, usize>,
    /// Runs whose authoritative verdict was recorded.
    pub runs_scored: usize,
    /// Of those, the ones that promoted a candidate.
    pub runs_with_a_winner: usize,
    /// Best candidate's delta over the champion, one per scored run that
    /// recorded one. Sorted ascending.
    pub best_vs_champion: Vec<f64>,
    /// Screen against the authoritative pass.
    pub screen: ScreenSummary,
}

impl Report {
    /// Read every journal in `paths` and aggregate them.
    ///
    /// # Errors
    ///
    /// The first file that cannot be read, named. An unreadable *file* is an
    /// operational failure and is reported loudly; an unreadable *line* is
    /// not, and is counted instead.
    pub fn read(paths: &[PathBuf]) -> Result<Self, String> {
        let mut report = Self::default();
        for path in paths {
            let text =
                std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            report.absorb(&text);
        }
        report
            .best_vs_champion
            .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        Ok(report)
    }

    /// Absorb the text of one journal.
    ///
    /// Runs never span files, so whatever is still open when the text ends is
    /// closed as a run that recorded no result.
    fn absorb(&mut self, text: &str) {
        self.journals += 1;
        let mut run = RunState::default();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            self.lines += 1;
            let Ok(raw) = serde_json::from_str::<RawLine>(line) else {
                // A truncated last line lands here, and so does anything else
                // that is not a record. Counted, and shown in the table.
                self.unreadable_lines += 1;
                continue;
            };
            let Some(kind) = raw.record.as_deref() else {
                self.unreadable_lines += 1;
                continue;
            };
            match kind {
                "opening" => {
                    // A second opening means the previous run never wrote its
                    // result.
                    self.close(std::mem::take(&mut run));
                    run.started = true;
                }
                "enhancement" => {
                    run.started = true;
                    if let Some(outcome) = raw.outcome {
                        if outcome == "incompatible"
                            && let Some(reason) = raw.reason
                        {
                            *self.incompatible_reasons.entry(reason).or_default() += 1;
                        }
                        *self.enhancements_by_outcome.entry(outcome).or_default() += 1;
                    }
                }
                "dropped" => {
                    run.started = true;
                    // How a screen phase was recorded before Issue #43 gave it
                    // a record of its own. Journals in this shape are still on
                    // disk, and a soak reading them back is still a soak that
                    // screened.
                    if raw
                        .label
                        .as_deref()
                        .is_some_and(|l| l.starts_with(SCREEN_PHASE_LABEL_PREFIX))
                    {
                        run.screened = true;
                    }
                }
                SCREEN_RECORD => {
                    run.started = true;
                    run.screened = true;
                }
                "verdict" => {
                    run.started = true;
                    run.verdict = Some(VerdictFacts {
                        best_delta: raw
                            .candidates
                            .unwrap_or_default()
                            .into_iter()
                            .filter_map(|c| c.delta)
                            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)),
                        won: raw.winner.is_some_and(|w| !w.is_null()),
                    });
                }
                "result" => {
                    run.started = true;
                    run.status = raw.status;
                    self.close(std::mem::take(&mut run));
                }
                _ => run.started = true,
            }
        }
        self.close(run);
    }

    /// Fold one finished run into the totals.
    fn close(&mut self, run: RunState) {
        if !run.started {
            return;
        }
        self.runs += 1;
        match &run.status {
            Some(status) => *self.runs_by_status.entry(status.clone()).or_default() += 1,
            None => self.runs_without_result += 1,
        }
        if let Some(verdict) = run.verdict {
            self.runs_scored += 1;
            if verdict.won {
                self.runs_with_a_winner += 1;
            }
            if let Some(delta) = verdict.best_delta {
                self.best_vs_champion.push(delta);
            }
        }
        if run.screened {
            self.screen.screened_runs += 1;
            match (run.verdict, run.status.as_deref()) {
                (Some(v), _) if v.won => self.screen.confirmed += 1,
                (Some(_), _) => self.screen.rejected += 1,
                (None, Some("nothingToDo")) => self.screen.kept_nothing += 1,
                (None, _) => self.screen.undecided += 1,
            }
        }
    }

    /// Smallest recorded best-candidate delta, when any run recorded one.
    pub fn min_best_vs_champion(&self) -> Option<f64> {
        self.best_vs_champion.first().copied()
    }

    /// Largest recorded best-candidate delta, when any run recorded one.
    pub fn max_best_vs_champion(&self) -> Option<f64> {
        self.best_vs_champion.last().copied()
    }

    /// Median recorded best-candidate delta, when any run recorded one.
    ///
    /// An even count averages the two middle values — the usual convention, and
    /// the one the median of a soak is quoted under.
    pub fn median_best_vs_champion(&self) -> Option<f64> {
        let n = self.best_vs_champion.len();
        match n {
            0 => None,
            _ if n % 2 == 1 => Some(self.best_vs_champion[n / 2]),
            _ => Some((self.best_vs_champion[n / 2 - 1] + self.best_vs_champion[n / 2]) / 2.0),
        }
    }
}

/// Where a run is up to while its journal is being read.
#[derive(Debug, Default)]
struct RunState {
    /// Any record at all has been seen for this run.
    started: bool,
    /// A screen phase was recorded.
    screened: bool,
    /// The authoritative verdict, when one was recorded.
    verdict: Option<VerdictFacts>,
    /// The status the `result` record recorded.
    status: Option<String>,
}

/// Render `report` as the table the command prints.
pub fn render(report: &Report) -> String {
    let mut out = String::new();
    out.push_str("NEAT-AI-Rebase journal report\n");
    section(&mut out, "Journals");
    row(&mut out, "files read", report.journals);
    row(
        &mut out,
        "records read",
        report.lines - report.unreadable_lines,
    );
    row(&mut out, "unreadable lines", report.unreadable_lines);
    row(&mut out, "runs", report.runs);

    section(&mut out, "Runs by outcome");
    let mut statuses: Vec<String> = KNOWN_STATUSES.iter().map(|s| (*s).to_string()).collect();
    for status in report.runs_by_status.keys() {
        if !statuses.iter().any(|s| s == status) {
            statuses.push(status.clone());
        }
    }
    for status in &statuses {
        row(
            &mut out,
            status,
            report.runs_by_status.get(status).copied().unwrap_or(0),
        );
    }
    row(&mut out, "no result recorded", report.runs_without_result);

    section(&mut out, "Enhancements");
    if report.enhancements_by_outcome.is_empty() {
        out.push_str("  no enhancement records\n");
    } else {
        for (outcome, count) in &report.enhancements_by_outcome {
            row(&mut out, outcome, *count);
        }
    }
    if !report.incompatible_reasons.is_empty() {
        section(&mut out, "Incompatible because");
        for (reason, count) in &report.incompatible_reasons {
            row(&mut out, reason, *count);
        }
    }

    section(&mut out, "Best candidate vs champion");
    row(&mut out, "runs scored", report.runs_scored);
    row(&mut out, "runs with a winner", report.runs_with_a_winner);
    if report.best_vs_champion.is_empty() {
        // Absent, not zero: no run recorded a delta, and inventing one here
        // would be the whole failure this reporter exists to avoid.
        out.push_str("  no delta recorded\n");
    } else {
        delta_row(&mut out, "minimum", report.min_best_vs_champion());
        delta_row(&mut out, "median", report.median_best_vs_champion());
        delta_row(&mut out, "maximum", report.max_best_vs_champion());
    }

    section(&mut out, "Screen vs the authoritative pass");
    row(&mut out, "runs screened", report.screen.screened_runs);
    row(&mut out, "screen kept nothing", report.screen.kept_nothing);
    row(&mut out, "full pass confirmed", report.screen.confirmed);
    row(&mut out, "full pass rejected", report.screen.rejected);
    row(&mut out, "outcome not recorded", report.screen.undecided);
    out
}

/// Width the label column is padded to.
const LABEL_WIDTH: usize = 34;

fn section(out: &mut String, title: &str) {
    out.push('\n');
    out.push_str(title);
    out.push('\n');
}

fn row(out: &mut String, label: &str, value: usize) {
    out.push_str(&format!("  {label:<LABEL_WIDTH$}{value:>8}\n"));
}

fn delta_row(out: &mut String, label: &str, value: Option<f64>) {
    match value {
        Some(v) => out.push_str(&format!("  {label:<LABEL_WIDTH$}{v:>+15.3e}\n")),
        None => out.push_str(&format!("  {label:<LABEL_WIDTH$}{:>15}\n", "absent")),
    }
}

/// Read `paths` and return the rendered table.
///
/// # Errors
///
/// The first journal that cannot be read.
pub fn report(paths: &[PathBuf]) -> Result<String, String> {
    if paths.is_empty() {
        return Err("no journal given: neat_ai_rebase report <experiments.jsonl>...".to_string());
    }
    Ok(render(&Report::read(paths)?))
}

/// Read one journal, for a caller that has a single path.
///
/// # Errors
///
/// The journal that cannot be read.
pub fn read_one(path: &Path) -> Result<Report, String> {
    Report::read(std::slice::from_ref(&path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A journal for one run, assembled from the record shapes the CLI writes.
    fn run_journal(status: &str, extra: &[&str]) -> String {
        let mut lines = vec![
            r#"{"record":"opening","producer":"p","openingChecksum":"a","championChecksum":"b","corpusIdentity":"c","enhancementCount":1}"#.to_string(),
        ];
        lines.extend(extra.iter().map(|s| (*s).to_string()));
        lines.push(format!(r#"{{"record":"result","status":"{status}"}}"#));
        lines.join("\n") + "\n"
    }

    fn verdict_line(best_delta: f64, won: bool) -> String {
        let winner = if won {
            format!(
                r#","winner":{{"label":"single-00","checksum":"x","appliedIds":[],"result":{{"score":0.6,"error":0.4}},"delta":{best_delta}}}"#
            )
        } else {
            String::new()
        };
        format!(
            r#"{{"record":"verdict","championChecksum":"b","baseline":{{"score":0.5,"error":0.5}},"candidates":[{{"label":"single-00","checksum":"x","appliedIds":[],"result":{{"score":0.6,"error":0.4}},"delta":{best_delta}}}],"minImprovement":1e-9,"mode":"full","scorerIdentity":"scripted"{winner}}}"#
        )
    }

    /// A screen phase as journals written before Issue #43 recorded it. Still
    /// read back, so the runs those journals hold keep counting as screened.
    fn screen_line(kept: usize, of: usize) -> String {
        format!(
            r#"{{"record":"dropped","label":"{SCREEN_PHASE_LABEL_PREFIX}0","reason":"{kept} of {of} enhancements beat the champion alone"}}"#
        )
    }

    /// A screen phase in the shape Issue #43 introduced.
    fn screen_record_line(kept: usize) -> String {
        serde_json::to_string(&crate::journal::Record::Screen {
            phase: 0,
            sample_rate: 0.05,
            resolution: 1e-9,
            baseline_score: 0.5,
            record_count: 1000,
            enhancements: Vec::new(),
            kept,
        })
        .unwrap()
    }

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn the_four_non_win_outcomes_are_never_collapsed() {
        let tmp = tempfile::tempdir().unwrap();
        let text = [
            run_journal("nothingToDo", &[]),
            run_journal("noImprovement", &[]),
            run_journal("incompatible", &[]),
            run_journal("failed", &[]),
            run_journal("nothingToDo", &[]),
        ]
        .concat();
        let path = write(tmp.path(), "experiments.jsonl", &text);

        let report = read_one(&path).unwrap();
        assert_eq!(report.runs, 5);
        assert_eq!(report.runs_by_status.get("nothingToDo"), Some(&2));
        assert_eq!(report.runs_by_status.get("noImprovement"), Some(&1));
        assert_eq!(report.runs_by_status.get("incompatible"), Some(&1));
        assert_eq!(report.runs_by_status.get("failed"), Some(&1));
        assert_eq!(report.runs_without_result, 0);

        let table = render(&report);
        for status in KNOWN_STATUSES {
            assert!(table.contains(status), "{status} missing from:\n{table}");
        }
    }

    #[test]
    fn a_partial_last_line_is_counted_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        // A run killed mid-write: the final record is truncated.
        let text = format!(
            "{}{}",
            run_journal("improved", &[&verdict_line(0.02, true)]),
            r#"{"record":"result","stat"#
        );
        let path = write(tmp.path(), "experiments.jsonl", &text);

        let report = read_one(&path).unwrap();
        assert_eq!(report.unreadable_lines, 1);
        assert_eq!(report.runs, 1, "the completed run is still readable");
        assert_eq!(report.runs_by_status.get("improved"), Some(&1));
        assert!(render(&report).contains("unreadable lines"));
    }

    #[test]
    fn a_run_that_never_wrote_its_result_is_reported_as_such() {
        let tmp = tempfile::tempdir().unwrap();
        // Killed after the opening; then the directory was reused and a second
        // run ran to completion.
        let text = format!(
            "{}{}",
            r#"{"record":"opening","producer":"p","openingChecksum":"a","championChecksum":"b","corpusIdentity":"c","enhancementCount":1}
"#,
            run_journal("improved", &[&verdict_line(0.01, true)])
        );
        let path = write(tmp.path(), "experiments.jsonl", &text);

        let report = read_one(&path).unwrap();
        assert_eq!(report.runs, 2);
        assert_eq!(report.runs_without_result, 1);
        assert_eq!(report.runs_by_status.get("improved"), Some(&1));
    }

    #[test]
    fn an_absent_field_reads_as_absent_rather_than_zero() {
        let tmp = tempfile::tempdir().unwrap();
        // An older record with no `claimedGain`, and a verdict whose candidate
        // carries no `delta`. Neither may become a 0 in the numbers.
        let text = run_journal(
            "noImprovement",
            &[
                r#"{"record":"enhancement","id":"e1","kind":"forestPatch","producer":"p","outcome":"applied"}"#,
                r#"{"record":"verdict","championChecksum":"b","baseline":{"score":0.5,"error":0.5},"candidates":[{"label":"single-00","checksum":"x","appliedIds":[]}],"minImprovement":1e-9,"mode":"full","scorerIdentity":"s"}"#,
            ],
        );
        let path = write(tmp.path(), "experiments.jsonl", &text);

        let report = read_one(&path).unwrap();
        assert_eq!(report.unreadable_lines, 0, "a lenient read keeps them");
        assert_eq!(report.enhancements_by_outcome.get("applied"), Some(&1));
        assert_eq!(report.runs_scored, 1);
        assert!(
            report.best_vs_champion.is_empty(),
            "a missing delta is absent, not 0.0"
        );
        assert_eq!(report.median_best_vs_champion(), None);
        assert!(render(&report).contains("no delta recorded"));
    }

    #[test]
    fn enhancement_fates_and_incompatible_reasons_are_counted_separately() {
        let tmp = tempfile::tempdir().unwrap();
        let text = run_journal(
            "nothingToDo",
            &[
                r#"{"record":"enhancement","id":"e1","kind":"forestPatch","producer":"p","outcome":"alreadyPresent","claimedGain":0.1}"#,
                r#"{"record":"enhancement","id":"e2","kind":"forestPatch","producer":"p","outcome":"incompatible","reason":"corpus identity mismatch","claimedGain":0.1}"#,
                r#"{"record":"enhancement","id":"e3","kind":"ockhamRemoval","producer":"p","outcome":"incompatible","reason":"corpus identity mismatch","claimedGain":0.1}"#,
                r#"{"record":"enhancement","id":"e4","kind":"ockhamRemoval","producer":"p","outcome":"incompatible","reason":"input count 2 != 3","claimedGain":0.1}"#,
            ],
        );
        let path = write(tmp.path(), "experiments.jsonl", &text);

        let report = read_one(&path).unwrap();
        assert_eq!(
            report.enhancements_by_outcome.get("alreadyPresent"),
            Some(&1)
        );
        assert_eq!(report.enhancements_by_outcome.get("incompatible"), Some(&3));
        assert_eq!(report.enhancements_by_outcome.get("applied"), None);
        assert_eq!(
            report.incompatible_reasons.get("corpus identity mismatch"),
            Some(&2)
        );
        assert_eq!(
            report.incompatible_reasons.get("input count 2 != 3"),
            Some(&1)
        );
        let table = render(&report);
        assert!(table.contains("corpus identity mismatch"), "{table}");
    }

    #[test]
    fn the_delta_distribution_comes_from_the_verdicts_that_recorded_one() {
        let tmp = tempfile::tempdir().unwrap();
        let text = [
            run_journal("improved", &[&verdict_line(0.04, true)]),
            run_journal("noImprovement", &[&verdict_line(-0.02, false)]),
            run_journal("improved", &[&verdict_line(0.01, true)]),
            // No verdict at all: nothing was scored, so nothing enters the
            // distribution.
            run_journal("nothingToDo", &[]),
        ]
        .concat();
        let path = write(tmp.path(), "experiments.jsonl", &text);

        let report = read_one(&path).unwrap();
        assert_eq!(report.runs_scored, 3);
        assert_eq!(report.runs_with_a_winner, 2);
        assert_eq!(report.best_vs_champion.len(), 3);
        assert!((report.min_best_vs_champion().unwrap() + 0.02).abs() < 1e-12);
        assert!((report.median_best_vs_champion().unwrap() - 0.01).abs() < 1e-12);
        assert!((report.max_best_vs_champion().unwrap() - 0.04).abs() < 1e-12);
    }

    #[test]
    fn an_even_number_of_deltas_takes_the_midpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let text = [
            run_journal("improved", &[&verdict_line(0.02, true)]),
            run_journal("improved", &[&verdict_line(0.04, true)]),
        ]
        .concat();
        let path = write(tmp.path(), "experiments.jsonl", &text);
        let report = read_one(&path).unwrap();
        assert!((report.median_best_vs_champion().unwrap() - 0.03).abs() < 1e-12);
    }

    #[test]
    fn the_screens_agreement_with_the_full_pass_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let text = [
            // Screened, and the corpus agreed.
            run_journal("improved", &[&screen_line(1, 3), &verdict_line(0.03, true)]),
            // Screened, and the corpus rejected what survived.
            run_journal(
                "noImprovement",
                &[&screen_line(2, 3), &verdict_line(-0.01, false)],
            ),
            // The screen killed everything, so the corpus was never paid for.
            run_journal("nothingToDo", &[&screen_line(0, 3)]),
            // Not screened at all — outside the comparison entirely.
            run_journal("improved", &[&verdict_line(0.05, true)]),
        ]
        .concat();
        let path = write(tmp.path(), "experiments.jsonl", &text);

        let report = read_one(&path).unwrap();
        assert_eq!(
            report.screen,
            ScreenSummary {
                screened_runs: 3,
                kept_nothing: 1,
                confirmed: 1,
                rejected: 1,
                undecided: 0,
            }
        );
        let table = render(&report);
        assert!(table.contains("full pass rejected"), "{table}");
    }

    /// Issue #43: the structured screen record and the string it replaced are
    /// both a screened run. A soak that spans the change must not lose half its
    /// phases.
    #[test]
    fn both_screen_record_shapes_count_as_a_screened_run() {
        let tmp = tempfile::tempdir().unwrap();
        let text = [
            run_journal(
                "improved",
                &[&screen_record_line(1), &verdict_line(0.03, true)],
            ),
            run_journal("improved", &[&screen_line(1, 3), &verdict_line(0.03, true)]),
            run_journal("improved", &[&verdict_line(0.05, true)]),
        ]
        .concat();
        let path = write(tmp.path(), "experiments.jsonl", &text);

        let report = read_one(&path).unwrap();
        assert_eq!(report.runs, 3);
        assert_eq!(
            report.screen.screened_runs, 2,
            "the unscreened run is not counted, and neither shape is missed"
        );
        assert_eq!(report.screen.confirmed, 2);
        assert_eq!(report.unreadable_lines, 0);
    }

    #[test]
    fn a_screened_run_killed_before_scoring_is_undecided_not_a_rejection() {
        let tmp = tempfile::tempdir().unwrap();
        let text = format!(
            "{}\n{}\n",
            r#"{"record":"opening","producer":"p","openingChecksum":"a","championChecksum":"b","corpusIdentity":"c","enhancementCount":3}"#,
            screen_line(2, 3)
        );
        let path = write(tmp.path(), "experiments.jsonl", &text);

        let report = read_one(&path).unwrap();
        assert_eq!(report.screen.screened_runs, 1);
        assert_eq!(report.screen.undecided, 1);
        assert_eq!(report.screen.rejected, 0);
        assert_eq!(report.runs_without_result, 1);
    }

    #[test]
    fn many_journals_aggregate_into_one_table() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write(
            tmp.path(),
            "a.jsonl",
            &run_journal("improved", &[&verdict_line(0.02, true)]),
        );
        let b = write(tmp.path(), "b.jsonl", &run_journal("nothingToDo", &[]));

        let report = Report::read(&[a, b]).unwrap();
        assert_eq!(report.journals, 2);
        assert_eq!(report.runs, 2);
        assert_eq!(report.runs_by_status.get("improved"), Some(&1));
        assert_eq!(report.runs_by_status.get("nothingToDo"), Some(&1));
    }

    #[test]
    fn an_unreadable_journal_fails_loudly() {
        let missing = PathBuf::from("/nonexistent/experiments.jsonl");
        let err = Report::read(&[missing]).unwrap_err();
        assert!(err.contains("experiments.jsonl"), "{err}");
        assert!(report(&[]).is_err(), "no journal is a usage error");
    }

    #[test]
    fn an_empty_journal_reports_zero_runs_rather_than_failing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(tmp.path(), "experiments.jsonl", "");
        let report = read_one(&path).unwrap();
        assert_eq!(report.runs, 0);
        assert_eq!(report.lines, 0);
        assert!(render(&report).contains("no enhancement records"));
    }
}
