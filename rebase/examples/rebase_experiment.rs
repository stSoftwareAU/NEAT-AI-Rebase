//! Overnight experiment harness: does rebasing a Forests creature's discoveries
//! onto the concurrently-evolved champion beat publishing the Forests creature
//! itself?
//!
//! ```text
//! cargo run --release --example rebase_experiment -- \
//!     --forest <F.json> --champion <B.json> --training <dir> --scorer <bin> \
//!     --out <dir> [--sample-rate R] [--max-candidates N]
//! ```
//!
//! The question it answers is the fleet's, not the library's: a Forests run
//! publishes `F` and the score jumps, but `F` descends from an hour-old
//! ancestor and silently drops whatever Lamarck, Ockham and backprop achieved
//! meanwhile. `B` is the creature carrying that work. If `B + Δ` — the champion
//! with Forests' discoveries grafted on — beats `F`, then publishing `F` was a
//! net loss and the rebase is worth wiring in.
//!
//! Everything is scored in **one** scorer call so the numbers are comparable,
//! `F` included: it is the thing being beaten, so it has to be in the same
//! pass, not compared against a score tag written by another host on another
//! day.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use neat_ai_rebase::corpus::corpus_info;
use neat_ai_rebase::creature::{creature_checksum, validate_source_creature};
use neat_ai_rebase::engine::{EnhancementOutcome, RebaseRequest, rebase};
use neat_ai_rebase::harvest::{harvest_delta, patch_ids};
use neat_ai_rebase::scorer::{DirectoryScorer, ExternalScorer, ScoreResult, ScorerMode};
use neat_ai_rebase::tags::{CreatureMeta, RebaseStamp};
use neat_core::training_data::TrainingDataConfig;
use neat_core::{CreatureExport, creature_to_json, parse_creature_json};

/// Stem the champion is scored under; also the scorer's reserved baseline name.
const CHAMPION: &str = "baseline";
/// Stem the Forests creature is scored under — the thing we have to beat.
const FOREST: &str = "forest";

struct Args {
    forest: PathBuf,
    champion: PathBuf,
    training: PathBuf,
    scorer: PathBuf,
    out: PathBuf,
    sample_rate: Option<f64>,
    max_candidates: usize,
    /// Where to write the winning creature, tags and all. Only written on an
    /// authoritative pass where the winner beats BOTH parents.
    emit: Option<PathBuf>,
    /// Restrict the rebase to these enhancement ids.
    ///
    /// Measured on the fleet: of 13 patches a divergent donor carried, two
    /// improved the champion and eleven made it worse, and every cumulative
    /// prefix was worse than the singles. A blind bundle is the wrong default
    /// for a distant donor. Screen the singles, then come back with `--only`
    /// naming the ones that actually won and let the scorer judge their
    /// combination.
    only: Option<Vec<String>>,
    /// Which stratum a sampled screen takes.
    ///
    /// Selecting enhancements on one phase and confirming them on the SAME
    /// phase is circular — it harvests that stratum's noise. Screening on
    /// phase 0 and re-screening the survivors on phase 1 keeps most of the
    /// accidents out of the expensive authoritative pass.
    sample_phase: u64,
}

fn parse_args() -> Args {
    let mut forest = None;
    let mut champion = None;
    let mut training = None;
    let mut scorer = None;
    let mut out = None;
    let mut sample_rate = None;
    let mut max_candidates = 6usize;
    let mut emit = None;
    let mut only: Option<Vec<String>> = None;
    let mut sample_phase: u64 = 0;
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let flag = argv[i].clone();
        let value = argv.get(i + 1).cloned().unwrap_or_default();
        match flag.as_str() {
            "--forest" => forest = Some(PathBuf::from(value)),
            "--champion" => champion = Some(PathBuf::from(value)),
            "--training" => training = Some(PathBuf::from(value)),
            "--scorer" => scorer = Some(PathBuf::from(value)),
            "--out" => out = Some(PathBuf::from(value)),
            "--sample-rate" => sample_rate = value.parse().ok(),
            "--sample-phase" => sample_phase = value.parse().unwrap_or(0),
            "--max-candidates" => max_candidates = value.parse().unwrap_or(6),
            "--emit" => emit = Some(PathBuf::from(value)),
            "--only" => {
                only = Some(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect(),
                )
            }
            other => {
                eprintln!("unknown argument `{other}`");
                std::process::exit(2);
            }
        }
        i += 2;
    }
    let missing = |name: &str| -> ! {
        eprintln!("missing required argument {name}");
        std::process::exit(2);
    };
    Args {
        forest: forest.unwrap_or_else(|| missing("--forest")),
        champion: champion.unwrap_or_else(|| missing("--champion")),
        training: training.unwrap_or_else(|| missing("--training")),
        scorer: scorer.unwrap_or_else(|| missing("--scorer")),
        out: out.unwrap_or_else(|| missing("--out")),
        sample_rate,
        max_candidates,
        emit,
        only,
        sample_phase,
    }
}

