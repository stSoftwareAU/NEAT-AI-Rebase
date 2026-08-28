//! The unattended CLI (Issue #6).
//!
//! ```text
//! neat_ai_rebase --champion <file> --enhancements <file-or-dir> \
//!                --training-data <dir> --scorer <path> --output-dir <dir>
//! ```
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

use std::path::{Path, PathBuf};

use clap::Parser;
use neat_core::training_data::TrainingDataConfig;
use neat_core::{CreatureExport, creature_to_json, parse_creature_json};
use serde::{Deserialize, Serialize};

use crate::corpus::{CorpusInfo, corpus_info};
use crate::creature::{sha256_hex, validate_source_creature};
use crate::engine::{EnhancementOutcome, RebaseOutcome, RebaseRequest, rebase};
use crate::enhancement::{Enhancement, EnhancementBundle};
use crate::harvest::harvest_delta;
use crate::journal::{Journal, Record};
use crate::scorer::{DirectoryScorer, ExternalScorer, ScorerMode, Verdict, judge};
use crate::tags::{CreatureMeta, RebaseStamp};

/// Exit code: a verified improvement was emitted.
pub const EXIT_IMPROVED: i32 = 0;
/// Exit code: operational or scorer failure.
pub const EXIT_FAILURE: i32 = 1;
/// Exit code: no improvement, or nothing left to do. A successful outcome.
pub const EXIT_NO_IMPROVEMENT: i32 = 3;
/// Exit code: incompatible input; nothing could be attempted.
pub const EXIT_INCOMPATIBLE: i32 = 4;

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
or scorer failure."
)]
pub struct Cli {
    /// The **freshly fetched** current global champion. Never written to.
    #[arg(long, value_name = "FILE")]
    pub champion: PathBuf,

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
    #[arg(long, value_name = "DIR")]
    pub training_data: PathBuf,

    /// The NEAT-AI-scorer binary (`rust_scorer`). Not required with
    /// `--dry-run`.
    #[arg(long, value_name = "PATH")]
    pub scorer: Option<PathBuf>,

    /// Where `population-candidate.json`, `rebase.json` and
    /// `experiments.jsonl` are written.
    #[arg(long, value_name = "DIR")]
    pub output_dir: PathBuf,

    /// Extra argument passed verbatim to the scorer. Repeatable.
    #[arg(long = "scorer-arg", value_name = "ARG", allow_hyphen_values = true)]
    pub scorer_args: Vec<String>,

    /// Score a candidate must beat the champion by before it is emitted.
    #[arg(long, default_value_t = 1e-9, value_name = "DELTA")]
    pub min_improvement: f64,

    /// Maximum candidates to construct, excluding the baseline. `0` = no cap.
    #[arg(long, default_value_t = 8, value_name = "N")]
    pub max_candidates: usize,

    /// Build and validate candidates without scoring, and without writing a
    /// population candidate.
    #[arg(long)]
    pub dry_run: bool,

    /// Screen each enhancement on a sub-sample before the authoritative pass,
    /// and carry forward only the ones that beat the champion on their own.
    ///
    /// Measured on a live fleet: of one donor's 13 patches, two improved the
    /// champion and eleven made it worse, and every cumulative prefix was
    /// worse than the best single it contained. Scoring the whole cohort on
    /// the full corpus finds the same answer, but a 14-patch cohort is ~28
    /// creatures over the whole corpus; screening first cuts that to a
    /// handful.
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
    std::fs::create_dir_all(&cli.output_dir)
        .map_err(|e| RunError::failure(format!("{}: {e}", cli.output_dir.display())))?;
    let journal = Journal::new(cli.output_dir.join("experiments.jsonl"));

    let (champion, champion_file_checksum, champion_meta) = load_champion(&cli.champion)?;
    let corpus = corpus_info(
        &cli.training_data,
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
            let journal = Journal::new(cli.output_dir.join("experiments.jsonl"));
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
        finish(&cli.output_dir, &journal, &summary, status, None)?;
        return Ok(code);
    }

