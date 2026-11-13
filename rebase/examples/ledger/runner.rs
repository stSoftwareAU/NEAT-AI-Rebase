//! Paired replay: fixed parents and candidate order; no population publication.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::time::Instant;

use neat_ai_rebase::creature::sha256_hex;
use neat_ai_rebase::engine::{RebaseOutcome, RebaseRequest, rebase};
use neat_ai_rebase::enhancement::Enhancement;
use neat_ai_rebase::journal::{Journal, Record, ScreenVerdict};
use neat_ai_rebase::scorer::{
    DirectoryScorer, ScoreResult, ScorerError, ScorerMode, check_finite, judge, stage,
};
use neat_core::CreatureExport;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::manifest::{Artifact, Manifest, VERSION, hash_file, read_json, write_new};
use super::results;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum State {
    Started,
    Built,
    Accepted,
    Rejected,
    Unresolved,
    ReusedRejection,
    OperationalFailure,
    Incompatible,
    AlreadyPresent,
    ScreenedOut,
    PeerRejected,
    BudgetExhausted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Measurement {
    pub baseline: ScoreResult,
    pub child: ScoreResult,
    pub score_delta: f64,
    pub error_delta: f64,
    pub complexity_delta: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Event {
    pub id: usize,
    pub manifest: String,
    pub seed: u64,
    pub arm: String,
    pub attempt: usize,
    pub stage: String,
    pub state: State,
    pub context: String,
    pub patch: Artifact,
    pub enhancement_id: String,
    pub parent_hash: String,
    pub child_hash: Option<String>,
    pub affected_genes: Vec<String>,
    pub measurement: Option<Measurement>,
    pub seconds: f64,
    pub ledger_seconds: f64,
    pub failure: Option<String>,
    pub source_event: Option<usize>,
}

pub(super) struct Ledger {
    journal: Journal,
    pub events: Vec<Event>,
    previous_write_seconds: f64,
}

impl Ledger {
    fn open(path: &Path, manifest: &str) -> Result<Self, String> {
        let mut events = Vec::new();
        if path.exists() {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("ledger {}: {e}", path.display()))?;
            if !text.is_empty() && !text.ends_with('\n') {
                return Err("ledger has an incomplete final line; no automatic truncation".into());
            }
            for (line, text) in text.lines().enumerate() {
                let value: Value = serde_json::from_str(text)
                    .map_err(|e| format!("ledger line {}: {e}", line + 1))?;
                match value["record"].as_str() {
                    Some("experiment") => {
                        if value["version"].as_u64() != Some(u64::from(VERSION)) {
                            return Err("unsupported experiment version in ledger".into());
                        }
                        let event: Event = serde_json::from_value(value["event"].clone())
                            .map_err(|e| format!("ledger line {}: {e}", line + 1))?;
                        if event.manifest != manifest {
                            return Err("ledger belongs to another frozen manifest".into());
                        }
                        events.push(event);
                    }
                    // Legacy records contain no experiment cache evidence and stay byte-identical.
                    Some(
                        "opening" | "enhancement" | "candidate" | "dropped" | "screen" | "verdict"
                        | "result",
                    ) => {}
                    _ => return Err(format!("unknown ledger record at line {}", line + 1)),
                }
            }
        }
        Ok(Self {
            journal: Journal::new(path),
            events,
            previous_write_seconds: 0.0,
        })
    }

    fn push(&mut self, mut event: Event) -> Result<(), String> {
        event.ledger_seconds = self.previous_write_seconds;
        let started = Instant::now();
        self.journal
            .append(&Record::Experiment {
                version: VERSION,
                event: serde_json::to_value(&event).map_err(|e| e.to_string())?,
            })
            .map_err(|e| format!("ledger append: {e}"))?;
        File::open(self.journal.path())
            .and_then(|f| f.sync_data())
            .map_err(|e| format!("ledger sync: {e}"))?;
        self.previous_write_seconds = started.elapsed().as_secs_f64();
        self.events.push(event);
        Ok(())
    }

    pub fn calls(&self, seed: u64, arm: &str) -> usize {
        self.events
            .iter()
            .filter(|e| e.seed == seed && e.arm == arm && e.state == State::Started)
            .count()
    }

    pub fn spent(&self, seed: u64, arm: &str, timeout: u32) -> f64 {
        self.events
            .iter()
            .filter(|e| e.seed == seed && e.arm == arm)
            .map(|e| {
                let pending = e.state == State::Started
                    && !self
                        .events
                        .iter()
                        .any(|r| r.id == e.id && r.state != State::Started);
                e.seconds + e.ledger_seconds + if pending { f64::from(timeout) } else { 0.0 }
            })
            .sum()
    }

    fn completed(&self, seed: u64, arm: &str, attempt: usize) -> bool {
        self.events.iter().any(|e| {
            e.seed == seed && e.arm == arm && e.attempt == attempt && e.stage == "complete"
        })
    }
}

struct CheckedScorer<'a> {
    inner: &'a dyn DirectoryScorer,
    records: u64,
    called: Cell<bool>,
}