fn load(path: &Path) -> Result<CreatureExport, Box<dyn std::error::Error>> {
    let creature = parse_creature_json(&std::fs::read_to_string(path)?)?;
    validate_source_creature(&creature).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(creature)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args();
    std::fs::create_dir_all(&args.out)?;

    let forest = load(&args.forest)?;
    let champion = load(&args.champion)?;
    println!("forest   {}", args.forest.display());
    println!(
        "         {} neurons, {} synapses, {} patches, checksum {}",
        forest.neurons.len(),
        forest.synapses.len(),
        patch_ids(&forest).len(),
        &creature_checksum(&forest)?[..16]
    );
    println!("champion {}", args.champion.display());
    println!(
        "         {} neurons, {} synapses, {} patches, checksum {}",
        champion.neurons.len(),
        champion.synapses.len(),
        patch_ids(&champion).len(),
        &creature_checksum(&champion)?[..16]
    );

    let corpus = corpus_info(
        &args.training,
        &TrainingDataConfig::new(champion.input, champion.output),
    )?;
    println!(
        "corpus   {} · {} records in {} files",
        corpus.identity, corpus.record_count, corpus.file_count
    );

    // Δ: what the Forests creature discovered that the champion never saw.
    let harvest = harvest_delta(&forest, &champion);
    println!(
        "\nharvest  {} recovered, {} skipped",
        harvest.patches.len(),
        harvest.skipped.len()
    );
    for skip in &harvest.skipped {
        println!("  SKIP {} — {}", skip.id, skip.reason);
    }
    if harvest.patches.is_empty() {
        println!("\nnothing to rebase — the champion already carries every recoverable patch");
        return Ok(());
    }
    for p in &harvest.patches {
        println!(
            "  Δ {} output-{} depth {} splits {}",
            p.id(),
            p.output,
            p.root.depth(),
            p.root.split_count()
        );
    }

    let mut enhancements =
        harvest.into_enhancements(&forest, &corpus.identity, "harvest/rebase-experiment", 0.0)?;
    for e in &mut enhancements {
        e.meta.improved_score = 0.0;
    }

    if let Some(only) = &args.only {
        let before = enhancements.len();
        enhancements.retain(|e| only.contains(&e.meta.id));
        println!(
            "\nrestricted to {} of {before} enhancements by --only",
            enhancements.len()
        );
        if enhancements.is_empty() {
            println!("none of the requested ids are in the delta");
            return Ok(());
        }
    }

    let outcome = rebase(&RebaseRequest {
        champion: &champion,
        enhancements: &enhancements,
        corpus_identity: &corpus.identity,
        max_candidates: args.max_candidates,
    })?;
    for report in &outcome.reports {
        if let EnhancementOutcome::Incompatible(reason) = &report.outcome {
            println!("  !! {} incompatible: {reason}", report.id);
        }
    }
    for failure in &outcome.combination_failures {
        println!("  !! {failure}");
    }
    println!(
        "\ncohort   {} candidates (+ baseline){}",
        outcome.candidates().count(),
        if outcome.dropped_for_cap.is_empty() {
            String::new()
        } else {
            format!(", {} dropped for the cap", outcome.dropped_for_cap.len())
        }
    );

    // Stage the champion, the Forests creature and every candidate together.
    let staging = args.out.join("scoring");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    for candidate in &outcome.cohort {
        std::fs::write(
            staging.join(format!("{}.json", candidate.label)),
            creature_to_json(&candidate.creature)?,
        )?;
    }
    std::fs::write(
        staging.join(format!("{FOREST}.json")),
        creature_to_json(&forest)?,
    )?;

    // Every candidate has to be a creature the fleet would accept, checked
    // before it is scored rather than after it might have won.
    for candidate in outcome.candidates() {
        validate_source_creature(&candidate.creature).map_err(|e| {
            format!(
                "candidate `{}` failed NEAT-AI-core validation: {e}",
                candidate.label
            )
        })?;
    }
    println!("         every candidate passes neat_core::creature_validate");

    let mode = match args.sample_rate {
        Some(rate) if rate < 1.0 => ScorerMode::Sample {
            rate,
            phase: args.sample_phase,
        },
        _ => ScorerMode::Full,
    };
    println!(
        "\nscoring  {} creatures in one {} pass …",
        outcome.cohort.len() + 1,
        mode.label()
    );
    let started = std::time::Instant::now();
    let scorer = ExternalScorer::with_args(&args.scorer, vec!["--gpu=off".into()]);
    let results = scorer.score_directory(&staging, &args.training, mode)?;
    println!("         took {:.1}s", started.elapsed().as_secs_f64());

    let winner = report(&outcome, &results, &args.out, mode)?;

    // Emission is deliberately narrow: only an authoritative pass, only a
    // candidate that beat BOTH parents, and only with the champion's own tags
    // carried across. A creature that arrives in the population with a better
    // score and no provenance is refused by the check-in guard, and should be.
    if let (Some(path), Some(winner)) = (&args.emit, winner) {
        if !mode.is_authoritative() {
            println!("         --emit ignored: a sampled screen may not promote anything");
        } else {
            let candidate = outcome
                .cohort
                .iter()
                .find(|c| c.label == winner.label)
                .ok_or("winner left the cohort")?;
            let mut meta = CreatureMeta::from_json(&std::fs::read_to_string(&args.champion)?);
            meta.retain_neurons(&candidate.creature);
            let scored = get(&results, &winner.label)?;
            meta.stamp(&RebaseStamp {
                score: scored.score,
                error: scored.error,
                champion_score: get(&results, CHAMPION)?.score,
                source_score: get(&results, FOREST)?.score,
                applied: candidate.applied_ids.len(),
                label: &candidate.label,
                source: "harvest",
            });
            std::fs::write(path, meta.serialize_with(&candidate.creature, true)?)?;
            println!("         emitted {}", path.display());
            println!(
                "         rebase tag: {}",
                meta.get("rebase").unwrap_or_default()
            );
        }
    }
    Ok(())
}