    if cli.dry_run {
        summary.status = "dryRun".into();
        finish(&cli.output_dir, &journal, &summary, "dryRun", None)?;
        return Ok(EXIT_IMPROVED);
    }

    let scorer = scorer.ok_or_else(|| {
        RunError::failure("--scorer is required unless --dry-run is set".to_string())
    })?;

    // Narrow the cohort before paying for the corpus. Never widens it, and
    // never promotes: `judge` refuses a sampled mode outright.
    let outcome = match cli.screen_sample_rate {
        Some(rate) if rate > 0.0 && rate < 1.0 && enhancements.len() > 1 => screen(
            cli,
            scorer,
            &champion,
            &enhancements,
            &corpus_identity,
            rate,
            &journal,
        )?,
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
        finish(&cli.output_dir, &journal, &summary, "nothingToDo", None)?;
        return Ok(EXIT_NO_IMPROVEMENT);
    }

    let verdict = judge(
        scorer,
        &outcome,
        &cli.training_data,
        &cli.output_dir.join("scoring"),
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
            let path = cli.output_dir.join("population-candidate.json");
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
    finish(&cli.output_dir, &journal, &summary, status, emitted)?;
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

/// Narrow the cohort to the enhancements that earn their place, on a sample.
///
/// Two strata, not one. Selecting on a stratum and trusting that selection is
/// circular — with N candidates some beat the champion on any given stratum by
/// chance, and "keep the winners" picks exactly those. Re-screening the
/// survivors on different records drops most of those accidents before they
/// reach the corpus.
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
        let staging = cli.output_dir.join(format!("screen-{phase}"));
        std::fs::create_dir_all(&staging)
            .map_err(|e| RunError::failure(format!("{}: {e}", staging.display())))?;
        crate::scorer::stage(&outcome, &staging).map_err(|e| RunError::failure(e.to_string()))?;
        let scores = scorer
            .score_directory(
                &staging,
                &cli.training_data,
                ScorerMode::Sample { rate, phase },
            )
            .map_err(|e| RunError::failure(e.to_string()))?;
        let baseline = scores
            .get(crate::engine::BASELINE_LABEL)
            .ok_or_else(|| RunError::failure("screen produced no baseline"))?
            .score;
        let survivors: Vec<Enhancement> = outcome
            .candidates()
            .filter(|c| c.applied_ids.len() == 1)
            .filter(|c| scores.get(&c.label).is_some_and(|r| r.score > baseline))
            .filter_map(|c| kept.iter().find(|e| e.meta.id == c.applied_ids[0]).cloned())
            .collect();
        let _ = journal.append(&Record::Dropped {
            label: format!("screen-phase-{phase}"),
            reason: format!(
                "{} of {} enhancements beat the champion alone",
                survivors.len(),
                kept.len()
            ),
        });
        eprintln!(
            "neat_ai_rebase: screen phase {phase} kept {} of {}",
            survivors.len(),
            kept.len()
        );
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
                champion: champion_path,
                enhancements: Some(enhancements.clone()),
                harvest_from: None,
                screen_sample_rate: None,
                screen_held_out: true,
                training_data: training,
                scorer: None,
                output_dir: tmp.path().join("out"),
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

        let emitted = h.cli.output_dir.join("population-candidate.json");
        assert!(emitted.exists());
        let s = summary(&h.cli.output_dir);
        assert_eq!(s.status, "improved");
        assert_eq!(
            s.emitted_checksum.unwrap(),
            sha256_hex(std::fs::read_to_string(&emitted).unwrap().as_bytes())
        );
        let verdict = s.verdict.unwrap();
        assert!(verdict.improved());
        assert!((verdict.baseline.score - 0.50).abs() < 1e-12);
        assert!(
            h.cli.output_dir.join("experiments.jsonl").exists(),
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
        tag_champion_file(&h.cli.champion);
        write_bundle(
            &h.enhancements.join("bundle.json"),
            vec![forest(&h.corpus_identity, 1, 0.25)],
        );

        let scorer = ScriptedScorer::flat(0.50).with("single-00", 0.60);
        assert_eq!(run_with(&h.cli, Some(&scorer)).unwrap(), EXIT_IMPROVED);

        let text =
            std::fs::read_to_string(h.cli.output_dir.join("population-candidate.json")).unwrap();
        let meta = CreatureMeta::from_json(&text);

        // The score is the one the judge measured, and it has replaced the
        // champion's stale tag rather than sitting beside it.
        let winner = summary(&h.cli.output_dir)
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
        assert!(!h.cli.output_dir.join("population-candidate.json").exists());
        assert_eq!(summary(&h.cli.output_dir).status, "noImprovement");
        // The champion file is untouched.
        let champion_now = std::fs::read_to_string(&h.cli.champion).unwrap();
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
        assert!(!h.cli.output_dir.join("population-candidate.json").exists());
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
        assert!(!cli.output_dir.join("population-candidate.json").exists());
        let s = summary(&cli.output_dir);
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
        assert_eq!(summary(&h.cli.output_dir).status, "incompatible");
        assert!(!h.cli.output_dir.join("population-candidate.json").exists());
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
        std::fs::write(&h.cli.champion, creature_to_json(&grafted).unwrap()).unwrap();
        write_bundle(&h.enhancements.join("bundle.json"), vec![e]);

        let code = run_with(&h.cli, Some(&ScriptedScorer::flat(0.5))).unwrap();
        assert_eq!(code, EXIT_NO_IMPROVEMENT);
        assert_eq!(summary(&h.cli.output_dir).status, "nothingToDo");
        assert!(!h.cli.output_dir.join("population-candidate.json").exists());
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
        let s = summary(&cli.output_dir);
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

        let s = summary(&cli.output_dir);
        assert_eq!(s.status, "improved");
        assert_eq!(
            s.enhancements.len(),
            1,
            "one patch harvested from the creature"
        );
        assert!(s.enhancements[0].producer.starts_with("harvest/"), "{s:?}");
        assert!(cli.output_dir.join("population-candidate.json").exists());
    }

    #[test]
    fn harvest_from_a_creature_with_nothing_new_is_nothing_to_do() {
        // Harvesting the champion from itself: no patch it lacks.
        let champion = linear_hidden_creature(2.0);
        let h = harness(&champion);
        let mut cli = h.cli.clone();
        cli.enhancements = None;
        cli.harvest_from = Some(cli.champion.clone());
        let code = run_with(&cli, Some(&ScriptedScorer::flat(0.5))).unwrap();
        assert_eq!(
            code, EXIT_NO_IMPROVEMENT,
            "a harvest with no delta is normal"
        );
        assert!(!cli.output_dir.join("population-candidate.json").exists());
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
        let mut cli = h.cli.clone();
        cli.screen_sample_rate = Some(0.5);
        let scorer = ScriptedScorer::flat(0.50)
            .with("single-00", 0.60)
            .with("single-01", 0.40)
            .with("bundle", 0.55);
        assert_eq!(run_with(&cli, Some(&scorer)).unwrap(), EXIT_IMPROVED);

        let s = summary(&cli.output_dir);
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
        cli.screen_sample_rate = Some(0.5);
        // Everything loses on the screen.
        let scorer = ScriptedScorer::flat(0.50)
            .with("single-00", 0.30)
            .with("single-01", 0.30)
            .with("bundle", 0.30);
        assert_eq!(run_with(&cli, Some(&scorer)).unwrap(), EXIT_NO_IMPROVEMENT);
        assert!(!cli.output_dir.join("population-candidate.json").exists());
        assert_eq!(summary(&cli.output_dir).status, "nothingToDo");
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
    }
}
