//! Consolidate a whole population: graft every scorer-verified discovery the
//! fittest creature is missing back onto it.
//!
//! ```text
//! cargo run --release --example union_experiment -- \
//!     --samples <dir> --training <dir> --scorer <bin> --out <dir> \
//!     [--base <file>] [--sample-rate R] [--max-candidates N] [--emit <path>]
//! ```
//!
//! ## Why this exists
//!
//! Measured on the live fleet: the population held 1037 distinct Forest patch
//! ids, and the fittest creature carried 947. Ninety discoveries — each one
//! searched for, grafted and confirmed by a full-corpus scorer on some host —
//! were sitting in creatures that then lost the fitness race, and were
//! therefore never going to reach the champion.
//!
//! That is the monoculture cost with a number on it. Every publish keeps one
//! lineage and discards the rest, so the population converges on whichever
//! process finished last rather than on the union of what everybody found.
//!
//! This is the other direction from `rebase_experiment`, which rebases one
//! run's discoveries at re-entry. Here the run is over, the discoveries are
//! scattered, and the question is whether they can be collected.
//!
//! ## What it does not assume
//!
//! That collecting them helps. Ninety patches is a lot of structure, and the
//! scorer prices complexity; the bundle may well lose to the creature it was
//! built from. The cohort is ordered so the full bundle is scored first and a
//! tight `--max-candidates` keeps it, but the verdict is the scorer's.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use neat_ai_rebase::corpus::corpus_info;
use neat_ai_rebase::creature::{creature_checksum, validate_source_creature};
use neat_ai_rebase::engine::{EnhancementOutcome, RebaseRequest, rebase};
use neat_ai_rebase::enhancement::Enhancement;
use neat_ai_rebase::harvest::{harvest_selected, patch_ids};
use neat_ai_rebase::scorer::{DirectoryScorer, ExternalScorer, ScoreResult, ScorerMode};
use neat_ai_rebase::tags::{CreatureMeta, RebaseStamp};
use neat_core::training_data::TrainingDataConfig;
use neat_core::{CreatureExport, creature_to_json, parse_creature_json};

const BASELINE: &str = "baseline";
/// Below this the creature is a different architecture entirely, not a rival.
const MIN_NEURONS: usize = 100;