/// The candidate that won, when one beat both parents.
struct Winner {
    label: String,
}

fn get<'a>(
    results: &'a BTreeMap<String, ScoreResult>,
    stem: &str,
) -> Result<&'a ScoreResult, String> {
    results
        .get(stem)
        .ok_or_else(|| format!("scorer returned no entry for `{stem}`"))
}

fn report(
    outcome: &neat_ai_rebase::engine::RebaseOutcome,
    results: &BTreeMap<String, ScoreResult>,
    out: &Path,
    mode: ScorerMode,
) -> Result<Option<Winner>, Box<dyn std::error::Error>> {
    let champion = get(results, CHAMPION)?;
    let forest = get(results, FOREST)?;

    println!(
        "\n{:<14}{:>20}{:>16}{:>16}",
        "stem", "score", "vs champion", "vs forest"
    );
    println!(
        "{:<14}{:>20.12}{:>16}{:>16.3e}",
        "champion",
        champion.score,
        "—",
        champion.score - forest.score
    );
    println!(
        "{:<14}{:>20.12}{:>16.3e}{:>16}",
        "forest",
        forest.score,
        forest.score - champion.score,
        "—"
    );

    let mut rows: Vec<(&str, f64, f64, f64)> = Vec::new();
    for candidate in outcome.candidates() {
        let r = get(results, &candidate.label)?;
        rows.push((
            candidate.label.as_str(),
            r.score,
            r.score - champion.score,
            r.score - forest.score,
        ));
    }
    rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (label, score, vs_champ, vs_forest) in &rows {
        println!("{label:<14}{score:>20.12}{vs_champ:>16.3e}{vs_forest:>16.3e}");
    }

    let best = rows.first();
    let mut winner = None;
    println!();
    match best {
        Some((label, score, vs_champ, vs_forest)) if *vs_forest > 0.0 && *vs_champ > 0.0 => {
            println!(
                "RESULT   rebase WINS: `{label}` at {score:.12} beats the forest creature by {vs_forest:.3e} \
                 and the champion by {vs_champ:.3e}"
            );
            winner = Some(Winner {
                label: (*label).to_string(),
            });
        }
        Some((label, score, vs_champ, vs_forest)) if *vs_champ > 0.0 => {
            println!(
                "RESULT   rebase improves the champion (`{label}` {score:.12}, +{vs_champ:.3e}) but does \
                 not beat the forest creature ({vs_forest:.3e})"
            );
        }
        Some((label, score, vs_champ, _)) => {
            println!(
                "RESULT   no improvement: best candidate `{label}` {score:.12} ({vs_champ:.3e} vs champion)"
            );
        }
        None => println!("RESULT   no candidates were built"),
    }
    if !mode.is_authoritative() {
        println!("         (SAMPLED — indicative only, not a promotion decision)");
    }

    let summary = serde_json::json!({
        "mode": mode.label(),
        "authoritative": mode.is_authoritative(),
        "championChecksum": outcome.champion_checksum,
        "scores": results,
        "candidates": outcome.cohort.iter().map(|c| serde_json::json!({
            "label": c.label,
            "checksum": c.checksum,
            "appliedIds": c.applied_ids,
        })).collect::<Vec<_>>(),
    });
    let path = out.join("experiment.json");
    std::fs::write(&path, serde_json::to_string_pretty(&summary)?)?;
    println!("         wrote {}", path.display());
    Ok(winner)
}
