//! Freeze the inputs and protocol before validation touches the scorer.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use neat_ai_rebase::corpus::corpus_info;
use neat_ai_rebase::creature::{creature_checksum, sha256_hex, validate_source_creature};
use neat_ai_rebase::enhancement::Enhancement;
use neat_core::training_data::{TrainingDataConfig, find_bin_files};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(super) const VERSION: u32 = 1;
pub(super) const METHOD: &str = "https://arxiv.org/html/2610.05950v1";
pub(super) const METHOD_TEXT_SHA256: &str =
    "bc492f7ed3a83a12ba2f610817ab1e7957615cfb730ae195217d2a4ad8836f2e";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Draft {
    pub version: u32,
    pub training: PathBuf,
    pub calibration_training: PathBuf,
    pub calibration_report: PathBuf,
    pub parents: [ParentInput; 3],
    pub patches: Vec<PathBuf>,
    pub seeds: Vec<u64>,
    pub sample_count: usize,
    pub min_improvement: f64,
    pub scorer: PathBuf,
    pub scorer_args: Vec<String>,
    pub evaluation_timeout_seconds: u32,
    pub budget: Budget,
    pub success: Success,
    pub preparation_seconds: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ParentInput {
    pub creature: PathBuf,
    pub provenance: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Budget {
    pub calls: usize,
    pub seconds: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Success {
    pub minimum_paired_runs: usize,
    pub minimum_rate_gain: f64,
    pub maximum_lost_wins: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Calibration {
    sample_count: usize,
    min_improvement: f64,
    validation_used: bool,
    rationale: String,
    seconds: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct History {
    pub complete_to_root: bool,
    pub established_before_validation: bool,
    pub source: String,
    pub seed: u64,
    pub ancestors: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Artifact {
    pub path: PathBuf,
    pub sha256: String,
}

impl Artifact {
    pub fn pin(path: &Path) -> Result<Self, String> {
        let path = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self {
            sha256: hash_file(&path)?,
            path,
        })
    }

    pub fn verify(&self) -> Result<(), String> {
        if hash_file(&self.path)? != self.sha256 {
            return Err(format!("checksum changed: {}", self.path.display()));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Parent {
    pub creature: Artifact,
    pub provenance: Artifact,
    pub checksum: String,
    pub history: History,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CorpusFile {
    pub artifact: Artifact,
    pub records: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RecordRef {
    pub file: usize,
    pub index: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Manifest {
    pub version: u32,
    pub method: String,
    pub method_text_sha256: String,
    pub protocol: Draft,
    pub code: Artifact,
    pub cargo_lock_sha256: String,
    pub code_revision: String,
    pub dirty_checkout: bool,
    pub toolchain: String,
    pub scorer: Artifact,
    pub corpus_identity: String,
    pub corpus: Vec<CorpusFile>,
    pub calibration_corpus: Vec<CorpusFile>,
    pub calibration_report: Artifact,
    pub calibration_seconds: f64,
    pub parents: Vec<Parent>,
    pub patches: Vec<Artifact>,
    pub samples: Vec<Vec<RecordRef>>,
    pub freeze_seconds: f64,
}

pub(super) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    serde_json::from_reader(File::open(path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| format!("{}: {e}", path.display()))
}

pub(super) fn write_new(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("{}: {e}", path.display()))
}

pub(super) fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let len = file
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if len == 0 {
            break;
        }
        hash.update(&buf[..len]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn validate_protocol(draft: &Draft) -> Result<(), String> {
    if draft.version != VERSION {
        return Err("unsupported experiment version".into());
    }
    if draft.seeds.is_empty()
        || draft.seeds.len() > 100
        || draft.seeds.iter().collect::<BTreeSet<_>>().len() != draft.seeds.len()
    {
        return Err("seeds must contain 1..=100 distinct values".into());
    }
    if draft.patches.is_empty() || draft.patches.len() > 10000 {
        return Err("patches must contain 1..=10000 ordered enhancement paths".into());
    }
    if draft.sample_count == 0 || !draft.min_improvement.is_finite() || draft.min_improvement < 0.0
    {
        return Err(
            "sampleCount must be positive and minImprovement finite and non-negative".into(),
        );
    }
    if draft.budget.calls == 0
        || !draft.budget.seconds.is_finite()
        || draft.budget.seconds <= 0.0
        || draft.evaluation_timeout_seconds == 0
        || draft.evaluation_timeout_seconds > 3600
    {
        return Err("budget must be positive; evaluationTimeoutSeconds must be 1..=3600".into());
    }
    if !draft.preparation_seconds.is_finite()
        || draft.preparation_seconds < 0.0
        || draft.success.minimum_paired_runs < 2
        || !draft.success.minimum_rate_gain.is_finite()
        || draft.success.minimum_rate_gain < 0.0
    {
        return Err("invalid preparation cost or success criteria".into());
    }
    // Scorer settings cannot override the full-corpus acceptance contract.
    if draft
        .scorer_args
        .iter()
        .any(|s| !matches!(s.as_str(), "--gpu=off" | "--cost=MSE"))
    {
        return Err("scorerArgs permits only --gpu=off and --cost=MSE".into());
    }
    Ok(())
}

fn pin_corpus(dir: &Path, config: &TrainingDataConfig) -> Result<Vec<CorpusFile>, String> {
    corpus_info(dir, config)?;
    find_bin_files(dir)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|path| {
            let artifact = Artifact::pin(&path)?;
            let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
            Ok(CorpusFile {
                artifact,
                records: size / config.bytes_per_record() as u64,
            })
        })
        .collect()
}

fn record_hashes(
    files: &[CorpusFile],
    width: usize,
    mut visit: impl FnMut(String) -> Result<(), String>,
) -> Result<(), String> {
    let mut buf = vec![0; width];
    for corpus in files {
        let mut file =
            std::io::BufReader::new(File::open(&corpus.artifact.path).map_err(|e| e.to_string())?);
        for _ in 0..corpus.records {
            file.read_exact(&mut buf).map_err(|e| e.to_string())?;
            visit(sha256_hex(&buf))?;
        }
    }
    Ok(())
}

fn disjoint(
    calibration: &[CorpusFile],
    validation: &[CorpusFile],
    width: usize,
) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    record_hashes(calibration, width, |hash| {
        seen.insert(hash);
        Ok(())
    })?;
    record_hashes(validation, width, |hash| {
        if seen.contains(&hash) {
            Err("calibration and validation share a record".into())
        } else {
            Ok(())
        }
    })
}

fn histories_disjoint(parents: &[Parent]) -> Result<(), String> {
    let mut ancestors = BTreeSet::new();
    let mut sources = BTreeSet::new();
    for parent in parents {
        let history = &parent.history;
        if !history.complete_to_root
            || !history.established_before_validation
            || history.source.trim().is_empty()
            || history.ancestors.len() < 2
            || history.ancestors.last() != Some(&parent.checksum)
        {
            return Err(
                "provenance requires a pre-existing complete root-to-parent history".into(),
            );
        }
        if !sources.insert(&history.source) {
            return Err("provenance must name independently established sources".into());
        }
        for hash in &history.ancestors {
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("ancestry must contain SHA-256 identities".into());
            }
            if !ancestors.insert(hash) {
                return Err("overlapping ancestry".into());
            }
        }
    }
    Ok(())
}

fn sample(files: &[CorpusFile], count: usize, seed: u64) -> Vec<RecordRef> {
    let total: u64 = files.iter().map(|file| file.records).sum();
    let mut state = seed;
    let mut selected = BTreeSet::new();
    // SplitMix64 fixes sampling independently of standard-library RNG changes.
    while selected.len() < count {
        state = state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        selected.insert((z ^ (z >> 31)) % total);
    }
    selected
        .into_iter()
        .map(|mut index| {
            for (file, corpus) in files.iter().enumerate() {
                if index < corpus.records {
                    return RecordRef { file, index };
                }
                index -= corpus.records;
            }
            unreachable!("selected index is below the corpus size")
        })
        .collect()
}

fn git(args: &[&str]) -> Result<String, String> {
    let result = std::process::Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err(format!("cannot identify checkout: {}", result.status));
    }
    String::from_utf8(result.stdout)
        .map(|s| s.trim().to_string())
        .map_err(|e| e.to_string())
}

pub(crate) fn freeze(draft_path: &Path, destination: &Path) -> Result<(), String> {
    let started = Instant::now();
    let mut protocol: Draft = read_json(draft_path)?;
    validate_protocol(&protocol)?;
    protocol.training = protocol
        .training
        .canonicalize()
        .map_err(|e| e.to_string())?;
    protocol.calibration_training = protocol
        .calibration_training
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let calibration: Calibration = read_json(&protocol.calibration_report)?;
    if calibration.validation_used
        || calibration.rationale.trim().is_empty()
        || calibration.sample_count != protocol.sample_count
        || calibration.min_improvement != protocol.min_improvement
        || !calibration.seconds.is_finite()
        || calibration.seconds < 0.0
    {
        return Err(
            "calibration must select this sampleCount and minImprovement without validation".into(),
        );
    }
    let mut parents = Vec::new();
    let mut config = None;
    for input in &protocol.parents {
        let creature = Artifact::pin(&input.creature)?;
        let parent = neat_core::parse_creature_json(
            &std::fs::read_to_string(&creature.path).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        validate_source_creature(&parent).map_err(|e| e.to_string())?;
        let shape = (parent.input, parent.output);
        if config.is_some_and(|prior| prior != shape) {
            return Err("parent dimensions differ".into());
        }
        config = Some(shape);
        let provenance = Artifact::pin(&input.provenance)?;
        parents.push(Parent {
            checksum: creature_checksum(&parent)?,
            history: read_json(&provenance.path)?,
            creature,
            provenance,
        });
    }
    histories_disjoint(&parents)?;
    let (inputs, outputs) = config.ok_or("no parents")?;
    let config = TrainingDataConfig::new(inputs, outputs);
    let corpus = pin_corpus(&protocol.training, &config)?;
    let total: u64 = corpus.iter().map(|file| file.records).sum();
    if protocol.sample_count as u64 > total / 2 {
        return Err("sampleCount must be at most half the corpus".into());
    }
    let calibration_corpus = pin_corpus(&protocol.calibration_training, &config)?;
    disjoint(&calibration_corpus, &corpus, config.bytes_per_record())?;
    let corpus_identity = corpus_info(&protocol.training, &config)?.identity;
    let patches: Vec<Artifact> = protocol
        .patches
        .iter()
        .map(|path| {
            let artifact = Artifact::pin(path)?;
            let enhancement = Enhancement::parse_json(
                &std::fs::read_to_string(&artifact.path).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            if enhancement.meta.corpus_identity != corpus_identity {
                return Err("enhancement corpus differs from validation corpus".into());
            }
            Ok(artifact)
        })
        .collect::<Result<_, String>>()?;
    let manifest = Manifest {
        version: VERSION,
        method: METHOD.into(),
        method_text_sha256: METHOD_TEXT_SHA256.into(),
        code: Artifact::pin(&std::env::current_exe().map_err(|e| e.to_string())?)?,
        cargo_lock_sha256: sha256_hex(include_bytes!("../../../Cargo.lock")),
        code_revision: git(&["rev-parse", "HEAD"])?,
        dirty_checkout: !git(&["status", "--porcelain"])?.is_empty(),
        toolchain: include_str!("../../../rust-toolchain.toml").into(),
        scorer: Artifact::pin(&protocol.scorer)?,
        samples: protocol
            .seeds
            .iter()
            .map(|seed| sample(&corpus, protocol.sample_count, *seed))
            .collect(),
        calibration_report: Artifact::pin(&protocol.calibration_report)?,
        calibration_seconds: calibration.seconds,
        corpus_identity,
        corpus,
        calibration_corpus,
        parents,
        patches,
        protocol,
        freeze_seconds: started.elapsed().as_secs_f64(),
    };
    write_new(destination, &manifest)
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self, String> {
        let manifest: Self = read_json(path)?;
        validate_protocol(&manifest.protocol)?;
        if manifest.version != VERSION
            || manifest.method != METHOD
            || manifest.method_text_sha256 != METHOD_TEXT_SHA256
        {
            return Err("unsupported experiment version or research method".into());
        }
        if manifest.parents.len() != 3
            || manifest.samples.len() != manifest.protocol.seeds.len()
            || manifest.patches.len() != manifest.protocol.patches.len()
        {
            return Err("incomplete frozen manifest".into());
        }
        histories_disjoint(&manifest.parents)?;
        manifest.verify()?;
        for membership in &manifest.samples {
            if membership.len() != manifest.protocol.sample_count
                || membership.iter().collect::<BTreeSet<_>>().len() != membership.len()
                || membership.iter().any(|r| {
                    manifest
                        .corpus
                        .get(r.file)
                        .is_none_or(|f| r.index >= f.records)
                })
            {
                return Err("invalid fixed sample membership".into());
            }
        }
        Ok(manifest)
    }

    pub fn verify(&self) -> Result<(), String> {
        self.code.verify()?;
        if hash_file(&std::env::current_exe().map_err(|e| e.to_string())?)? != self.code.sha256 {
            return Err("runner executable differs from frozen code".into());
        }
        self.scorer.verify()?;
        self.calibration_report.verify()?;
        for parent in &self.parents {
            parent.creature.verify()?;
            parent.provenance.verify()?;
        }
        for patch in &self.patches {
            patch.verify()?;
        }
        for corpus in self.corpus.iter().chain(&self.calibration_corpus) {
            corpus.artifact.verify()?;
        }
        Ok(())
    }

    pub fn materialise(
        &self,
        seed_index: usize,
        output: &Path,
    ) -> Result<tempfile::TempDir, String> {
        let directory = tempfile::tempdir_in(output).map_err(|e| e.to_string())?;
        let parent = self.parent(0)?;
        let width = (parent.input + parent.output) * 4;
        let mut bytes = vec![0; width];
        let mut sample =
            File::create(directory.path().join("fixed.bin")).map_err(|e| e.to_string())?;
        for reference in &self.samples[seed_index] {
            let mut source = File::open(&self.corpus[reference.file].artifact.path)
                .map_err(|e| e.to_string())?;
            source
                .seek(SeekFrom::Start(reference.index * width as u64))
                .map_err(|e| e.to_string())?;
            source.read_exact(&mut bytes).map_err(|e| e.to_string())?;
            sample.write_all(&bytes).map_err(|e| e.to_string())?;
        }
        Ok(directory)
    }

    pub fn parent(&self, index: usize) -> Result<neat_core::CreatureExport, String> {
        neat_core::parse_creature_json(
            &std::fs::read_to_string(&self.parents[index].creature.path)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
}
