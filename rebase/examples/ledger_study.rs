//! Real-data input preparation for the frozen ledger study; never publishes a model.
//! Usage: ledger_study <headerless-numeric.csv> <rust_scorer> <new-directory>
//! The registered design is docs/experiments/ledger-140-preregistration.json.

use neat_ai_rebase::corpus::corpus_info;
use neat_ai_rebase::creature::{creature_checksum, sha256_hex, validate_source_creature};
use neat_ai_rebase::engine::{RebaseRequest, rebase};
use neat_ai_rebase::enhancement::{Enhancement, Payload, ProducerContext};
use neat_ai_rebase::fixtures::{creature, neuron, synapse};
use neat_ai_rebase::patch::{Node, Patch, Provenance};
use neat_ai_rebase::scorer::{ScoreResult, parse_scorer_output};
use neat_core::training_data::TrainingDataConfig;
use neat_core::{CreatureExport, compile_creature, creature_to_json};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
    fn index(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn weight(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1_u64 << 53) as f64 - 0.5
    }
}

fn write(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    Ok(())
}

fn corpus(path: &Path, rows: &[Vec<f32>]) -> Result<()> {
    std::fs::create_dir(path)?;
    let bytes: Vec<u8> = rows
        .iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    std::fs::write(path.join("data.bin"), bytes)?;
    Ok(())
}

