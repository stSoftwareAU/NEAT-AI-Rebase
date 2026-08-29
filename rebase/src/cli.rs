//! The unattended CLI (Issue #6).
//!
//! ```text
//! neat_ai_rebase --champion <file> --enhancements <file-or-dir> \
//!                --training-data <dir> --scorer <path> --output-dir <dir>
//!
//! neat_ai_rebase report <experiments.jsonl>...
//! ```
//!
//! The `report` subcommand reads the journals earlier runs wrote and prints
//! what they did — see [`crate::report`]. It writes nothing, scores nothing,
//! and exits `0`; the flags above are not required with it.
//!
//! ## The one thing a caller must get right
//!
//! **Fetch the champion immediately before invoking.** Rebase loads it once,
//! at the start, and never re-reads it. The whole mechanism exists because a
//! champion goes stale while an optimiser runs; handing Rebase a champion that
//! is itself an hour old just moves the race one step along.
//!
//! ## Outputs
//!
//! | File | Written when |
//! | --- | --- |
//! | `population-candidate.json` | **only** when the scorer confirmed an improvement over the champion |
//! | `rebase.json` | always — the full summary, verdict included |
//! | `experiments.jsonl` | always — append-only journal for unattended diagnostics |
//! | `scoring/` | always — the creature files handed to the scorer, kept for diagnosis |
//!
//! `population-candidate.json` is a publishable population member, not bare
//! topology: it carries the champion's creature-level and per-neuron tags,
//! reconciled against the neurons that survived, with `score`, `error` and a
//! `rebase` summary stamped from this run's own verdict (Issue #48).
//!
//! Neither the champion file nor any enhancement file is ever written to.
//!
//! ## Exit codes
//!
//! | Code | Meaning |
//! | --- | --- |
//! | `0` | a verified improvement was emitted (with `--dry-run`: candidates built and validated) |
//! | `3` | no improvement, or nothing left to do. **A successful, non-destructive outcome** |
//! | `4` | incompatible input: nothing could be attempted |
//! | `1` | operational or scorer failure |
//!
//! `3` is deliberately not `0`: a caller polling exit status wants to tell
//! "published" from "correctly published nothing" without parsing JSON. It is
//! not an error, and a `set -e` caller should treat it as success.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use neat_core::training_data::TrainingDataConfig;
use neat_core::{CreatureExport, creature_to_json, parse_creature_json};
use serde::{Deserialize, Serialize};

use crate::corpus::{CorpusInfo, corpus_info};
use crate::creature::{sha256_hex, validate_source_creature};
use crate::engine::{EnhancementOutcome, RebaseOutcome, RebaseRequest, rebase};
use crate::enhancement::{Enhancement, EnhancementBundle};
use crate::harvest::harvest_delta;
use crate::journal::{Journal, Record, ScreenVerdict, ScreenedEnhancement};
use crate::scorer::{DirectoryScorer, ExternalScorer, ScoreResult, ScorerMode, Verdict, judge};
use crate::tags::{CreatureMeta, RebaseStamp};

/// Exit code: a verified improvement was emitted.
pub const EXIT_IMPROVED: i32 = 0;
/// Exit code: a subcommand that only reads did what it was asked.
///
/// The same number as [`EXIT_IMPROVED`], deliberately named apart: `report`
/// reads journals and decides nothing, so "improved" would be a lie about what
/// a `0` from it means.
pub const EXIT_OK: i32 = 0;
/// Exit code: operational or scorer failure.
pub const EXIT_FAILURE: i32 = 1;
/// Exit code: no improvement, or nothing left to do. A successful outcome.
pub const EXIT_NO_IMPROVEMENT: i32 = 3;
/// Exit code: incompatible input; nothing could be attempted.
pub const EXIT_INCOMPATIBLE: i32 = 4;

/// Default `--max-candidates`: the authoritative pass's budget, in creatures
/// scored over the whole corpus, excluding the baseline.
///
/// Also the cohort size above which an *uncapped* run (`--max-candidates 0`)
/// is treated as large enough for the screen to pay for itself — with no cap
/// there is no budget to compare against, so the documented default stands in
/// for one (Issue #42).
pub const DEFAULT_MAX_CANDIDATES: usize = 8;

/// Rebase portable NEAT-AI improvements onto the latest champion.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "neat_ai_rebase",
    version,
    about = "Rebase portable NEAT-AI improvements onto the latest champion and let the scorer decide.",
    long_about = "Rebase portable NEAT-AI improvements onto the latest champion and let the \
scorer decide.\n\n\
IMPORTANT: fetch --champion immediately before running this command. Rebase reads it once, at \
the start, and never re-reads it. Handing it a champion that is already stale reintroduces the \
race it exists to remove.\n\n\
Exit codes: 0 improvement emitted (or, with --dry-run, candidates validated); 3 no improvement \
or nothing to do (a successful, non-destructive outcome); 4 incompatible input; 1 operational \
or scorer failure.\n\n\
`neat_ai_rebase report <experiments.jsonl>...` reads those journals back and prints what a soak \
did, without running anything.",
    subcommand_negates_reqs = true,
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    /// Read `experiments.jsonl` journals back instead of running a rebase.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// The **freshly fetched** current global champion. Never written to.
    ///
    /// Required for a rebase run; absent when a subcommand was given.
    #[arg(long, value_name = "FILE", required = true)]
    pub champion: Option<PathBuf>,

    /// An enhancement bundle, a single enhancement, or a directory of either.
    /// Directory members are read in file-name order. Never written to.
    ///
    /// Mutually exclusive with `--harvest-from`.
    #[arg(
        long,
        value_name = "FILE-OR-DIR",
        required_unless_present = "harvest_from"
    )]
    pub enhancements: Option<PathBuf>,

    /// Take the enhancements from this creature instead of a bundle file.
    ///
    /// For a producer that does not file bundles yet. A Forest graft names
    /// every neuron it appends `forest-<patch id>-…` and the id digests the
    /// correction, so the patches can be read back out of the creature that
    /// carries them — and only the ones the champion lacks are taken. A
    /// reconstruction is accepted only if it hashes back to the id it was found
    /// under; see [`crate::harvest`].
    ///
    /// A bundle filed at the moment of acceptance is still better: a harvest
    /// only sees patches that survived into the published creature.
    #[arg(long, value_name = "FILE", conflicts_with = "enhancements")]
    pub harvest_from: Option<PathBuf>,

    /// Directory of `.bin` training data — the corpus the verdict is measured
    /// on, and the source of the corpus identity every enhancement is checked
    /// against.
    ///
    /// Required for a rebase run; absent when a subcommand was given.
    #[arg(long, value_name = "DIR", required = true)]
    pub training_data: Option<PathBuf>,

    /// The NEAT-AI-scorer binary (`rust_scorer`). Not required with
    /// `--dry-run`.
    #[arg(long, value_name = "PATH")]
    pub scorer: Option<PathBuf>,

    /// Where `population-candidate.json`, `rebase.json` and
    /// `experiments.jsonl` are written.
    ///
    /// Required for a rebase run; absent when a subcommand was given.
    #[arg(long, value_name = "DIR", required = true)]
    pub output_dir: Option<PathBuf>,

    /// Extra argument passed verbatim to the scorer. Repeatable.
    #[arg(long = "scorer-arg", value_name = "ARG", allow_hyphen_values = true)]
    pub scorer_args: Vec<String>,

    /// Score a candidate must beat the champion by before it is emitted.
    #[arg(long, default_value_t = 1e-9, value_name = "DELTA")]
    pub min_improvement: f64,

    /// Maximum candidates to construct, excluding the baseline. `0` = no cap.
    #[arg(long, default_value_t = DEFAULT_MAX_CANDIDATES, value_name = "N")]
    pub max_candidates: usize,

    /// Build and validate candidates without scoring, and without writing a
    /// population candidate.
    #[arg(long)]
    pub dry_run: bool,

    /// Screen each enhancement on a sub-sample before the authoritative pass,
    /// and drop the ones the stratum can see losing to the champion.
    ///
    /// Measured on a live fleet: of one donor's 13 patches, two improved the
    /// champion and eleven made it worse, and every cumulative prefix was
    /// worse than the best single it contained. Scoring the whole cohort on
    /// the full corpus finds the same answer, but a 14-patch cohort is ~28
    /// creatures over the whole corpus; screening first cuts that to a
    /// handful.
    ///
    /// Only engaged when the cohort does **not** already fit
    /// `--max-candidates`: below that the screen cannot save a corpus pass, so
    /// it would spend an extra scorer invocation only to discard information
    /// (Issue #42).
    ///
    /// The screen only ever *narrows* what is scored authoritatively. It
    /// cannot promote anything — [`ScorerMode::Sample`] is refused as a
    /// verdict.
    #[arg(long, value_name = "RATE")]
    pub screen_sample_rate: Option<f64>,

    /// Re-screen the survivors on a second, non-overlapping stratum and keep
    /// the intersection.
    ///
    /// Selecting on one stratum and trusting that selection is circular: with
    /// N candidates some beat the champion on any given stratum by chance, and
    /// "keep the winners" picks exactly those. Confirming on different records
    /// drops most of the accidents before they cost a full-corpus pass.
    #[arg(long, default_value_t = true, value_name = "BOOL", action = clap::ArgAction::Set)]
    pub screen_held_out: bool,
}