impl DirectoryScorer for CheckedScorer<'_> {
    fn score_directory(
        &self,
        creatures: &Path,
        training: &Path,
        mode: ScorerMode,
    ) -> Result<BTreeMap<String, ScoreResult>, ScorerError> {
        self.called.set(true);
        let results = self.inner.score_directory(creatures, training, mode)?;
        check_finite(&results)?;
        for (label, result) in &results {
            if result.record_count != self.records
                || result.sample_rate.is_some_and(|r| r != 1.0)
                || !result.complexity_penalty.is_finite()
                || !result.time_taken.is_finite()
                || result.time_taken < 0.0
            {
                return Err(ScorerError::Malformed(format!(
                    "{label}: incomplete, non-finite or sampled measurement"
                )));
            }
        }
        Ok(results)
    }

    fn identity(&self) -> String {
        self.inner.identity()
    }
}

fn affected(parent: &CreatureExport, child: &CreatureExport) -> Result<Vec<String>, String> {
    fn genes(creature: &CreatureExport) -> Result<BTreeMap<String, Value>, String> {
        let mut genes = BTreeMap::new();
        for neuron in &creature.neurons {
            genes.insert(
                format!("neuron:{}", neuron.uuid),
                serde_json::to_value(neuron).map_err(|e| e.to_string())?,
            );
        }
        for synapse in &creature.synapses {
            genes.insert(
                format!(
                    "synapse:{}->{}:{:?}",
                    synapse.from_uuid, synapse.to_uuid, synapse.synapse_type
                ),
                serde_json::to_value(synapse).map_err(|e| e.to_string())?,
            );
        }
        Ok(genes)
    }
    let before = genes(parent)?;
    let after = genes(child)?;
    let mut keys: Vec<_> = before
        .keys()
        .chain(after.keys())
        .filter(|key| before.get(*key) != after.get(*key))
        .cloned()
        .collect();
    keys.sort();
    keys.dedup();
    Ok(keys)
}

struct Attempt<'a> {
    manifest: &'a Manifest,
    manifest_hash: &'a str,
    seed: u64,
    arm: &'a str,
    index: usize,
    enhancement: &'a Enhancement,
    patch: &'a Artifact,
    scorer: &'a dyn DirectoryScorer,
    output: &'a Path,
}