struct Sample {
    path: PathBuf,
    score: f64,
    creature: CreatureExport,
    patches: BTreeSet<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut samples_dir = None;
    let mut training = None;
    let mut scorer_bin = None;
    let mut out = None;
    let mut base_path: Option<PathBuf> = None;
    let mut sample_rate: Option<f64> = None;
    let mut max_candidates = 1usize;
    let mut emit: Option<PathBuf> = None;
    // How far below the base a donor may score and still be worth harvesting.
    let mut donor_window: Option<f64> = None;
    let mut max_donors: Option<usize> = None;
    // Screen each stranded patch ALONE, in batches, instead of bundling them.
    // Blind bundling loses; the question this answers is whether any
    // individual stranded discovery still helps the champion.
    let mut batch: Option<usize> = None;
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let value = argv.get(i + 1).cloned().unwrap_or_default();
        match argv[i].as_str() {
            "--samples" => samples_dir = Some(PathBuf::from(value)),
            "--training" => training = Some(PathBuf::from(value)),
            "--scorer" => scorer_bin = Some(PathBuf::from(value)),
            "--out" => out = Some(PathBuf::from(value)),
            "--base" => base_path = Some(PathBuf::from(value)),
            "--sample-rate" => sample_rate = value.parse().ok(),
            "--max-candidates" => max_candidates = value.parse().unwrap_or(1),
            "--emit" => emit = Some(PathBuf::from(value)),
            "--donor-window" => donor_window = value.parse().ok(),
            "--max-donors" => max_donors = value.parse().ok(),
            "--batch" => batch = value.parse().ok(),
            other => {
                eprintln!("unknown argument `{other}`");
                std::process::exit(2);
            }
        }
        i += 2;
    }
    let samples_dir = samples_dir.ok_or("--samples is required")?;
    let training = training.ok_or("--training is required")?;
    let scorer_bin = scorer_bin.ok_or("--scorer is required")?;
    let out = out.ok_or("--out is required")?;
    std::fs::create_dir_all(&out)?;

    // 1. Everything the population currently holds.
    let mut samples = read_samples(&samples_dir)?;
    samples.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    println!("population {} scored creatures", samples.len());

    let base_index = match &base_path {
        Some(p) => samples
            .iter()
            .position(|s| &s.path == p)
            .ok_or_else(|| format!("--base {} is not a scored sample", p.display()))?,
        None => 0,
    };
    let base = &samples[base_index];
    println!(
        "base       {} · tag score {:.12} · {} neurons · {} patches",
        base.path.file_name().unwrap_or_default().to_string_lossy(),
        base.score,
        base.creature.neurons.len(),
        base.patches.len()
    );

    // 2. What the population knows that the base does not.
    let union: BTreeSet<String> = samples
        .iter()
        .flat_map(|s| s.patches.iter().cloned())
        .collect();
    let missing: Vec<String> = union.difference(&base.patches).cloned().collect();
    println!(
        "union      {} distinct patches across the population · base is missing {}",
        union.len(),
        missing.len()
    );
    if missing.is_empty() {
        println!("\nnothing stranded — the base already carries every discovery the fleet holds");
        return Ok(());
    }

    let corpus = corpus_info(
        &training,
        &TrainingDataConfig::new(base.creature.input, base.creature.output),
    )?;
    println!(
        "corpus     {} · {} records",
        corpus.identity, corpus.record_count
    );

    // 3. Take each stranded patch from the fittest creature that still has it:
    //    the reconstruction is verified against the id either way, but a
    //    higher-scoring donor is the one whose structure survived longest.
    // Donor quality is not a detail. Screened on the live fleet, harvesting
    // every stranded patch — donors scoring as low as 0.3686 against a 0.3966
    // base — cost 6.3e-3: 207 neurons of corrections computed against residuals
    // the champion no longer has. A patch is evidence about the creature that
    // found it, and that evidence goes stale.
    let cutoff = donor_window.map(|w| base.score - w);
    if let Some(cutoff) = cutoff {
        println!(
            "donors     only those scoring above {cutoff:.12} (base − {:.1e})",
            donor_window.unwrap_or(0.0)
        );
    }
    if let Some(n) = max_donors {
        println!("donors     at most the {n} highest-scoring");
    }
    let mut wanted: BTreeSet<&str> = missing.iter().map(String::as_str).collect();
    let mut enhancements: Vec<Enhancement> = Vec::new();
    let mut skipped = 0usize;
    let mut donors_used = 0usize;
    for sample in &samples {
        if let Some(cutoff) = cutoff
            && sample.score < cutoff
        {
            continue;
        }
        if let Some(limit) = max_donors
            && donors_used >= limit
        {
            break;
        }
        if wanted.is_empty() {
            break;
        }
        let here: Vec<&str> = wanted
            .iter()
            .copied()
            .filter(|id| sample.patches.contains(*id))
            .collect();
        if here.is_empty() {
            continue;
        }
        let harvest = harvest_selected(&sample.creature, here.iter().copied());
        for skip in &harvest.skipped {
            eprintln!(
                "  SKIP {} from {} — {}",
                skip.id,
                name(&sample.path),
                skip.reason
            );
            skipped += 1;
            wanted.remove(skip.id.as_str());
        }
        if harvest.patches.is_empty() {
            continue;
        }
        println!(
            "  +{:<3} from {:<30} (tag score {:.9})",
            harvest.patches.len(),
            name(&sample.path),
            sample.score
        );
        for p in &harvest.patches {
            wanted.remove(p.id().as_str());
        }
        donors_used += 1;
        enhancements.extend(harvest.into_enhancements(
            &sample.creature,
            &corpus.identity,
            "union/rebase-experiment",
            0.0,
        )?);
    }
    if cutoff.is_some() || max_donors.is_some() {
        println!(
            "           {} patches left behind by the donor filter",
            wanted.len()
        );
    } else {
        for id in &wanted {
            eprintln!("  SKIP {id} — no donor could reconstruct it");
            skipped += 1;
        }
    }
    println!(
        "\nharvested  {} of {} stranded patches ({} unrecoverable)",
        enhancements.len(),
        missing.len(),
        skipped
    );
    if enhancements.is_empty() {
        return Ok(());
    }
    for e in &mut enhancements {
        e.meta.improved_score = 0.0;
    }

    // 4a. Per-patch screening. One creature per patch is far too much JSON to
    //     stage at once for 90 patches (~550 MB, and the scorer parses all of
    //     it), so screen in batches and keep only what wins on its own.
    if let Some(size) = batch {
        let mode = match sample_rate {
            Some(rate) if rate < 1.0 => ScorerMode::Sample { rate, phase: 0 },
            _ => ScorerMode::Full,
        };
        let scorer = ExternalScorer::with_args(&scorer_bin, vec!["--gpu=off".into()]);
        println!(
            "\nscreening  {} patches individually in batches of {size}, {} pass",
            enhancements.len(),
            mode.label()
        );
        let mut winners: Vec<(f64, String)> = Vec::new();
        let mut screened = 0usize;
        for (batch_index, chunk) in enhancements.chunks(size).enumerate() {
            let staging = out.join(format!("screen-{batch_index:03}"));
            if staging.exists() {
                std::fs::remove_dir_all(&staging)?;
            }
            std::fs::create_dir_all(&staging)?;
            // One cohort per patch: a single-enhancement rebase has exactly one
            // candidate, which is the patch on its own.
            let mut labels: Vec<(String, String)> = Vec::new();
            std::fs::write(
                staging.join(format!("{BASELINE}.json")),
                creature_to_json(&base.creature)?,
            )?;
            for (i, e) in chunk.iter().enumerate() {
                let one = rebase(&RebaseRequest {
                    champion: &base.creature,
                    enhancements: std::slice::from_ref(e),
                    corpus_identity: &corpus.identity,
                    max_candidates: 1,
                })?;
                let Some(candidate) = one.candidates().next() else {
                    continue;
                };
                let label = format!("p{i:03}");
                std::fs::write(
                    staging.join(format!("{label}.json")),
                    creature_to_json(&candidate.creature)?,
                )?;
                labels.push((label, e.meta.id.clone()));
            }
            if labels.is_empty() {
                continue;
            }
            let results = scorer.score_directory(&staging, &training, mode)?;
            let b = results.get(BASELINE).ok_or("no baseline in batch")?.score;
            let mut found = 0usize;
            for (label, id) in &labels {
                if let Some(r) = results.get(label)
                    && r.score > b
                {
                    winners.push((r.score - b, id.clone()));
                    found += 1;
                }
            }
            screened += labels.len();
            println!(
                "  batch {batch_index:>2}: {:>3} screened, {found} beat the base",
                labels.len()
            );
            // Staging for a batch is ~120 MB; do not keep 5 of them around.
            let _ = std::fs::remove_dir_all(&staging);
        }
        winners.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        println!(
            "\nRESULT     {} of {screened} stranded patches improve the champion on their own",
            winners.len()
        );
        for (delta, id) in winners.iter().take(20) {
            println!("             {id}  {delta:+.3e}");
        }
        if !winners.is_empty() {
            println!(
                "\n--only {}",
                winners
                    .iter()
                    .map(|(_, id)| id.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            );
        }
        let path = out.join("screen.json");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({
                "experiment": "population-union-singles",
                "base": name(&base.path),
                "screened": screened,
                "winners": winners.iter().map(|(d, id)| serde_json::json!({"id": id, "delta": d})).collect::<Vec<_>>(),
            }))?,
        )?;
        println!("           wrote {}", path.display());
        return Ok(());
    }

    // 4b. Build the cohort. `bundle` is ordered first, so a tight cap keeps the
    //     consolidation and drops the diagnostic singles.
    let outcome = rebase(&RebaseRequest {
        champion: &base.creature,
        enhancements: &enhancements,
        corpus_identity: &corpus.identity,
        max_candidates,
    })?;
    let incompatible = outcome
        .reports
        .iter()
        .filter(|r| matches!(r.outcome, EnhancementOutcome::Incompatible(_)))
        .count();
    if incompatible > 0 {
        println!("           {incompatible} enhancements were incompatible with the base:");
        for r in &outcome.reports {
            if let EnhancementOutcome::Incompatible(reason) = &r.outcome {
                println!("             {} — {reason}", r.id);
            }
        }
    }
    for failure in &outcome.combination_failures {
        println!("           !! {failure}");
    }
    println!(
        "cohort     {} candidates (+ baseline){}",
        outcome.candidates().count(),
        if outcome.dropped_for_cap.is_empty() {
            String::new()
        } else {
            format!(", {} dropped for the cap", outcome.dropped_for_cap.len())
        }
    );
    for c in outcome.candidates() {
        println!(
            "             {:<12} {} patches, {} neurons",
            c.label,
            c.applied_ids.len(),
            c.creature.neurons.len()
        );
    }

    let staging = out.join("scoring");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    for candidate in &outcome.cohort {
        validate_source_creature(&candidate.creature)
            .map_err(|e| format!("`{}` failed NEAT-AI-core validation: {e}", candidate.label))?;
        std::fs::write(
            staging.join(format!("{}.json", candidate.label)),
            creature_to_json(&candidate.creature)?,
        )?;
    }
    println!("           every candidate passes neat_core::creature_validate");

    let mode = match sample_rate {
        Some(rate) if rate < 1.0 => ScorerMode::Sample { rate, phase: 0 },
        _ => ScorerMode::Full,
    };
    println!(
        "\nscoring    {} creatures, {} pass …",
        outcome.cohort.len(),
        mode.label()
    );
    let started = std::time::Instant::now();
    let scorer = ExternalScorer::with_args(&scorer_bin, vec!["--gpu=off".into()]);
    let results = scorer.score_directory(&staging, &training, mode)?;
    println!("           took {:.1}s", started.elapsed().as_secs_f64());

    let baseline = results.get(BASELINE).ok_or("no baseline result")?;
    println!(
        "\n{:<14}{:>20}{:>16}{:>10}",
        "stem", "score", "vs base", "patches"
    );
    println!(
        "{:<14}{:>20.12}{:>16}{:>10}",
        "base",
        baseline.score,
        "—",
        base.patches.len()
    );
    let mut rows: Vec<(&str, f64, usize)> = Vec::new();
    for c in outcome.candidates() {
        if let Some(r) = results.get(&c.label) {
            rows.push((c.label.as_str(), r.score, c.applied_ids.len()));
        }
    }
    rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (label, score, n) in &rows {
        println!(
            "{label:<14}{score:>20.12}{:>16.3e}{n:>10}",
            score - baseline.score
        );
    }

    println!();
    let winner = rows.first().filter(|(_, s, _)| *s > baseline.score);
    match winner {
        Some((label, score, n)) => println!(
            "RESULT     consolidation WINS: `{label}` at {score:.12} (+{:.3e}) carrying {n} recovered discoveries",
            score - baseline.score
        ),
        None => println!(
            "RESULT     no gain: the stranded discoveries do not pay for their complexity on this base"
        ),
    }
    if !mode.is_authoritative() {
        println!("           (SAMPLED — indicative only)");
    }

    write_summary(&out, &outcome, &results, mode, base, &missing)?;

    if let (Some(path), Some((label, score, n))) = (&emit, winner) {
        if mode.is_authoritative() {
            let candidate = outcome
                .cohort
                .iter()
                .find(|c| &c.label == label)
                .ok_or("winner left the cohort")?;
            let mut meta = CreatureMeta::from_json(&std::fs::read_to_string(&base.path)?);
            meta.retain_neurons(&candidate.creature);
            let scored = results.get(*label).ok_or("winner has no result")?;
            meta.stamp(&RebaseStamp {
                score: scored.score,
                error: scored.error,
                champion_score: baseline.score,
                source_score: baseline.score,
                applied: *n,
                label,
                source: "population-union",
            });
            std::fs::write(path, meta.serialize_with(&candidate.creature, true)?)?;
            println!("           emitted {} at {score:.12}", path.display());
        } else {
            println!("           --emit ignored: a sampled screen may not promote anything");
        }
    }
    Ok(())
}