/// A subcommand that reads what earlier runs wrote, rather than running one.
#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum Command {
    /// Summarise one or more `experiments.jsonl` journals.
    ///
    /// Reads them back and prints how many runs rebased at all, how many of
    /// those the corpus confirmed, the spread of the best candidate's gain over
    /// the champion, what became of each enhancement, and how far the cheap
    /// screen agreed with the authoritative pass. Nothing is written and
    /// nothing is scored.
    Report {
        /// The journals to read. A partial last line — a run killed mid-write
        /// — is counted and reported, never fatal.
        #[arg(value_name = "EXPERIMENTS.JSONL", required = true)]
        journals: Vec<PathBuf>,
    },
}

impl Cli {
    /// The freshly fetched champion.
    ///
    /// # Errors
    ///
    /// Names the missing flag. `clap` enforces it for a rebase run; a library
    /// caller that built the struct itself gets a loud failure rather than a
    /// guess.
    fn champion_path(&self) -> Result<&Path, RunError> {
        self.champion
            .as_deref()
            .ok_or_else(|| RunError::incompatible("--champion is required"))
    }

    /// The corpus directory.
    ///
    /// # Errors
    ///
    /// Names the missing flag.
    fn training_data_dir(&self) -> Result<&Path, RunError> {
        self.training_data
            .as_deref()
            .ok_or_else(|| RunError::incompatible("--training-data is required"))
    }

    /// Where the outputs are written.
    ///
    /// # Errors
    ///
    /// Names the missing flag.
    fn output_directory(&self) -> Result<&Path, RunError> {
        self.output_dir
            .as_deref()
            .ok_or_else(|| RunError::incompatible("--output-dir is required"))
    }
}

/// One enhancement's fate, as written to `rebase.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnhancementSummary {
    /// Stable enhancement id.
    pub id: String,
    /// Payload kind.
    pub kind: String,
    /// Producer.
    pub producer: String,
    /// `applied` / `alreadyPresent` / `incompatible`.
    pub outcome: String,
    /// Reason, when incompatible.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The gain the producer measured on its own opening creature.
    pub claimed_gain: f64,
}

/// One constructed candidate, as written to `rebase.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateSummary {
    /// Cohort label.
    pub label: String,
    /// Checksum of the candidate creature.
    pub checksum: String,
    /// Enhancement ids applied.
    pub applied_ids: Vec<String>,
}

/// The `rebase.json` summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RebaseSummary {
    /// Summary format version.
    pub version: u32,
    /// `improved` / `noImprovement` / `nothingToDo` / `incompatible` /
    /// `dryRun`.
    pub status: String,
    /// True when `--dry-run` was set.
    pub dry_run: bool,
    /// Producer of the bundle.
    pub producer: String,
    /// Checksum the producer recorded for its opening creature — the stale
    /// ancestor the enhancements were discovered on.
    pub opening_checksum: String,
    /// SHA-256 of the champion file exactly as supplied.
    pub champion_file_checksum: String,
    /// Canonical checksum of the champion creature.
    pub champion_checksum: String,
    /// The corpus the decision was made on.
    pub corpus: CorpusInfo,
    /// Every enhancement's fate.
    pub enhancements: Vec<EnhancementSummary>,
    /// Every candidate constructed.
    pub candidates: Vec<CandidateSummary>,
    /// Candidates dropped to honour `--max-candidates`.
    pub dropped_for_cap: Vec<String>,
    /// Combinations that could not be constructed, with the reason.
    pub combination_failures: Vec<String>,
    /// The authoritative verdict, when scoring ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    /// Checksum of the emitted `population-candidate.json` **as written**,
    /// tags included, when one was emitted.
    ///
    /// Hash the file to check it. This is deliberately not the canonical
    /// creature checksum the verdict was gated on — that one is
    /// `verdict.winner.checksum`, and the two differ by exactly the tags,
    /// mirroring `champion_file_checksum` against `champion_checksum`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emitted_checksum: Option<String>,
}

/// A run that could not proceed.
#[derive(Debug, Clone, PartialEq)]
pub struct RunError {
    /// The message a human reads on stderr.
    pub message: String,
    /// The process exit code.
    pub code: i32,
}

impl RunError {
    fn incompatible(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: EXIT_INCOMPATIBLE,
        }
    }

    fn failure(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: EXIT_FAILURE,
        }
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RunError {}