impl Attempt<'_> {
    fn event(
        &self,
        stage: &str,
        parent: usize,
        outcome: &RebaseOutcome,
        state: State,
    ) -> Result<Event, String> {
        let child = outcome.candidates().next();
        let context = sha256_hex(
            &serde_json::to_vec(&json!([
                self.manifest_hash,
                self.seed,
                self.arm,
                stage,
                self.patch.sha256,
                outcome.champion_checksum,
            ]))
            .map_err(|e| e.to_string())?,
        );
        Ok(Event {
            id: 0,
            manifest: self.manifest_hash.into(),
            seed: self.seed,
            arm: self.arm.into(),
            attempt: self.index,
            stage: stage.into(),
            state,
            context,
            patch: self.patch.clone(),
            enhancement_id: self.enhancement.meta.id.clone(),
            parent_hash: outcome.champion_checksum.clone(),
            child_hash: child.map(|c| c.checksum.clone()),
            affected_genes: match child {
                Some(c) => affected(&self.manifest.parent(parent)?, &c.creature)?,
                None => Vec::new(),
            },
            measurement: None,
            seconds: 0.0,
            ledger_seconds: 0.0,
            failure: None,
            source_event: None,
        })
    }

    fn build(&self, parent: usize, ledger: &mut Ledger) -> Result<RebaseOutcome, String> {
        let started = Instant::now();
        let creature = self.manifest.parent(parent)?;
        let outcome = rebase(&RebaseRequest {
            champion: &creature,
            enhancements: std::slice::from_ref(self.enhancement),
            corpus_identity: &self.manifest.corpus_identity,
            max_candidates: 1,
        })
        .map_err(|e| e.to_string())?;
        let mut event = self.event("replay", parent, &outcome, State::Built)?;
        event.id = ledger.events.len();
        event.seconds = started.elapsed().as_secs_f64();
        ledger.push(event)?;
        Ok(outcome)
    }

    fn evaluate(
        &self,
        stage_name: &str,
        parent: usize,
        outcome: &RebaseOutcome,
        training: &Path,
        records: u64,
        ledger: &mut Ledger,
    ) -> Result<Event, String> {
        let mut event = self.event(stage_name, parent, outcome, State::Started)?;
        event.id = ledger.events.len();
        // Final outcomes stay sealed; other rejections apply only to this receiving context.
        if self.arm == "ledger"
            && stage_name != "full"
            && let Some(previous) = ledger
                .events
                .iter()
                .rev()
                .find(|e| e.context == event.context && e.state == State::Rejected)
        {
            event.state = State::ReusedRejection;
            event.source_event = Some(previous.id);
            event.measurement = previous.measurement.clone();
            ledger.push(event.clone())?;
            return Ok(event);
        }
        let protocol = &self.manifest.protocol;
        let shared = (self.manifest.freeze_seconds
            + self.manifest.calibration_seconds
            + protocol.preparation_seconds)
            / (2.0 * protocol.seeds.len() as f64);
        if ledger.calls(self.seed, self.arm) >= protocol.budget.calls
            || ledger.spent(self.seed, self.arm, protocol.evaluation_timeout_seconds)
                + shared
                + f64::from(protocol.evaluation_timeout_seconds)
                > protocol.budget.seconds
        {
            event.state = State::BudgetExhausted;
            ledger.push(event.clone())?;
            return Ok(event);
        }
        ledger.push(event.clone())?;
        let started = Instant::now();
        let checked = CheckedScorer {
            inner: self.scorer,
            records,
            called: Cell::new(false),
        };
        let scoring = tempfile::tempdir_in(self.output).map_err(|e| e.to_string())?;
        let result: Result<(State, Option<Measurement>), ScorerError> = if stage_name == "screen" {
            stage(outcome, scoring.path())
                .and_then(|()| checked.score_directory(scoring.path(), training, ScorerMode::Full))
                .and_then(|scores| {
                    let baseline = scores.get("baseline").ok_or(ScorerError::MissingBaseline)?;
                    let candidate = outcome
                        .candidates()
                        .next()
                        .ok_or_else(|| ScorerError::Malformed("empty cohort".into()))?;
                    let child = scores.get(&candidate.label);
                    let verdict = ScreenVerdict::classify(
                        child.map(|c| c.score),
                        baseline.score,
                        protocol.min_improvement,
                    );
                    let state = match verdict {
                        ScreenVerdict::Worse => State::Rejected,
                        ScreenVerdict::Better => State::Accepted,
                        _ => State::Unresolved,
                    };
                    Ok((state, child.map(|child| measurement(baseline, child))))
                })
        } else {
            judge(
                &checked,
                outcome,
                training,
                scoring.path(),
                protocol.min_improvement,
                ScorerMode::Full,
            )
            .and_then(|verdict| {
                let child = verdict
                    .candidates
                    .first()
                    .ok_or_else(|| ScorerError::Malformed("empty verdict".into()))?;
                Ok((
                    if verdict.improved() {
                        State::Accepted
                    } else {
                        State::Rejected
                    },
                    Some(measurement(&verdict.baseline, &child.result)),
                ))
            })
        };
        event.seconds = started.elapsed().as_secs_f64();
        match result {
            Ok((state, measured)) => {
                event.state = state;
                event.measurement = measured;
            }
            Err(error) => {
                event.state = State::OperationalFailure;
                event.failure = Some(format!("experiment scorer failure: {error}"));
                ledger.push(event)?;
                return Err(format!("experiment scorer failure: {error}"));
            }
        }
        if !checked.called.get() {
            return Err("scoring completed without invoking scorer".into());
        }
        ledger.push(event.clone())?;
        Ok(event)
    }

    fn execute(&self, sample: &Path, ledger: &mut Ledger) -> Result<bool, String> {
        let target = self.build(0, ledger)?;
        if target.is_empty() {
            let mut event = self.event("complete", 0, &target, State::AlreadyPresent)?;
            if let Some(report) = target.reports.iter().find(|r| {
                matches!(
                    r.outcome,
                    neat_ai_rebase::engine::EnhancementOutcome::Incompatible(_)
                )
            }) {
                event.state = State::Incompatible;
                event.failure = Some(format!("{:?}", report.outcome));
            }
            event.id = ledger.events.len();
            ledger.push(event)?;
            return Ok(true);
        }
        let records = self.manifest.corpus.iter().map(|f| f.records).sum();
        let mut terminal = self.event("complete", 0, &target, State::Rejected)?;
        if self.arm == "ledger" {
            let screen = self.evaluate(
                "screen",
                0,
                &target,
                sample,
                self.manifest.protocol.sample_count as u64,
                ledger,
            )?;
            if screen.state == State::BudgetExhausted {
                return Ok(false);
            }
            if matches!(screen.state, State::Rejected | State::ReusedRejection) {
                terminal.state = State::ScreenedOut;
                terminal.id = ledger.events.len();
                ledger.push(terminal)?;
                return Ok(true);
            }
            let mut passed = true;
            for (parent, name) in [(1, "peer1"), (2, "peer2")] {
                let peer = self.build(parent, ledger)?;
                if peer.is_empty() {
                    passed = false;
                    continue;
                }
                let result = self.evaluate(
                    name,
                    parent,
                    &peer,
                    &self.manifest.protocol.training,
                    records,
                    ledger,
                )?;
                if result.state == State::BudgetExhausted {
                    return Ok(false);
                }
                passed &= result.state == State::Accepted;
            }
            if !passed {
                terminal.state = State::PeerRejected;
                terminal.id = ledger.events.len();
                ledger.push(terminal)?;
                return Ok(true);
            }
        }
        let result = self.evaluate(
            "full",
            0,
            &target,
            &self.manifest.protocol.training,
            records,
            ledger,
        )?;
        if result.state == State::BudgetExhausted {
            return Ok(false);
        }
        terminal.state = result.state;
        terminal.measurement = result.measurement;
        terminal.id = ledger.events.len();
        ledger.push(terminal)?;
        Ok(true)
    }
}