struct Study {
    root: PathBuf,
    scorer: PathBuf,
    calls: usize,
    records: u64,
}
impl Study {
    fn score(
        &mut self,
        models: &[CreatureExport],
        data: &Path,
    ) -> Result<BTreeMap<String, ScoreResult>> {
        let staging = tempfile::tempdir_in(&self.root)?;
        for (i, model) in models.iter().enumerate() {
            let label = if i == 0 {
                "baseline".to_string()
            } else {
                format!("child-{i}")
            };
            std::fs::write(
                staging.path().join(format!("{label}.json")),
                creature_to_json(model)?,
            )?;
        }
        let output = Command::new("timeout")
            .env_clear()
            .env("NEAT_SCORER_GPU", "off")
            .env("NEAT_SCORER_ACTIVATION_THREADS", "1")
            .env("NEAT_SCORER_FILE_THREADS", "1")
            .env("RAYON_NUM_THREADS", "1")
            .args(["--signal=KILL", "10"])
            .arg(&self.scorer)
            .args(["--gpu=off", "--cost=MSE"])
            .arg(staging.path())
            .arg(data)
            .stdin(Stdio::null())
            .output()?;
        self.calls += 1;
        if !output.status.success() {
            return Err(format!(
                "native study scorer {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        let scores = parse_scorer_output(&String::from_utf8(output.stdout)?)?;
        if scores.len() != models.len() {
            return Err("native scorer omitted study models".into());
        }
        self.records += scores.values().map(|s| s.record_count).sum::<u64>();
        Ok(scores)
    }
}

fn root(rng: &mut Rng, inputs: usize, seed: u64) -> CreatureExport {
    let mut neurons = Vec::new();
    let mut synapses = Vec::new();
    for h in 0..12 {
        let id = format!("root-{seed}-{h}");
        neurons.push(neuron("hidden", &id, rng.weight(), Some("TANH")));
        for i in 0..inputs {
            synapses.push(synapse(&format!("input-{i}"), &id, rng.weight()));
        }
        synapses.push(synapse(&id, "output-0", rng.weight() * 0.2));
    }
    neurons.push(neuron(
        "output",
        "output-0",
        rng.weight() * 0.1,
        Some("IDENTITY"),
    ));
    creature(inputs, 1, neurons, synapses)
}

fn proposal(
    parent: &CreatureExport,
    rows: &[Vec<f32>],
    rng: &mut Rng,
    identity: &str,
) -> Result<Enhancement> {
    let feature = rng.index(parent.input);
    let threshold = rows[rng.index(rows.len())][feature];
    let scale = [-1.0, 0.25, 0.5, 1.0, 2.0, 4.0][rng.index(6)];
    let mut network = compile_creature(parent)?;
    let mut sums = [0.0_f64; 2];
    let mut counts = [0_u64; 2];
    for row in rows {
        let side = usize::from(row[feature] > threshold);
        let prediction = network.activate(&row[..parent.input], 1)[0];
        sums[side] += f64::from(row[parent.input] - prediction);
        counts[side] += 1;
    }
    let correction = |side: usize| {
        if counts[side] == 0 {
            0.0
        } else {
            (scale * sums[side] / counts[side] as f64) as f32
        }
    };
    Ok(Enhancement::new(
        Payload::ForestPatch {
            patch: Patch::new(
                0,
                Node::stump(feature, threshold, correction(0), correction(1)),
                Provenance {
                    strategy: "random-residual-stump".into(),
                    backend: "native-core-residuals".into(),
                    search_records: rows.len() as u64,
                    incumbent_checksum: creature_checksum(parent)?,
                    seed: Some(rng.0),
                    ..Provenance::default()
                },
            ),
        },
        &ProducerContext {
            producer: "ledger-real-data-search".into(),
            base_checksum: creature_checksum(parent)?,
            base_score: 0.0,
            improved_score: 0.0,
            corpus_identity: identity.into(),
            input_count: parent.input,
            output_count: 1,
        },
    ))
}

fn child(
    parent: &CreatureExport,
    enhancement: &Enhancement,
    identity: &str,
) -> Result<CreatureExport> {
    let result = rebase(&RebaseRequest {
        champion: parent,
        enhancements: std::slice::from_ref(enhancement),
        corpus_identity: identity,
        max_candidates: 1,
    })?;
    Ok(result
        .candidates()
        .next()
        .ok_or("study proposal did not build")?
        .creature
        .clone())
}

fn main() -> Result<()> {
    let started = Instant::now();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: ledger_study <numeric.csv> <rust_scorer> <new-directory>".into());
    }
    let raw = std::fs::read(&args[0])?;
    let mut rows: Vec<Vec<f64>> = Vec::new();
    let mut unique = BTreeSet::new();
    for line in std::str::from_utf8(&raw)?.lines() {
        let row: Vec<f64> = line
            .split(',')
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()?;
        if row.len() < 2
            || row.iter().any(|x| !x.is_finite())
            || rows.first().is_some_and(|r| r.len() != row.len())
        {
            return Err(
                "CSV must have consistent finite numeric columns and a final target".into(),
            );
        }
        if unique.insert(row.iter().map(|v| v.to_bits()).collect::<Vec<_>>()) {
            rows.push(row);
        }
    }
    if rows.len() < 2048 {
        return Err("study requires at least 2048 unique rows".into());
    }
    let mut rng = Rng(140);
    for i in (1..rows.len()).rev() {
        let j = rng.index(i + 1);
        rows.swap(i, j);
    }
    let n = rows.len();
    let width = rows[0].len();
    let means: Vec<f64> = (0..width)
        .map(|c| rows[..n / 2].iter().map(|r| r[c]).sum::<f64>() / (n / 2) as f64)
        .collect();
    let scales: Vec<f64> = (0..width)
        .map(|c| {
            (rows[..n / 2]
                .iter()
                .map(|r| (r[c] - means[c]).powi(2))
                .sum::<f64>()
                / (n / 2) as f64)
                .sqrt()
        })
        .collect();
    if scales.contains(&0.0) {
        return Err("constant columns are unsupported".into());
    }
    let rows: Vec<Vec<f32>> = rows
        .iter()
        .map(|r| {
            (0..width)
                .map(|c| ((r[c] - means[c]) / scales[c]) as f32)
                .collect()
        })
        .collect();
    let output = PathBuf::from(&args[2]);
    std::fs::create_dir(&output)?;
    let output = output.canonicalize()?;
    let discovery = output.join("discovery");
    let calibration = output.join("calibration");
    let validation = output.join("validation");
    corpus(&discovery, &rows[..n / 2])?;
    corpus(&calibration, &rows[n / 2..3 * n / 4])?;
    corpus(&validation, &rows[3 * n / 4..])?;
    let identity = corpus_info(&validation, &TrainingDataConfig::new(width - 1, 1))?.identity;
    write(
        &output.join("data.json"),
        &json!({"rawSha256":sha256_hex(&raw), "uniqueRows":n,
        "discoveryRows":n/2,"calibrationRows":3*n/4-n/2,"validationRows":n-3*n/4,
        "means":means,"scales":scales,"splitSeed":140}),
    )?;
    let mut study = Study {
        root: output.clone(),
        scorer: PathBuf::from(&args[1]).canonicalize()?,
        calls: 0,
        records: 0,
    };
    let mut run_parents = Vec::new();
    let mut run_patches = Vec::new();
    let mut calibration_models = Vec::new();
    for run in 1..=3 {
        let dir = output.join(format!("run-{run}"));
        std::fs::create_dir(&dir)?;
        let mut parents = Vec::new();
        let mut donor = None;
        for lineage in 1..=4 {
            let seed = run * 1000 + lineage;
            let mut rng = Rng(seed);
            let mut parent = root(&mut rng, width - 1, seed);
            validate_source_creature(&parent)?;
            let mut ancestors = vec![creature_checksum(&parent)?];
            write(&dir.join(format!("lineage-{lineage}-0.json")), &parent)?;
            let mut receipts = Vec::new();
            for generation in 1..=6 {
                let mut best = None;
                let mut best_delta = 1e-8;
                for attempt in 0..8 {
                    let patch = proposal(&parent, &rows[..n / 2], &mut rng, &identity)?;
                    let candidate = child(&parent, &patch, &identity)?;
                    let scores = study.score(&[parent.clone(), candidate.clone()], &discovery)?;
                    let delta = scores["child-1"].score - scores["baseline"].score;
                    receipts.push(json!({"generation":generation,"attempt":attempt,"patch":patch,
                        "parentHash":creature_checksum(&parent)?,"childHash":creature_checksum(&candidate)?,"measurements":scores,"delta":delta}));
                    if delta > best_delta {
                        best_delta = delta;
                        best = Some(candidate);
                    }
                }
                if let Some(candidate) = best {
                    parent = candidate;
                    ancestors.push(creature_checksum(&parent)?);
                    write(
                        &dir.join(format!("lineage-{lineage}-{generation}.json")),
                        &parent,
                    )?;
                }
            }
            if ancestors.len() < 2 {
                return Err("independent search produced no accepted mutation".into());
            }
            let receipt_path = dir.join(format!("search-{lineage}.json"));
            write(&receipt_path, &receipts)?;
            let history = dir.join(format!("history-{lineage}.json"));
            write(
                &history,
                &json!({"completeToRoot":true,"establishedBeforeValidation":true,"seed":seed,
                "source":format!("{} sha256:{}; lineage snapshots alongside receipt",receipt_path.display(),sha256_hex(&std::fs::read(&receipt_path)?)),"ancestors":ancestors}),
            )?;
            let path = dir.join(format!("parent-{lineage}.json"));
            write(&path, &parent)?;
            if lineage < 4 {
                parents.push(json!({"creature":path,"provenance":history}));
            } else {
                donor = Some((parent, rng));
            }
        }
        let (donor, mut rng) = donor.ok_or("missing donor")?;
        let target: CreatureExport =
            serde_json::from_slice(&std::fs::read(dir.join("parent-1.json"))?)?;
        let mut models = vec![target.clone()];
        let mut paths = Vec::new();
        let mut receipts = Vec::new();
        for i in 0..24 {
            let patch = proposal(&donor, &rows[..n / 2], &mut rng, &identity)?;
            let candidate = child(&donor, &patch, &identity)?;
            let scores = study.score(&[donor.clone(), candidate], &discovery)?;
            let path = dir.join(format!("patch-{i}.json"));
            write(&path, &patch)?;
            receipts.push(json!({"patch":path,"delta":scores["child-1"].score-scores["baseline"].score,"measurements":scores}));
            models.push(child(&target, &patch, &identity)?);
            paths.push(path);
        }
        write(&dir.join("candidates.json"), &receipts)?;
        run_parents.push(parents);
        run_patches.push(paths);
        calibration_models.push(models);
    }
    let preparation_seconds = started.elapsed().as_secs_f64();
    let calibration_started = Instant::now();
    let calibration_rows = &rows[n / 2..3 * n / 4];
    let mut full = Vec::new();
    for models in &calibration_models {
        full.push(study.score(models, &calibration)?);
    }
    let mut trials = Vec::new();
    let mut selected = None;
    for count in [32, 64, 128, 256] {
        let mut sampled = Vec::new();
        for seed in [71, 72, 73, 74] {
            let mut rng = Rng(seed);
            let mut indices: Vec<_> = (0..calibration_rows.len()).collect();
            for i in (1..indices.len()).rev() {
                let j = rng.index(i + 1);
                indices.swap(i, j);
            }
            let data = output.join(format!("calibration-{count}-{seed}"));
            corpus(
                &data,
                &indices[..count]
                    .iter()
                    .map(|i| calibration_rows[*i].clone())
                    .collect::<Vec<_>>(),
            )?;
            let mut scores = Vec::new();
            for models in &calibration_models {
                scores.push(study.score(models, &data)?);
            }
            sampled.push(scores);
        }
        for margin in [1e-8, 1e-5, 0.001, 0.01] {
            let mut false_rejections = 0;
            let mut wins = 0;
            for (r, full_scores) in full.iter().enumerate() {
                for i in 1..=24 {
                    let label = format!("child-{i}");
                    if full_scores[&label].score - full_scores["baseline"].score > margin {
                        wins += 1;
                        for scores in &sampled {
                            if scores[r][&label].score - scores[r]["baseline"].score < -margin {
                                false_rejections += 1;
                            }
                        }
                    }
                }
            }
            trials.push(json!({"sampleCount":count,"margin":margin,"calibrationTargetWins":wins,"falseRejections":false_rejections}));
            if selected.is_none() && false_rejections == 0 && wins > 0 {
                selected = Some((count, margin));
            }
        }
        write(
            &output.join(format!("calibration-measurements-{count}.json")),
            &sampled,
        )?;
    }
    write(&output.join("calibration-full.json"), &full)?;
    write(&output.join("calibration-trials.json"), &trials)?;
    let (count, margin) = selected.ok_or("calibration failed; validation must remain unscored")?;
    let calibration_seconds = calibration_started.elapsed().as_secs_f64();
    let report = output.join("calibration-report.json");
    write(
        &report,
        &json!({"sampleCount":count,"minImprovement":margin,"validationUsed":false,
        "rationale":"Registered smallest sample, then smallest margin, with zero false rejections and at least one calibration win over three independent runs and four sample seeds; all trials retained",
        "seconds":calibration_seconds/3.0}),
    )?;
    for r in 0..3 {
        let patches: Vec<_> = run_patches[r].iter().chain(&run_patches[r]).collect();
        write(
            &output.join(format!("run-{}/draft.json", r + 1)),
            &json!({"version":1,"evidenceClass":"representative",
            "training":validation,"calibrationTraining":calibration,"calibrationReport":report,"parents":run_parents[r],
            "patches":patches,"seeds":[41,42,43,44,45,46,47,48],"sampleCount":count,"minImprovement":margin,
            "scorer":study.scorer,"scorerArgs":["--gpu=off","--cost=MSE"],"evaluationTimeoutSeconds":10,
            "budget":{"calls":192,"seconds":600.0},"success":{"minimumPairedRuns":8,"minimumRateGain":0.01,"maximumLostWins":0},
            "preparationSeconds":preparation_seconds/3.0}),
        )?;
    }
    write(
        &output.join("preparation.json"),
        &json!({"seconds":preparation_seconds,"calibrationSeconds":calibration_seconds,
        "nativeCalls":study.calls,"scoredRecords":study.records,"sampleCount":count,"minImprovement":margin,
        "validationScored":false,"candidateEnvelope":"Neutral zero score claims; source measurements are discovery-only in separate receipts, not validation scores"}),
    )?;
    println!("{}", output.display());
    Ok(())
}