/// Run the CLI with `scorer` as the judge.
///
/// Separated from [`run`] so the end-to-end test can drive the whole command
/// with a scripted scorer.
///
/// # Errors
///
/// A [`RunError`] carrying the message and the exit code to use.
pub fn run_with(cli: &Cli, scorer: Option<&dyn DirectoryScorer>) -> Result<i32, RunError> {
    if let Some(Command::Report { journals }) = &cli.command {
        // Reading decides nothing, so nothing is written and no scorer is
        // needed. An unreadable *file* is still a loud failure — only an
        // unreadable *line* is tolerated, and it is counted in the table.
        print!(
            "{}",
            crate::report::report(journals).map_err(RunError::failure)?
        );
        return Ok(EXIT_OK);
    }
    let output_dir = cli.output_directory()?;
    let training_data = cli.training_data_dir()?;
    std::fs::create_dir_all(output_dir)
        .map_err(|e| RunError::failure(format!("{}: {e}", output_dir.display())))?;
    let journal = Journal::new(output_dir.join("experiments.jsonl"));

    let (champion, champion_file_checksum, champion_meta) = load_champion(cli.champion_path()?)?;
    let corpus = corpus_info(
        training_data,
        &TrainingDataConfig::new(champion.input, champion.output),
    )
    .map_err(RunError::incompatible)?;
    let corpus_identity = corpus.identity.clone();
    let enhancements = match (&cli.enhancements, &cli.harvest_from) {
        (Some(path), _) => load_enhancements(path)?,
        (None, Some(path)) => harvest_from_creature(path, &champion, &corpus.identity)?,
        (None, None) => {
            return Err(RunError::incompatible(
                "one of --enhancements or --harvest-from is required".to_string(),
            ));
        }
    };
    if enhancements.is_empty() {
        // A harvest that finds nothing means the champion already carries every
        // recoverable discovery — a normal outcome, not bad input.
        let source = cli
            .enhancements
            .as_ref()
            .or(cli.harvest_from.as_ref())
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        if cli.harvest_from.is_some() {
            let journal = Journal::new(output_dir.join("experiments.jsonl"));
            let _ = journal.append(&Record::Result {
                status: "nothingToDo".into(),
                detail: Some(format!("nothing in {source} that the champion lacks")),
                emitted_checksum: None,
            });
            return Ok(EXIT_NO_IMPROVEMENT);
        }
        return Err(RunError::incompatible(format!(
            "no enhancements found at '{source}'"
        )));
    }
    let producer = enhancements[0].meta.producer.clone();
    let opening_checksum = enhancements[0].meta.base_checksum.clone();
    // The score of the creature the discoveries came from, as its producer
    // measured it. Read here because `--enhancements` and `--harvest-from`
    // both land it in the same place: a harvest fills it from the donor's own
    // `score` tag, a bundle from what the producer claimed. It goes into the
    // `rebase` tag so a reader can see that publishing that creature on its
    // own would have been a loss.
    let source_score = enhancements[0].meta.improved_score;

    let outcome = rebase(&RebaseRequest {
        champion: &champion,
        enhancements: &enhancements,
        corpus_identity: &corpus.identity,
        max_candidates: cli.max_candidates,
    })
    .map_err(|e| RunError::incompatible(e.to_string()))?;

    let _ = journal.append(&Record::Opening {
        producer: producer.clone(),
        opening_checksum: opening_checksum.clone(),
        champion_checksum: outcome.champion_checksum.clone(),
        corpus_identity: corpus.identity.clone(),
        enhancement_count: enhancements.len(),
    });
    journal
        .append_outcome(&outcome)
        .map_err(RunError::failure)?;

    let mut summary = RebaseSummary {
        version: crate::enhancement::ENHANCEMENT_FORMAT_VERSION,
        status: String::new(),
        dry_run: cli.dry_run,
        producer,
        opening_checksum,
        champion_file_checksum,
        champion_checksum: outcome.champion_checksum.clone(),
        corpus,
        enhancements: summarise_enhancements(&outcome),
        candidates: outcome
            .cohort
            .iter()
            .map(|c| CandidateSummary {
                label: c.label.clone(),
                checksum: c.checksum.clone(),
                applied_ids: c.applied_ids.clone(),
            })
            .collect(),
        dropped_for_cap: outcome.dropped_for_cap.clone(),
        combination_failures: outcome.combination_failures.clone(),
        verdict: None,
        emitted_checksum: None,
    };

    // Nothing to score. That is a normal outcome when the champion already
    // carries the work — and an incompatible one when nothing could even be
    // attempted.
    if outcome.is_empty() {
        let all_incompatible = !outcome.reports.is_empty()
            && outcome
                .reports
                .iter()
                .all(|r| matches!(r.outcome, EnhancementOutcome::Incompatible(_)));
        let (status, code) = if all_incompatible {
            ("incompatible", EXIT_INCOMPATIBLE)
        } else {
            ("nothingToDo", EXIT_NO_IMPROVEMENT)
        };
        summary.status = status.into();
        finish(output_dir, &journal, &summary, status, None)?;
        return Ok(code);
    }

    if cli.dry_run {
        summary.status = "dryRun".into();
        finish(output_dir, &journal, &summary, "dryRun", None)?;
        return Ok(EXIT_IMPROVED);
    }

    let scorer = scorer.ok_or_else(|| {
        RunError::failure("--scorer is required unless --dry-run is set".to_string())
    })?;

    // Narrow the cohort before paying for the corpus. Never widens it, and
    // never promotes: `judge` refuses a sampled mode outright.
    //
    // Only when it can buy something. The screen costs a scorer invocation of
    // its own and can only ever discard information, so a cohort the
    // authoritative pass was going to score in full anyway is handed straight
    // to it — the full corpus is the better test, and it was already paid for
    // (Issue #42).
    let built = cohort_before_the_cap(&outcome);
    let budget = screening_budget(cli.max_candidates);
    let rate = cli.screen_sample_rate.filter(|r| *r > 0.0 && *r < 1.0);
    let outcome = match rate {
        Some(rate) if enhancements.len() > 1 => {
            if built > budget {
                screen(
                    cli,
                    scorer,
                    &champion,
                    &enhancements,
                    &corpus_identity,
                    rate,
                    &journal,
                )?
            } else {
                let reason = format!(
                    "cohort of {built} fits the authoritative budget of {budget}; \
                     screening could not save a corpus pass"
                );
                eprintln!("neat_ai_rebase: screen skipped — {reason}");
                let _ = journal.append(&Record::Dropped {
                    label: crate::journal::SCREEN_SKIPPED_LABEL.to_string(),
                    reason,
                });
                outcome
            }
        }
        _ => outcome,
    };
    // The screen may have narrowed the cohort, and `rebase.json` has to record
    // what was actually scored — not what was built before the screen ran.
    summary.candidates = outcome
        .cohort
        .iter()
        .map(|c| CandidateSummary {
            label: c.label.clone(),
            checksum: c.checksum.clone(),
            applied_ids: c.applied_ids.clone(),
        })
        .collect();
    if outcome.is_empty() {
        summary.status = "nothingToDo".into();
        finish(output_dir, &journal, &summary, "nothingToDo", None)?;
        return Ok(EXIT_NO_IMPROVEMENT);
    }

    let verdict = judge(
        scorer,
        &outcome,
        training_data,
        &output_dir.join("scoring"),
        cli.min_improvement,
        ScorerMode::Full,
    )
    .map_err(|e| RunError::failure(e.to_string()))?;
    let _ = journal.append(&Record::Verdict(Box::new(verdict.clone())));

    let emitted = match &verdict.winner {
        Some(winner) => {
            let creature = outcome
                .cohort
                .iter()
                .find(|c| c.label == winner.label)
                .ok_or_else(|| {
                    RunError::failure(format!("winner `{}` left the cohort", winner.label))
                })?;
            let json = creature_to_json(&creature.creature)
                .map_err(|e| RunError::failure(e.to_string()))?;
            // Last gate before anything is published: what is about to be
            // written must be the creature that was actually scored. The gate
            // stays over the untagged bytes, because those are the bytes the
            // scorer saw — tags carry provenance and never reach the network.
            let checksum = sha256_hex(json.as_bytes());
            if checksum != winner.checksum {
                return Err(RunError::failure(format!(
                    "winner checksum drifted between scoring and emission: {} != {}",
                    checksum, winner.checksum
                )));
            }
            // Write the creature back with its tags, not as bare topology.
            //
            // Every consumer reads the score off the creature it is about to
            // publish, and GRQ-sampler's check-in guard additionally refuses a
            // creature that arrived with a better score but lost its discovery
            // and intelligent-design provenance. A bare `creature_to_json` has
            // neither: `CreatureExport` does not model `tags`, so serialising
            // through it drops the authoritative numbers this binary just
            // measured along with every per-neuron tag the champion carried.
            //
            // The tags come from the champion — that is the creature being
            // improved and the lineage the population tracks — reconciled
            // against the neurons that actually survived, and then stamped
            // with this run's own numbers.
            let mut meta = champion_meta.clone();
            meta.retain_neurons(&creature.creature);
            meta.stamp(&RebaseStamp {
                score: winner.result.score,
                error: winner.result.error,
                champion_score: verdict.baseline.score,
                source_score,
                applied: winner.applied_ids.len(),
                label: &winner.label,
                source: &summary.producer,
            });
            let tagged = meta
                .serialize_with(&creature.creature, false)
                .map_err(RunError::failure)?;
            let path = output_dir.join("population-candidate.json");
            std::fs::write(&path, &tagged)
                .map_err(|e| RunError::failure(format!("{}: {e}", path.display())))?;
            // Two checksums again, for the same reason `load_champion` keeps
            // two: `checksum` above identifies the *creature* and is what the
            // scorer's verdict is gated on; this one identifies the *bytes a
            // caller receives*, which now carry the tags. A caller checking
            // what it was handed hashes the file, so that is what
            // `emittedChecksum` has to be.
            Some(sha256_hex(tagged.as_bytes()))
        }
        None => None,
    };

    let status = if emitted.is_some() {
        "improved"
    } else {
        "noImprovement"
    };
    summary.status = status.into();
    summary.verdict = Some(verdict);
    summary.emitted_checksum = emitted.clone();
    finish(output_dir, &journal, &summary, status, emitted)?;
    Ok(if status == "improved" {
        EXIT_IMPROVED
    } else {
        EXIT_NO_IMPROVEMENT
    })
}