fn name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn read_samples(dir: &Path) -> Result<Vec<Sample>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    paths.sort();
    for path in paths {
        let text = std::fs::read_to_string(&path)?;
        let Some(score) = CreatureMeta::from_json(&text).score() else {
            continue;
        };
        let Ok(creature) = parse_creature_json(&text) else {
            eprintln!("  skip {} — does not parse", name(&path));
            continue;
        };
        if creature.neurons.len() < MIN_NEURONS {
            continue;
        }
        if let Err(e) = validate_source_creature(&creature) {
            eprintln!("  skip {} — {e}", name(&path));
            continue;
        }
        let patches = patch_ids(&creature);
        out.push(Sample {
            path,
            score,
            creature,
            patches,
        });
    }
    Ok(out)
}

fn write_summary(
    out: &Path,
    outcome: &neat_ai_rebase::engine::RebaseOutcome,
    results: &BTreeMap<String, ScoreResult>,
    mode: ScorerMode,
    base: &Sample,
    missing: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let summary = serde_json::json!({
        "experiment": "population-union",
        "mode": mode.label(),
        "authoritative": mode.is_authoritative(),
        "base": name(&base.path),
        "baseTagScore": base.score,
        "baseChecksum": creature_checksum(&base.creature)?,
        "strandedPatches": missing.len(),
        "donorsUsed": outcome.reports.len(),
        "scores": results,
        "candidates": outcome.cohort.iter().map(|c| serde_json::json!({
            "label": c.label,
            "checksum": c.checksum,
            "applied": c.applied_ids.len(),
            "neurons": c.creature.neurons.len(),
        })).collect::<Vec<_>>(),
    });
    let path = out.join("union.json");
    std::fs::write(&path, serde_json::to_string_pretty(&summary)?)?;
    println!("           wrote {}", path.display());
    Ok(())
}