fn measurement(baseline: &ScoreResult, child: &ScoreResult) -> Measurement {
    Measurement {
        baseline: baseline.clone(),
        child: child.clone(),
        score_delta: child.score - baseline.score,
        error_delta: child.error - baseline.error,
        complexity_delta: child.complexity_penalty - baseline.complexity_penalty,
    }
}

pub(crate) fn run(
    manifest_path: &Path,
    output: &Path,
    scorer: &dyn DirectoryScorer,
) -> Result<Value, String> {
    let preparation = Instant::now();
    let manifest = Manifest::load(manifest_path)?;
    let manifest_hash = hash_file(manifest_path)?;
    std::fs::create_dir_all(output).map_err(|e| e.to_string())?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(output.join("ledger.lock"))
        .map_err(|e| format!("ledger lock: {e}"))?;
    lock.try_lock()
        .map_err(|e| format!("ledger already in use: {e}"))?;
    let mut ledger = Ledger::open(&output.join("ledger-v1.jsonl"), &manifest_hash)?;
    let result_path = output.join("results.json");
    if result_path.exists() {
        let result: Value = read_json(&result_path)?;
        if result["manifestSha256"] != manifest_hash {
            return Err("results belong to another manifest".into());
        }
        return Ok(result);
    }
    let preparation_seconds = preparation.elapsed().as_secs_f64();
    let mut overhead = 0.0;
    for (seed_index, seed) in manifest.protocol.seeds.iter().enumerate() {
        let sample_started = Instant::now();
        let sample = manifest.materialise(seed_index, output)?;
        overhead += sample_started.elapsed().as_secs_f64();
        let arms = if seed_index % 2 == 0 {
            ["control", "ledger"]
        } else {
            ["ledger", "control"]
        };
        for arm in arms {
            for (index, patch) in manifest.patches.iter().enumerate() {
                if ledger.completed(*seed, arm, index) {
                    continue;
                }
                let enhancement = Enhancement::parse_json(
                    &std::fs::read_to_string(&patch.path).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                let attempt = Attempt {
                    manifest: &manifest,
                    manifest_hash: &manifest_hash,
                    seed: *seed,
                    arm,
                    index,
                    enhancement: &enhancement,
                    patch,
                    scorer,
                    output,
                };
                if !attempt.execute(sample.path(), &mut ledger)? {
                    break;
                }
            }
        }
    }
    let verification = Instant::now();
    manifest.verify()?;
    if hash_file(manifest_path)? != manifest_hash {
        return Err("manifest changed during validation".into());
    }
    overhead += verification.elapsed().as_secs_f64();
    let result = results::summarise(
        &manifest,
        &manifest_hash,
        &ledger,
        preparation_seconds + overhead,
        ledger.previous_write_seconds,
    );
    write_new(&result_path, &result)?;
    Ok(result)
}