/// Run the CLI, spawning the real `rust_scorer` binary.
///
/// # Errors
///
/// A [`RunError`] carrying the message and the exit code to use.
pub fn run(cli: &Cli) -> Result<i32, RunError> {
    let external = cli
        .scorer
        .as_ref()
        .map(|path| ExternalScorer::with_args(path, cli.scorer_args.clone()));
    run_with(cli, external.as_ref().map(|s| s as &dyn DirectoryScorer))
}

fn finish(
    output_dir: &Path,
    journal: &Journal,
    summary: &RebaseSummary,
    status: &str,
    emitted_checksum: Option<String>,
) -> Result<(), RunError> {
    let path = output_dir.join("rebase.json");
    let json =
        serde_json::to_string_pretty(summary).map_err(|e| RunError::failure(e.to_string()))?;
    std::fs::write(&path, json)
        .map_err(|e| RunError::failure(format!("{}: {e}", path.display())))?;
    journal
        .append(&Record::Result {
            status: status.to_string(),
            detail: None,
            emitted_checksum,
        })
        .map_err(RunError::failure)?;
    Ok(())
}

fn summarise_enhancements(outcome: &RebaseOutcome) -> Vec<EnhancementSummary> {
    outcome
        .reports
        .iter()
        .map(|r| EnhancementSummary {
            id: r.id.clone(),
            kind: r.kind.to_string(),
            producer: r.producer.clone(),
            outcome: r.outcome.label().to_string(),
            reason: match &r.outcome {
                EnhancementOutcome::Incompatible(reason) => Some(reason.clone()),
                _ => None,
            },
            claimed_gain: r.claimed_gain,
        })
        .collect()
}

/// Derive the enhancement bundle from a creature that already carries the work.
///
/// Only the patches the champion lacks are taken — the rest are already there,
/// and [`crate::adapter::is_present`] would skip them anyway. A reconstruction
/// that does not hash back to the id it was found under is discarded and
/// reported, never grafted under a different name.
fn harvest_from_creature(
    path: &Path,
    champion: &CreatureExport,
    corpus_identity: &str,
) -> Result<Vec<Enhancement>, RunError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| RunError::incompatible(format!("{}: {e}", path.display())))?;
    let source = parse_creature_json(&text)
        .map_err(|e| RunError::incompatible(format!("{}: {e}", path.display())))?;
    validate_source_creature(&source)
        .map_err(|e| RunError::incompatible(format!("{}: {e}", path.display())))?;

    let harvest = harvest_delta(&source, champion);
    for skip in &harvest.skipped {
        eprintln!(
            "neat_ai_rebase: skipped patch {} — {}",
            skip.id, skip.reason
        );
    }
    // A harvest measures nothing of its own, so both scores are the source's
    // own tag when it has one and zero otherwise. Recording an invented gain
    // would put a number in the journal that nothing stands behind; Rebase
    // never promotes on those numbers in any case.
    let claimed = crate::tags::CreatureMeta::from_json(&text)
        .score()
        .unwrap_or(0.0);
    harvest
        .into_enhancements(
            &source,
            corpus_identity,
            &format!(
                "harvest/{}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
            claimed,
        )
        .map_err(RunError::incompatible)
}

/// Candidates the authoritative pass would score if nothing were capped.
///
/// The cap is applied while the cohort is built, so what survived it is not the
/// size of the work: the members dropped for the cap have to be added back to
/// see what the screen would actually be saving.
fn cohort_before_the_cap(outcome: &RebaseOutcome) -> usize {
    outcome.candidates().count() + outcome.dropped_for_cap.len()
}

/// The cohort size above which the screen can pay for itself.
///
/// `--max-candidates` is the number of creatures the authoritative pass is
/// willing to score, so a cohort that fits it costs the same with or without a
/// screen. An uncapped run has no such number; the documented default stands in
/// for one rather than letting a 14-patch cohort reach the corpus unscreened.
fn screening_budget(max_candidates: usize) -> usize {
    if max_candidates == 0 {
        DEFAULT_MAX_CANDIDATES
    } else {
        max_candidates
    }
}

/// Score every enhancement offered to one screen phase against the stratum's
/// baseline, and say which of them survive (Issue #43).
///
/// Returns the per-enhancement record in cohort order and the survivors in the
/// same order, from one classification — so the line the journal carries and
/// the decision the screen made can never disagree.
///
/// An enhancement the engine built no single-patch candidate for is reported as
/// [`ScreenVerdict::NotBuilt`] rather than vanishing from the count: it did not
/// lose on the stratum, the stratum never saw it.
fn measure_phase(
    outcome: &RebaseOutcome,
    offered: &[Enhancement],
    scores: &BTreeMap<String, ScoreResult>,
    baseline: f64,
    resolution: f64,
) -> (Vec<ScreenedEnhancement>, Vec<Enhancement>) {
    let mut measured = Vec::with_capacity(offered.len());
    let mut survivors = Vec::new();
    for candidate in outcome.candidates().filter(|c| c.applied_ids.len() == 1) {
        let Some(enhancement) = offered
            .iter()
            .find(|e| e.meta.id == candidate.applied_ids[0])
        else {
            continue;
        };
        let score = scores.get(&candidate.label).map(|r| r.score);
        let verdict = ScreenVerdict::classify(score, baseline, resolution);
        measured.push(ScreenedEnhancement {
            id: enhancement.meta.id.clone(),
            producer: enhancement.meta.producer.clone(),
            score,
            delta: score.map(|s| s - baseline),
            verdict,
            kept: verdict.survives(),
        });
        if verdict.survives() {
            survivors.push(enhancement.clone());
        }
    }
    for enhancement in offered {
        if measured.iter().any(|m| m.id == enhancement.meta.id) {
            continue;
        }
        measured.push(ScreenedEnhancement {
            id: enhancement.meta.id.clone(),
            producer: enhancement.meta.producer.clone(),
            score: None,
            delta: None,
            verdict: ScreenVerdict::NotBuilt,
            kept: false,
        });
    }
    (measured, survivors)
}

/// Narrow the cohort by dropping what a sample can see losing.
///
/// Two strata, not one. Selecting on a stratum and trusting that selection is
/// circular — with N candidates some beat the champion on any given stratum by
/// chance, and "keep the winners" picks exactly those. Re-screening the
/// survivors on different records drops most of those accidents before they
/// reach the corpus.
///
/// Elimination is one-sided: see [`ScreenVerdict::survives`]. A candidate the
/// stratum cannot resolve is carried forward, so the only thing the screen
/// removes is work that a sample of the corpus already says is a loss.
///
/// Every phase journals what it measured, per enhancement, as
/// [`Record::Screen`] — the deltas and the size of the stratum that produced
/// them, because the staging directory is deleted on the way out and a bare
/// count of survivors cannot be diagnosed afterwards (Issue #43).
///
/// Whatever survives still has to win the authoritative pass; this only decides
/// what that pass is spent on.
#[allow(clippy::too_many_arguments)]
fn screen(
    cli: &Cli,
    scorer: &dyn DirectoryScorer,
    champion: &CreatureExport,
    enhancements: &[Enhancement],
    corpus_identity: &str,
    rate: f64,
    journal: &Journal,
) -> Result<RebaseOutcome, RunError> {
    let output_dir = cli.output_directory()?;
    let training_data = cli.training_data_dir()?;
    let mut kept: Vec<Enhancement> = enhancements.to_vec();
    let phases: &[u64] = if cli.screen_held_out { &[0, 1] } else { &[0] };
    for &phase in phases {
        if kept.len() < 2 {
            break;
        }
        let outcome = rebase(&RebaseRequest {
            champion,
            enhancements: &kept,
            corpus_identity,
            max_candidates: 0,
        })
        .map_err(|e| RunError::incompatible(e.to_string()))?;
        // Stage and score directly. `judge` refuses a sampled mode outright —
        // correctly, it is the thing that decides — so the screen stages the
        // cohort itself and reads the baseline out of the raw results.
        let staging = output_dir.join(format!("screen-{phase}"));
        std::fs::create_dir_all(&staging)
            .map_err(|e| RunError::failure(format!("{}: {e}", staging.display())))?;
        crate::scorer::stage(&outcome, &staging).map_err(|e| RunError::failure(e.to_string()))?;
        let scores = scorer
            .score_directory(&staging, training_data, ScorerMode::Sample { rate, phase })
            .map_err(|e| RunError::failure(e.to_string()))?;
        let baseline_result = scores
            .get(crate::engine::BASELINE_LABEL)
            .ok_or_else(|| RunError::failure("screen produced no baseline"))?;
        let baseline = baseline_result.score;
        let record_count = baseline_result.record_count;
        let (measured, survivors) =
            measure_phase(&outcome, &kept, &scores, baseline, cli.min_improvement);
        let _ = journal.append(&Record::Screen {
            phase,
            sample_rate: rate,
            resolution: cli.min_improvement,
            baseline_score: baseline,
            record_count,
            kept: survivors.len(),
            enhancements: measured.clone(),
        });
        eprintln!(
            "neat_ai_rebase: screen phase {phase} kept {} of {} \
             (baseline {baseline:.6} over {record_count} records at rate {rate})",
            survivors.len(),
            kept.len()
        );
        for m in &measured {
            // Scientific notation deliberately: `-3e-4` and `0.0` are the two
            // explanations a survivor count cannot tell apart, and fixed-point
            // rounding hides exactly that difference.
            let delta = m
                .delta
                .map_or_else(|| "none".to_string(), |d| format!("{d:+.3e}"));
            eprintln!(
                "neat_ai_rebase:   {} {} delta {delta} {}",
                m.id,
                m.producer,
                m.verdict.label()
            );
        }
        let _ = std::fs::remove_dir_all(&staging);
        if survivors.is_empty() {
            kept.clear();
            break;
        }
        kept = survivors;
    }

    rebase(&RebaseRequest {
        champion,
        enhancements: &kept,
        corpus_identity,
        max_candidates: cli.max_candidates,
    })
    .map_err(|e| RunError::incompatible(e.to_string()))
}

/// Read the champion and its file checksum. The file is never written to.
fn load_champion(path: &Path) -> Result<(CreatureExport, String, CreatureMeta), RunError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| RunError::incompatible(format!("{}: {e}", path.display())))?;
    let creature = parse_creature_json(&text)
        .map_err(|e| RunError::incompatible(format!("{}: {e}", path.display())))?;
    validate_source_creature(&creature)
        .map_err(|e| RunError::incompatible(format!("{}: {e}", path.display())))?;
    // `CreatureExport` models the fields it validates and drops the rest, so
    // the champion's tags have to be lifted out of the raw text here — this is
    // the only point at which they still exist — and carried to the emit path.
    // Everything downstream works on the parsed creature.
    let meta = CreatureMeta::from_json(&text);
    // Two checksums, deliberately: the engine's canonical one identifies the
    // *creature*, and is what a candidate is compared against; this one
    // identifies the *bytes on disk*, which is what a caller comparing against
    // the population sees. They differ whenever the file was pretty-printed.
    Ok((creature, sha256_hex(text.as_bytes()), meta))
}

/// Read a bundle, a single enhancement, or a directory of either.
///
/// Directory members are read in file-name order so the same directory always
/// produces the same bundle order, and therefore the same prefixes.
fn load_enhancements(path: &Path) -> Result<Vec<Enhancement>, RunError> {
    if path.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(|e| RunError::incompatible(format!("{}: {e}", path.display())))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        files.sort();
        let mut out = Vec::new();
        for file in files {
            out.extend(load_one(&file)?);
        }
        return Ok(out);
    }
    load_one(path)
}

fn load_one(path: &Path) -> Result<Vec<Enhancement>, RunError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| RunError::incompatible(format!("{}: {e}", path.display())))?;
    // A bundle has a top-level `enhancements`; a bare enhancement has `meta`.
    // Either shape is accepted, and neither is guessed at: an unknown version
    // or kind fails closed in both readers.
    let looks_like_bundle = serde_json::from_str::<serde_json::Value>(&text)
        .map(|v| v.get("enhancements").is_some())
        .unwrap_or(false);
    if looks_like_bundle {
        let bundle = EnhancementBundle::parse_json(&text)
            .map_err(|e| RunError::incompatible(format!("{}: {e}", path.display())))?;
        Ok(bundle.enhancements)
    } else {
        let one = Enhancement::parse_json(&text)
            .map_err(|e| RunError::incompatible(format!("{}: {e}", path.display())))?;
        Ok(vec![one])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enhancement::{Payload, ProducerContext};
    use crate::fixtures::{evolved_descendant, linear_hidden_creature};
    use crate::patch::{Node, Patch, Provenance};
    use crate::scorer::ScriptedScorer;
    use crate::tags::Tag;
    use clap::CommandFactory;
    use std::collections::HashSet;

    struct Harness {
        _tmp: tempfile::TempDir,
        cli: Cli,
        corpus_identity: String,
        /// Where a test writes its bundle; `cli.enhancements` points here.
        enhancements: PathBuf,
    }

    fn write_corpus(dir: &Path, inputs: usize, outputs: usize) {
        std::fs::create_dir_all(dir).unwrap();
        let mut bytes = Vec::new();
        for record in 0..8u32 {
            for slot in 0..(inputs + outputs) {
                let v = (record as f32) * 0.1 + slot as f32;
                bytes.extend_from_slice(&v.to_le_bytes());
            }
        }
        std::fs::write(dir.join("corpus.bin"), bytes).unwrap();
    }

    fn harness(champion: &CreatureExport) -> Harness {
        let tmp = tempfile::tempdir().unwrap();
        let training = tmp.path().join("training");
        write_corpus(&training, champion.input, champion.output);
        let corpus = corpus_info(
            &training,
            &TrainingDataConfig::new(champion.input, champion.output),
        )
        .unwrap();
        let champion_path = tmp.path().join("champion.json");
        std::fs::write(&champion_path, creature_to_json(champion).unwrap()).unwrap();
        let enhancements = tmp.path().join("enhancements");
        Harness {
            cli: Cli {
                command: None,
                champion: Some(champion_path),
                enhancements: Some(enhancements.clone()),
                harvest_from: None,
                screen_sample_rate: None,
                screen_held_out: true,
                training_data: Some(training),
                scorer: None,
                output_dir: Some(tmp.path().join("out")),
                scorer_args: Vec::new(),
                min_improvement: 1e-9,
                max_candidates: 8,
                dry_run: false,
            },
            corpus_identity: corpus.identity,
            enhancements,
            _tmp: tmp,
        }
    }

    fn forest(corpus: &str, feature: usize, right: f32) -> Enhancement {
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
                base_checksum: "opening-checksum".into(),
                base_score: 0.5,
                improved_score: 0.6,
                corpus_identity: corpus.into(),
                input_count: 2,
                output_count: 1,
            },
        )
    }

    fn write_bundle(path: &Path, enhancements: Vec<Enhancement>) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bundle = EnhancementBundle::from_enhancements(enhancements);
        std::fs::write(path, serde_json::to_string_pretty(&bundle).unwrap()).unwrap();
    }

    /// The output directory of a CLI a test built, which always has one.
    fn out(cli: &Cli) -> &Path {
        cli.output_dir
            .as_deref()
            .expect("a test CLI is built with an output directory")
    }

    /// The champion file of a CLI a test built, which always has one.
    fn champion_file(cli: &Cli) -> &Path {
        cli.champion
            .as_deref()
            .expect("a test CLI is built with a champion")
    }

    fn summary(dir: &Path) -> RebaseSummary {
        serde_json::from_str(&std::fs::read_to_string(dir.join("rebase.json")).unwrap()).unwrap()
    }

    /// Rewrite a champion file with the tags a real population member carries.
    ///
    /// `harness` writes bare topology, which is precisely the creature that has
    /// no provenance to lose — useless for proving that provenance survives.
    fn tag_champion_file(path: &Path) {
        let mut doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        doc["tags"] = serde_json::json!([
            { "name": "score", "value": "0.111111" },
            { "name": "lineage", "value": "champion-lineage" },
        ]);
        for n in doc["neurons"].as_array_mut().unwrap() {
            if n["uuid"] == "h1" {
                n["tags"] = serde_json::json!([{ "name": "discovery", "value": "seed-h1" }]);
            }
        }
        std::fs::write(path, serde_json::to_string(&doc).unwrap()).unwrap();
    }

    #[test]
    fn end_to_end_emits_a_population_candidate_when_the_scorer_agrees() {
        let champion = evolved_descendant(2.0, 0.5);
        let h = harness(&champion);
        let bundle_path = h.enhancements.join("bundle.json");
        write_bundle(&bundle_path, vec![forest(&h.corpus_identity, 1, 0.25)]);

        let scorer = ScriptedScorer::flat(0.50).with("single-00", 0.60);
        let code = run_with(&h.cli, Some(&scorer)).unwrap();
        assert_eq!(code, EXIT_IMPROVED);

        let emitted = out(&h.cli).join("population-candidate.json");
        assert!(emitted.exists());
        let s = summary(out(&h.cli));
        assert_eq!(s.status, "improved");
        assert_eq!(
            s.emitted_checksum.unwrap(),
            sha256_hex(std::fs::read_to_string(&emitted).unwrap().as_bytes())
        );
        let verdict = s.verdict.unwrap();
        assert!(verdict.improved());
        assert!((verdict.baseline.score - 0.50).abs() < 1e-12);
        assert!(
            out(&h.cli).join("experiments.jsonl").exists(),
            "the journal is always written"
        );
    }

    /// Issue #4438: what is published must be a creature, not bare topology.
    ///
    /// Every consumer reads the score off the artefact it is about to check in,
    /// and GRQ-sampler's guard additionally refuses one that arrived with a
    /// better score but lost its lineage. Before the fix this file had no
    /// `tags` key at all, so both checks failed on every successful rebase.
    #[test]
    fn the_emitted_candidate_carries_the_score_and_the_champions_provenance() {
        let champion = evolved_descendant(2.0, 0.5);
        let h = harness(&champion);
        tag_champion_file(champion_file(&h.cli));
        write_bundle(
            &h.enhancements.join("bundle.json"),
            vec![forest(&h.corpus_identity, 1, 0.25)],
        );

        let scorer = ScriptedScorer::flat(0.50).with("single-00", 0.60);
        assert_eq!(run_with(&h.cli, Some(&scorer)).unwrap(), EXIT_IMPROVED);

        let text = std::fs::read_to_string(out(&h.cli).join("population-candidate.json")).unwrap();
        let meta = CreatureMeta::from_json(&text);

        // The score is the one the judge measured, and it has replaced the
        // champion's stale tag rather than sitting beside it.
        let winner = summary(out(&h.cli))
            .verdict
            .unwrap()
            .winner
            .expect("a winner");
        let emitted_score = meta.score().expect("a numeric score tag");
        assert!((emitted_score - winner.result.score).abs() < 1e-12);
        assert!((emitted_score - 0.60).abs() < 1e-12);
        assert_eq!(
            meta.tags.iter().filter(|t| t.name == "score").count(),
            1,
            "upserted, not appended"
        );
        assert!(
            (meta
                .get("error")
                .expect("an error tag")
                .parse::<f64>()
                .unwrap()
                - winner.result.error)
                .abs()
                < 1e-12
        );
        assert!(
            meta.get("rebase")
                .expect("a rebase tag")
                .starts_with("🪢 Rebase"),
        );

        // The champion's own provenance rides along, creature-level and
        // per-neuron.
        assert_eq!(meta.get("lineage"), Some("champion-lineage"));
        assert_eq!(
            meta.neuron_tags.get("h1").map(Vec::as_slice),
            Some(&[Tag::new("discovery", "seed-h1")][..]),
        );

        // And no tag names a neuron the rebase left behind.
        let creature = parse_creature_json(&text).expect("a valid creature");
        let present: HashSet<&str> = creature.neurons.iter().map(|n| n.uuid.as_str()).collect();
        for uuid in meta.neuron_tags.keys() {
            assert!(
                present.contains(uuid.as_str()),
                "{uuid} is tagged but no longer in the creature"
            );
        }
        // The graft really did happen — otherwise this test would pass on a
        // creature that was never rebased at all.
        assert!(creature.neurons.len() > champion.neurons.len());
    }

    #[test]
    fn no_improvement_is_a_successful_non_destructive_outcome() {
        let champion = evolved_descendant(2.0, 0.5);
        let h = harness(&champion);
        write_bundle(
            &h.enhancements.join("bundle.json"),
            vec![forest(&h.corpus_identity, 1, 0.25)],
        );
        let scorer = ScriptedScorer::flat(0.80).with("single-00", 0.70);
        let code = run_with(&h.cli, Some(&scorer)).unwrap();
        assert_eq!(code, EXIT_NO_IMPROVEMENT);
        assert!(!out(&h.cli).join("population-candidate.json").exists());
        assert_eq!(summary(out(&h.cli)).status, "noImprovement");
        // The champion file is untouched.
        let champion_now = std::fs::read_to_string(champion_file(&h.cli)).unwrap();
        assert_eq!(champion_now, creature_to_json(&champion).unwrap());
    }

    #[test]
    fn a_scorer_failure_is_an_operational_failure_and_emits_nothing() {
        let h = harness(&evolved_descendant(2.0, 0.5));
        write_bundle(
            &h.enhancements.join("bundle.json"),
            vec![forest(&h.corpus_identity, 1, 0.25)],
        );
        let scorer = ScriptedScorer::flat(0.5)
            .failing(crate::scorer::ScorerError::Spawn("no such binary".into()));
        let err = run_with(&h.cli, Some(&scorer)).unwrap_err();
        assert_eq!(err.code, EXIT_FAILURE);
        assert!(!out(&h.cli).join("population-candidate.json").exists());
    }

    #[test]
    fn dry_run_validates_without_scoring_or_emitting() {
        let h = harness(&evolved_descendant(2.0, 0.5));
        write_bundle(
            &h.enhancements.join("bundle.json"),
            vec![forest(&h.corpus_identity, 1, 0.25)],
        );
        let mut cli = h.cli;
        cli.dry_run = true;
        let code = run_with(&cli, None).unwrap();
        assert_eq!(code, EXIT_IMPROVED);
        assert!(!out(&cli).join("population-candidate.json").exists());
        let s = summary(out(&cli));
        assert_eq!(s.status, "dryRun");
        assert!(s.verdict.is_none());
        assert_eq!(s.candidates.len(), 2, "baseline plus one candidate");
    }

    #[test]
    fn corpus_drift_is_incompatible_input() {
        let h = harness(&evolved_descendant(2.0, 0.5));
        write_bundle(
            &h.enhancements.join("bundle.json"),
            vec![forest("a-corpus-from-somewhere-else", 1, 0.25)],
        );
        // Nothing could be attempted — but the run still writes its summary
        // and journal, so an unattended host can be told why.
        let code = run_with(&h.cli, Some(&ScriptedScorer::flat(0.5))).unwrap();
        assert_eq!(code, EXIT_INCOMPATIBLE);
        assert_eq!(summary(out(&h.cli)).status, "incompatible");
        assert!(!out(&h.cli).join("population-candidate.json").exists());
    }

    #[test]
    fn an_already_present_bundle_is_nothing_to_do() {
        let champion = linear_hidden_creature(2.0);
        let h = harness(&champion);
        // Graft once so the champion already carries the patch.
        let e = forest(&h.corpus_identity, 1, 0.25);
        let outcome = rebase(&RebaseRequest {
            champion: &champion,
            enhancements: std::slice::from_ref(&e),
            corpus_identity: &h.corpus_identity,
            max_candidates: 0,
        })
        .unwrap();
        let grafted = outcome.cohort[1].creature.clone();
        std::fs::write(champion_file(&h.cli), creature_to_json(&grafted).unwrap()).unwrap();
        write_bundle(&h.enhancements.join("bundle.json"), vec![e]);

        let code = run_with(&h.cli, Some(&ScriptedScorer::flat(0.5))).unwrap();
        assert_eq!(code, EXIT_NO_IMPROVEMENT);
        assert_eq!(summary(out(&h.cli)).status, "nothingToDo");
        assert!(!out(&h.cli).join("population-candidate.json").exists());
    }

    #[test]
    fn a_directory_of_enhancements_is_read_in_file_name_order() {
        let h = harness(&evolved_descendant(2.0, 0.5));
        std::fs::create_dir_all(&h.enhancements).unwrap();
        let a = forest(&h.corpus_identity, 0, 0.25);
        let b = forest(&h.corpus_identity, 1, -0.1);
        std::fs::write(
            h.enhancements.join("02-second.json"),
            serde_json::to_string(&b).unwrap(),
        )
        .unwrap();
        std::fs::write(
            h.enhancements.join("01-first.json"),
            serde_json::to_string(&a).unwrap(),
        )
        .unwrap();

        let mut cli = h.cli;
        cli.dry_run = true;
        run_with(&cli, None).unwrap();
        let s = summary(out(&cli));
        assert_eq!(s.enhancements[0].id, a.meta.id);
        assert_eq!(s.enhancements[1].id, b.meta.id);
    }

    #[test]
    fn a_missing_or_malformed_enhancement_file_is_incompatible_input() {
        let h = harness(&evolved_descendant(2.0, 0.5));
        let err = run_with(&h.cli, Some(&ScriptedScorer::flat(0.5))).unwrap_err();
        assert_eq!(err.code, EXIT_INCOMPATIBLE);

        std::fs::create_dir_all(&h.enhancements).unwrap();
        std::fs::write(h.enhancements.join("bad.json"), "{ not json").unwrap();
        let err = run_with(&h.cli, Some(&ScriptedScorer::flat(0.5))).unwrap_err();
        assert_eq!(err.code, EXIT_INCOMPATIBLE);
    }

    #[test]
    fn harvest_from_derives_the_bundle_from_a_creature() {
        // The producer filed no bundle. The champion is the ancestor; the
        // Forests output carries a graft it lacks.
        let ancestor = linear_hidden_creature(2.0);
        let h = harness(&ancestor);
        let corpus = h.corpus_identity.clone();
        let grafted = {
            let e = forest(&corpus, 1, 0.25);
            let outcome = rebase(&RebaseRequest {
                champion: &ancestor,
                enhancements: std::slice::from_ref(&e),
                corpus_identity: &corpus,
                max_candidates: 0,
            })
            .unwrap();
            outcome.cohort[1].creature.clone()
        };
        let forests_output = h._tmp.path().join("best.json");
        std::fs::write(&forests_output, creature_to_json(&grafted).unwrap()).unwrap();

        let mut cli = h.cli.clone();
        cli.enhancements = None;
        cli.harvest_from = Some(forests_output);
        let scorer = ScriptedScorer::flat(0.50).with("single-00", 0.60);
        assert_eq!(run_with(&cli, Some(&scorer)).unwrap(), EXIT_IMPROVED);

        let s = summary(out(&cli));
        assert_eq!(s.status, "improved");
        assert_eq!(
            s.enhancements.len(),
            1,
            "one patch harvested from the creature"
        );
        assert!(s.enhancements[0].producer.starts_with("harvest/"), "{s:?}");
        assert!(out(&cli).join("population-candidate.json").exists());
    }

    #[test]
    fn harvest_from_a_creature_with_nothing_new_is_nothing_to_do() {
        // Harvesting the champion from itself: no patch it lacks.
        let champion = linear_hidden_creature(2.0);
        let h = harness(&champion);
        let mut cli = h.cli.clone();
        cli.enhancements = None;
        cli.harvest_from = Some(champion_file(&cli).to_path_buf());
        let code = run_with(&cli, Some(&ScriptedScorer::flat(0.5))).unwrap();
        assert_eq!(
            code, EXIT_NO_IMPROVEMENT,
            "a harvest with no delta is normal"
        );
        assert!(!out(&cli).join("population-candidate.json").exists());
    }

    #[test]
    fn screening_narrows_the_cohort_to_what_earns_its_place() {
        let champion = evolved_descendant(2.0, 0.5);
        let h = harness(&champion);
        let good = forest(&h.corpus_identity, 0, 0.25);
        let bad = forest(&h.corpus_identity, 1, -0.10);
        write_bundle(
            &h.enhancements.join("bundle.json"),
            vec![good.clone(), bad.clone()],
        );

        // On the screen, only `good` beats the champion; `bad` loses. The
        // authoritative pass should then never be asked about `bad`.
        //
        // The cap is tightened to 2 because the screen now engages only when it
        // can save a corpus pass (Issue #42): this cohort is baseline + bundle +
        // two singles, which the default budget of 8 would have swallowed whole.
        let mut cli = h.cli.clone();
        cli.max_candidates = 2;
        cli.screen_sample_rate = Some(0.5);
        let scorer = ScriptedScorer::flat(0.50)
            .with("single-00", 0.60)
            .with("single-01", 0.40)
            .with("bundle", 0.55);
        assert_eq!(run_with(&cli, Some(&scorer)).unwrap(), EXIT_IMPROVED);

        let s = summary(out(&cli));
        let scored: Vec<&str> = s.candidates.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            scored,
            vec!["baseline", "single-00"],
            "only the surviving enhancement reaches the corpus: {scored:?}"
        );
        let verdict = s.verdict.unwrap();
        assert!(verdict.improved());
        assert_eq!(verdict.winner.unwrap().applied_ids, vec![good.meta.id]);
    }

    #[test]
    fn a_screen_that_kills_everything_publishes_nothing() {
        let champion = evolved_descendant(2.0, 0.5);
        let h = harness(&champion);
        write_bundle(
            &h.enhancements.join("bundle.json"),
            vec![
                forest(&h.corpus_identity, 0, 0.25),
                forest(&h.corpus_identity, 1, -0.10),
            ],
        );
        let mut cli = h.cli.clone();
        // Same tightened cap as above, and for the same reason (Issue #42).
        cli.max_candidates = 2;
        cli.screen_sample_rate = Some(0.5);
        // Everything loses on the screen.
        let scorer = ScriptedScorer::flat(0.50)
            .with("single-00", 0.30)
            .with("single-01", 0.30)
            .with("bundle", 0.30);
        assert_eq!(run_with(&cli, Some(&scorer)).unwrap(), EXIT_NO_IMPROVEMENT);
        assert!(!out(&cli).join("population-candidate.json").exists());
        assert_eq!(summary(out(&cli)).status, "nothingToDo");
    }

    /// Issue #42: the gate is the budget, not the enhancement count.
    #[test]
    fn the_budget_gate_measures_the_cohort_the_cap_hid() {
        let champion = evolved_descendant(2.0, 0.5);
        let h = harness(&champion);
        let enhancements: Vec<Enhancement> = (0..3)
            .map(|i| forest(&h.corpus_identity, i % 2, 0.20 + i as f32 * 0.05))
            .collect();
        let build = |max_candidates| {
            rebase(&RebaseRequest {
                champion: &champion,
                enhancements: &enhancements,
                corpus_identity: &h.corpus_identity,
                max_candidates,
            })
            .unwrap()
        };

        // A cap of 2 hides most of the cohort behind `dropped_for_cap`; the
        // gate has to see the work the authoritative pass would have done.
        let capped = build(2);
        assert!(
            !capped.dropped_for_cap.is_empty(),
            "the cap must bite for this test to mean anything"
        );
        assert_eq!(
            cohort_before_the_cap(&capped),
            build(0).candidates().count(),
            "the gate counts every candidate built, capped or not"
        );
        assert!(cohort_before_the_cap(&capped) > screening_budget(2));
        assert!(
            cohort_before_the_cap(&build(8)) <= screening_budget(8),
            "the default budget swallows this cohort whole: nothing to screen for"
        );
    }

    /// Issue #42: an uncapped run has no budget, so the documented default is
    /// the threshold — a large cohort is still screened.
    #[test]
    fn an_uncapped_run_screens_only_a_cohort_past_the_default_budget() {
        assert_eq!(screening_budget(0), DEFAULT_MAX_CANDIDATES);
        assert_eq!(screening_budget(3), 3);

        let champion = evolved_descendant(2.0, 0.5);
        let h = harness(&champion);
        let uncapped = |enhancements: &[Enhancement]| {
            cohort_before_the_cap(
                &rebase(&RebaseRequest {
                    champion: &champion,
                    enhancements,
                    corpus_identity: &h.corpus_identity,
                    max_candidates: 0,
                })
                .unwrap(),
            )
        };
        let many: Vec<Enhancement> = (0..5)
            .map(|i| forest(&h.corpus_identity, i % 2, 0.20 + i as f32 * 0.05))
            .collect();
        assert!(
            uncapped(&many[..2]) <= screening_budget(0),
            "two enhancements are four creatures: the corpus can afford them"
        );
        assert!(
            uncapped(&many) > screening_budget(0),
            "five enhancements overflow the default budget and are worth screening"
        );
    }

    /// Issue #42: elimination is one-sided. Only a loss the stratum can see
    /// removes a candidate.
    ///
    /// Issue #43 moved the rule into [`ScreenVerdict`] so the journal and the
    /// decision come from one classification; the cases are unchanged, and each
    /// now also names *which* undecided case it is.
    #[test]
    fn only_a_loss_the_stratum_can_see_screens_a_candidate_out() {
        let verdict = |score| ScreenVerdict::classify(score, 0.50, 1e-9);
        assert_eq!(verdict(Some(0.40)), ScreenVerdict::Worse);
        assert!(!verdict(Some(0.40)).survives());
        assert_eq!(
            verdict(Some(0.50)),
            ScreenVerdict::Indistinguishable,
            "an exact tie is the stratum failing to resolve the graft, not a loss"
        );
        assert_eq!(
            verdict(Some(0.50 - 5e-11)),
            ScreenVerdict::Indistinguishable,
            "a difference below the run's own resolution decides nothing"
        );
        assert_eq!(verdict(Some(0.60)), ScreenVerdict::Better);
        assert_eq!(
            verdict(None),
            ScreenVerdict::NotScored,
            "no score is no evidence: the authoritative pass decides"
        );
        for undecided in [Some(0.50), Some(0.50 - 5e-11), Some(0.60), None] {
            assert!(
                verdict(undecided).survives(),
                "only a visible loss eliminates: {undecided:?}"
            );
        }
    }

    /// Issue #43: an enhancement the stratum never saw is reported as such,
    /// rather than disappearing from a survivor count that then reads as a
    /// screen rejecting it.
    #[test]
    fn an_enhancement_no_candidate_was_built_for_is_journalled_not_dropped_silently() {
        let champion = evolved_descendant(2.0, 0.5);
        let built = forest("corpus-1", 0, 0.25);
        let never_built = forest("a-corpus-from-somewhere-else", 1, -0.10);
        let outcome = rebase(&RebaseRequest {
            champion: &champion,
            enhancements: std::slice::from_ref(&built),
            corpus_identity: "corpus-1",
            max_candidates: 0,
        })
        .unwrap();

        let mut scores = BTreeMap::new();
        for label in ["baseline", "single-00"] {
            scores.insert(
                label.to_string(),
                ScoreResult {
                    score: 0.5,
                    error: 0.5,
                    complexity_penalty: 0.0,
                    record_count: 64,
                    sample_rate: Some(0.05),
                    gpu_backend: None,
                    cost_name: None,
                    time_taken: 0.0,
                },
            );
        }
        let offered = vec![built.clone(), never_built.clone()];
        let (measured, survivors) = measure_phase(&outcome, &offered, &scores, 0.5, 1e-9);

        assert_eq!(
            measured.len(),
            2,
            "every enhancement offered is accounted for"
        );
        let missing = measured
            .iter()
            .find(|m| m.id == never_built.meta.id)
            .expect("the unbuilt enhancement is still reported");
        assert_eq!(missing.verdict, ScreenVerdict::NotBuilt);
        assert!(
            missing.delta.is_none(),
            "no delta to claim: it was not scored"
        );
        assert!(!missing.kept);
        let seen = measured
            .iter()
            .find(|m| m.id == built.meta.id)
            .expect("the scored enhancement is reported");
        assert_eq!(seen.verdict, ScreenVerdict::Indistinguishable);
        assert_eq!(seen.delta, Some(0.0));
        assert_eq!(
            survivors
                .iter()
                .map(|e| e.meta.id.clone())
                .collect::<Vec<_>>(),
            vec![built.meta.id],
            "only what the stratum actually saw is carried forward"
        );
    }

    #[test]
    fn help_explains_that_the_champion_must_be_freshly_fetched() {
        let help = Cli::command().render_long_help().to_string();
        assert!(
            help.contains("immediately before"),
            "help must tell the caller to refresh the champion: {help}"
        );
        assert!(help.contains("Exit codes"), "{help}");
    }

    #[test]
    fn cli_parses_the_documented_invocation() {
        let cli = Cli::try_parse_from([
            "neat_ai_rebase",
            "--champion",
            "champion.json",
            "--enhancements",
            "bundle.json",
            "--training-data",
            "training/",
            "--scorer",
            "../NEAT-AI-scorer/target/release/rust_scorer",
            "--output-dir",
            "runs/first",
            "--scorer-arg",
            "--gpu=off",
        ])
        .unwrap();
        assert_eq!(cli.scorer_args, vec!["--gpu=off"]);
        assert_eq!(cli.max_candidates, 8);
        assert!(!cli.dry_run);
        assert!(cli.command.is_none(), "no subcommand: this is a rebase run");
    }

    /// Issue #38: the journals a soak wrote have to be readable back.
    #[test]
    fn report_reads_the_journals_a_run_wrote() {
        // Run twice into the same output directory: one improvement, one
        // rejection. The journal is append-only, so both runs are in one file.
        let champion = evolved_descendant(2.0, 0.5);
        let h = harness(&champion);
        write_bundle(
            &h.enhancements.join("bundle.json"),
            vec![forest(&h.corpus_identity, 1, 0.25)],
        );
        assert_eq!(
            run_with(
                &h.cli,
                Some(&ScriptedScorer::flat(0.50).with("single-00", 0.60))
            )
            .unwrap(),
            EXIT_IMPROVED
        );
        assert_eq!(
            run_with(
                &h.cli,
                Some(&ScriptedScorer::flat(0.80).with("single-00", 0.70))
            )
            .unwrap(),
            EXIT_NO_IMPROVEMENT
        );

        let journal = out(&h.cli).join("experiments.jsonl");
        let report = crate::report::read_one(&journal).unwrap();
        assert_eq!(report.runs, 2);
        assert_eq!(report.runs_by_status.get("improved"), Some(&1));
        assert_eq!(report.runs_by_status.get("noImprovement"), Some(&1));
        assert_eq!(report.runs_with_a_winner, 1);
        assert_eq!(report.best_vs_champion.len(), 2);
        assert_eq!(report.enhancements_by_outcome.get("applied"), Some(&2));

        // And through the subcommand itself, which reads and exits 0.
        let cli = Cli::try_parse_from(["neat_ai_rebase", "report", &journal.display().to_string()])
            .unwrap();
        assert_eq!(
            cli.command,
            Some(Command::Report {
                journals: vec![journal]
            })
        );
        assert_eq!(run_with(&cli, None).unwrap(), EXIT_OK);
    }

    #[test]
    fn report_names_the_journal_it_cannot_read() {
        let cli =
            Cli::try_parse_from(["neat_ai_rebase", "report", "/nonexistent/experiments.jsonl"])
                .unwrap();
        let err = run_with(&cli, None).unwrap_err();
        assert_eq!(err.code, EXIT_FAILURE);
        assert!(err.message.contains("experiments.jsonl"), "{err}");
    }

    #[test]
    fn report_needs_no_champion_and_a_rebase_run_still_does() {
        // The subcommand negates the run's required flags …
        assert!(Cli::try_parse_from(["neat_ai_rebase", "report", "a.jsonl"]).is_ok());
        // … and needs at least one journal.
        assert!(Cli::try_parse_from(["neat_ai_rebase", "report"]).is_err());
        // … while a rebase run still demands every one of them.
        assert!(Cli::try_parse_from(["neat_ai_rebase", "--champion", "c.json"]).is_err());
        let help = Cli::command().render_long_help().to_string();
        assert!(
            help.contains("report"),
            "the subcommand is discoverable: {help}"
        );
    }
}
