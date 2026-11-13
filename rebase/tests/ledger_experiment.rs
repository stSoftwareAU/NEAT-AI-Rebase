//! Behaviour of the opt-in experiment through the same entry points as the example.

#[path = "../examples/ledger/mod.rs"]
mod ledger;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use neat_ai_rebase::corpus::corpus_info;
use neat_ai_rebase::creature::{creature_checksum, sha256_hex};
use neat_ai_rebase::enhancement::{Enhancement, Payload, ProducerContext};
use neat_ai_rebase::fixtures::linear_hidden_creature;
use neat_ai_rebase::patch::{Node, Patch, Provenance};
use neat_ai_rebase::scorer::{DirectoryScorer, ScoreResult, ScorerError, ScorerMode};
use neat_core::training_data::TrainingDataConfig;
use serde_json::{Value, json};

struct Harness {
    _temp: tempfile::TempDir,
    draft: PathBuf,
    manifest: PathBuf,
    output: PathBuf,
    training: PathBuf,
    patch: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let training = root.join("validation");
        let calibration = root.join("calibration");
        for (dir, offset) in [(&training, 0.0_f32), (&calibration, 100.0_f32)] {
            std::fs::create_dir(dir).unwrap();
            let mut data = Vec::new();
            for i in 0..16 {
                for x in [offset + i as f32, 0.25, 0.5] {
                    data.extend_from_slice(&x.to_le_bytes());
                }
            }
            std::fs::write(dir.join("data.bin"), data).unwrap();
        }
        let corpus = corpus_info(&training, &TrainingDataConfig::new(2, 1)).unwrap();
        let mut parents = Vec::new();
        for i in 0..3 {
            let parent = linear_hidden_creature(1.0 + i as f64);
            let hash = creature_checksum(&parent).unwrap();
            let path = root.join(format!("parent-{i}.json"));
            std::fs::write(&path, neat_core::creature_to_json(&parent).unwrap()).unwrap();
            let provenance = root.join(format!("history-{i}.json"));
            write_json(
                &provenance,
                &json!({
                    "completeToRoot": true, "establishedBeforeValidation": true,
                    "source": format!("independent-archive-{i}"), "seed": i + 50,
                    "ancestors": [sha256_hex(format!("root-{i}").as_bytes()), hash]
                }),
            );
            parents.push(json!({"creature": path, "provenance": provenance}));
        }
        let enhancement = Enhancement::new(
            Payload::ForestPatch {
                patch: Patch::new(0, Node::stump(0, 0.5, 0.0, 0.25), Provenance::default()),
            },
            &ProducerContext {
                producer: "public-test".into(),
                base_checksum: "source".into(),
                base_score: 0.5,
                improved_score: 0.6,
                corpus_identity: corpus.identity,
                input_count: 2,
                output_count: 1,
            },
        );
        let patch = root.join("enhancement.json");
        write_json(&patch, &serde_json::to_value(enhancement).unwrap());
        let report = root.join("calibration.json");
        write_json(
            &report,
            &json!({
                "sampleCount": 4, "minImprovement": 0.001,
                "validationUsed": false, "rationale": "separate calibration evidence",
                "seconds": 0.01
            }),
        );
        let draft = root.join("draft.json");
        write_json(
            &draft,
            &json!({
            "version": 1, "evidenceClass": "smoke", "training": training, "calibrationTraining": calibration,
                "calibrationReport": report, "parents": parents,
                "patches": [&patch, &patch], "seeds": [41, 42],
                "sampleCount": 4, "minImprovement": 0.001,
                "scorer": std::env::current_exe().unwrap(),
                "scorerArgs": [], "evaluationTimeoutSeconds": 10,
                "budget": {"calls": 30, "seconds": 120.0},
                "success": {"minimumPairedRuns": 2, "minimumRateGain": 0.1,
                    "maximumLostWins": 0},
                "preparationSeconds": 0.02
            }),
        );
        Self {
            draft,
            manifest: root.join("manifest.json"),
            output: root.join("output"),
            training,
            patch,
            _temp: temp,
        }
    }

    fn edit(&self, f: impl FnOnce(&mut Value)) {
        let mut value = read_json(&self.draft);
        f(&mut value);
        write_json(&self.draft, &value);
    }

    fn freeze(&self) {
        ledger::freeze(&self.draft, &self.manifest).unwrap();
    }

    fn events(&self) -> Vec<Value> {
        std::fs::read_to_string(self.output.join("ledger-v1.jsonl"))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str::<Value>(s).unwrap()["event"].clone())
            .collect()
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn write_json(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

#[derive(Default)]
struct Scorer {
    calls: RefCell<Vec<(String, bool)>>,
    sample_score: Option<f64>,
    full_score: f64,
    fail_parent: Option<String>,
    fail_once: RefCell<bool>,
}

impl Scorer {
    fn winning() -> Self {
        Self {
            sample_score: Some(0.6),
            full_score: 0.6,
            ..Self::default()
        }
    }
}

impl DirectoryScorer for Scorer {
    fn score_directory(
        &self,
        creatures: &Path,
        training: &Path,
        mode: ScorerMode,
    ) -> Result<BTreeMap<String, ScoreResult>, ScorerError> {
        assert_eq!(mode, ScorerMode::Full, "the fixed sample is materialised");
        let parent = neat_core::parse_creature_json(
            &std::fs::read_to_string(creatures.join("baseline.json")).unwrap(),
        )
        .unwrap();
        let hash = creature_checksum(&parent).unwrap();
        let count = corpus_info(training, &TrainingDataConfig::new(2, 1))
            .unwrap()
            .record_count;
        let sample = count == 4;
        self.calls.borrow_mut().push((hash.clone(), sample));
        if *self.fail_once.borrow() {
            *self.fail_once.borrow_mut() = false;
            return Err(ScorerError::Malformed("missing error field".into()));
        }
        let mut results = BTreeMap::new();
        for entry in std::fs::read_dir(creatures).unwrap() {
            let path = entry.unwrap().path();
            let stem = path.file_stem().unwrap().to_str().unwrap();
            let score = if stem == "baseline" {
                0.5
            } else if sample {
                match self.sample_score {
                    Some(s) => s,
                    None => continue,
                }
            } else if self.fail_parent.as_ref() == Some(&hash) {
                0.4
            } else {
                self.full_score
            };
            results.insert(
                stem.into(),
                ScoreResult {
                    score,
                    error: 1.0 - score - if stem == "baseline" { 0.01 } else { 0.02 },
                    complexity_penalty: if stem == "baseline" { 0.01 } else { 0.02 },
                    record_count: count,
                    sample_rate: None,
                    gpu_backend: Some("cpu-fallback".into()),
                    cost_name: Some("MSE".into()),
                    time_taken: 0.001,
                },
            );
        }
        Ok(results)
    }

    fn identity(&self) -> String {
        "deterministic contract scorer".into()
    }
}

#[test]
fn paired_replay_preserves_payloads_parents_and_the_full_acceptance_gate() {
    let h = Harness::new();
    let before = std::fs::read(&h.patch).unwrap();
    h.freeze();
    let frozen = read_json(&h.manifest);
    let scorer = Scorer::winning();
    let result = ledger::run(&h.manifest, &h.output, &scorer).unwrap();
    assert_eq!(result["pairedRuns"].as_array().unwrap().len(), 2);
    assert_eq!(result["pairedRuns"][0]["control"]["wins"], 1);
    assert_eq!(result["pairedRuns"][0]["ledger"]["wins"], 1);
    assert_eq!(result["pairedRuns"][0]["ledger"]["transferableWins"], 1);
    assert_eq!(std::fs::read(&h.patch).unwrap(), before);
    for parent in frozen["parents"].as_array().unwrap() {
        let path = parent["creature"]["path"].as_str().unwrap();
        assert_eq!(
            sha256_hex(&std::fs::read(path).unwrap()),
            parent["creature"]["sha256"]
        );
    }
    let full: Vec<_> = h
        .events()
        .into_iter()
        .filter(|e| e["stage"] == "full" && e["state"] == "accepted")
        .collect();
    assert_eq!(full.len(), 8);
    assert!(
        full.iter()
            .all(|e| e["measurement"]["errorDelta"].as_f64().unwrap() < 0.0)
    );
    assert!(
        full.iter()
            .all(|e| e["measurement"]["complexityDelta"] == 0.01)
    );
    assert!(full.iter().all(|e| {
        e["affectedGenes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g.as_str().unwrap().starts_with("neuron:forest-"))
    }));
    let calls = scorer.calls.borrow().len();
    let resumed = ledger::run(&h.manifest, &h.output, &scorer).unwrap();
    assert_eq!(resumed["pairedRuns"], result["pairedRuns"]);
    assert_eq!(
        scorer.calls.borrow().len(),
        calls,
        "completed attempts are not recharged"
    );
}

#[test]
fn only_exact_screen_failures_are_reused_and_a_new_context_retries() {
    let h = Harness::new();
    h.freeze();
    let scorer = Scorer {
        sample_score: Some(0.4),
        ..Scorer::winning()
    };
    let result = ledger::run(&h.manifest, &h.output, &scorer).unwrap();
    for pair in result["pairedRuns"].as_array().unwrap() {
        assert_eq!(pair["ledger"]["duplicateFailuresAvoided"], 1);
        assert_eq!(pair["ledger"]["calls"], 1);
        assert_eq!(pair["control"]["calls"], 2);
    }
    assert_eq!(
        scorer
            .calls
            .borrow()
            .iter()
            .filter(|(_, sample)| *sample)
            .count(),
        2,
        "a different seed must retry"
    );
    let changed = Harness::new();
    changed.edit(|d| d["minImprovement"] = json!(0.2));
    let calibration = read_json(&changed.draft)["calibrationReport"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut report = read_json(Path::new(&calibration));
    report["minImprovement"] = json!(0.2);
    write_json(Path::new(&calibration), &report);
    changed.freeze();
    let changed_scorer = Scorer {
        sample_score: Some(0.4),
        ..Scorer::winning()
    };
    let retried = ledger::run(&changed.manifest, &changed.output, &changed_scorer).unwrap();
    assert_eq!(
        retried["pairedRuns"][0]["ledger"]["duplicateFailuresAvoided"], 2,
        "only peer rejections can now be reused; the sample is unresolved"
    );
    assert!(
        changed_scorer
            .calls
            .borrow()
            .iter()
            .filter(|(_, sample)| *sample)
            .count()
            >= 2
    );
}

#[test]
fn a_rejected_peer_does_not_suppress_the_other_unrelated_lineage() {
    let h = Harness::new();
    h.freeze();
    let manifest = read_json(&h.manifest);
    let p1 = manifest["parents"][1]["checksum"]
        .as_str()
        .unwrap()
        .to_owned();
    let p2 = manifest["parents"][2]["checksum"]
        .as_str()
        .unwrap()
        .to_owned();
    let scorer = Scorer {
        fail_parent: Some(p1.clone()),
        ..Scorer::winning()
    };
    let result = ledger::run(&h.manifest, &h.output, &scorer).unwrap();
    assert_eq!(result["pairedRuns"][0]["ledger"]["wins"], 0);
    assert!(
        scorer
            .calls
            .borrow()
            .iter()
            .any(|(hash, sample)| hash == &p1 && !sample)
    );
    assert!(
        scorer
            .calls
            .borrow()
            .iter()
            .any(|(hash, sample)| hash == &p2 && !sample)
    );
    assert!(
        !h.events()
            .iter()
            .any(|e| e["arm"] == "ledger" && e["stage"] == "full")
    );
}

#[test]
fn unresolved_or_missing_sample_scores_continue_but_a_full_loss_cannot_promote() {
    for sample in [None, Some(0.5)] {
        let h = Harness::new();
        h.freeze();
        let scorer = Scorer {
            sample_score: sample,
            full_score: 0.4,
            ..Scorer::default()
        };
        let result = ledger::run(&h.manifest, &h.output, &scorer).unwrap();
        assert_eq!(result["pairedRuns"][0]["ledger"]["wins"], 0);
        assert!(
            h.events()
                .iter()
                .any(|e| e["stage"] == "screen" && e["state"] == "unresolved")
        );
        assert!(
            h.events()
                .iter()
                .any(|e| e["stage"] == "peer1" && e["state"] == "rejected")
        );
    }
    let h = Harness::new();
    h.freeze();
    let scorer = Scorer {
        full_score: 0.5001,
        ..Scorer::winning()
    };
    let result = ledger::run(&h.manifest, &h.output, &scorer).unwrap();
    assert_eq!(
        result["pairedRuns"][0]["control"]["wins"], 0,
        "judge enforces the threshold"
    );
}

#[test]
fn malformed_measurements_are_operational_failures_and_are_retryable() {
    let h = Harness::new();
    h.freeze();
    let scorer = Scorer {
        fail_once: RefCell::new(true),
        ..Scorer::winning()
    };
    let error = ledger::run(&h.manifest, &h.output, &scorer).unwrap_err();
    assert!(error.contains("missing error field"), "{error}");
    let failures: Vec<_> = h
        .events()
        .into_iter()
        .filter(|e| e["state"] == "operationalFailure")
        .collect();
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0]["measurement"].is_null(),
        "unavailable is not zero"
    );
    assert!(
        failures[0]["failure"]
            .as_str()
            .unwrap()
            .contains("missing error field")
    );
    let result = ledger::run(&h.manifest, &h.output, &scorer).unwrap();
    assert_eq!(result["pairedRuns"][0]["control"]["wins"], 1);
    assert_eq!(result["pairedRuns"][0]["control"]["operationalFailures"], 1);
}

#[test]
fn overlapping_ancestry_and_changed_inputs_refuse_before_scoring() {
    let h = Harness::new();
    let draft = read_json(&h.draft);
    let p0 = draft["parents"][0]["provenance"].as_str().unwrap();
    let p1 = draft["parents"][1]["provenance"].as_str().unwrap();
    let mut history = read_json(Path::new(p1));
    history["ancestors"][0] = read_json(Path::new(p0))["ancestors"][0].clone();
    write_json(Path::new(p1), &history);
    let err = ledger::freeze(&h.draft, &h.manifest).unwrap_err();
    assert!(err.contains("overlapping ancestry"), "{err}");
    let h = Harness::new();
    h.freeze();
    std::fs::write(h.training.join("data.bin"), vec![0; 16 * 12]).unwrap();
    let scorer = Scorer::winning();
    let err = ledger::run(&h.manifest, &h.output, &scorer).unwrap_err();
    assert!(err.contains("checksum changed"), "{err}");
    assert_eq!(scorer.calls.borrow().len(), 0);
}

#[test]
fn ledger_errors_and_unknown_versions_fail_loud_without_scoring() {
    let h = Harness::new();
    h.freeze();
    std::fs::create_dir_all(h.output.join("ledger-v1.jsonl")).unwrap();
    let scorer = Scorer::winning();
    let error = ledger::run(&h.manifest, &h.output, &scorer).unwrap_err();
    assert!(error.contains("ledger"), "{error}");
    assert_eq!(scorer.calls.borrow().len(), 0);
    let h = Harness::new();
    h.freeze();
    std::fs::create_dir_all(&h.output).unwrap();
    std::fs::write(
        h.output.join("ledger-v1.jsonl"),
        "{\"record\":\"experiment\",\"version\":99,\"event\":{}}\n",
    )
    .unwrap();
    let error = ledger::run(&h.manifest, &h.output, &scorer).unwrap_err();
    assert!(error.contains("unsupported experiment version"), "{error}");
    assert_eq!(scorer.calls.borrow().len(), 0);
}

#[test]
fn external_measurements_require_real_fields_instead_of_defaulting_missing_values() {
    let baseline = json!({"score":0.5,"error":0.49,"complexityPenalty":0.01,
        "recordCount":16,"timeTaken":0.001});
    let accepted = json!({"baseline": baseline});
    assert_eq!(
        ledger::scorer::parse(&accepted.to_string()).unwrap()["baseline"].error,
        0.49
    );
    for field in [
        "score",
        "error",
        "complexityPenalty",
        "recordCount",
        "timeTaken",
    ] {
        let mut missing = accepted.clone();
        missing["baseline"].as_object_mut().unwrap().remove(field);
        let error = ledger::scorer::parse(&missing.to_string()).unwrap_err();
        assert!(
            matches!(error, ScorerError::Malformed(ref reason) if reason.contains(field)),
            "{error}"
        );
    }
    assert_eq!(
        ledger::scorer::parse("{}").unwrap_err(),
        ScorerError::MissingBaseline
    );
    assert!(matches!(
        ledger::scorer::parse("invalid JSON").unwrap_err(),
        ScorerError::Malformed(_)
    ));
    let mut empty = accepted;
    empty["baseline"]["recordCount"] = json!(0);
    assert!(
        matches!(ledger::scorer::parse(&empty.to_string()).unwrap_err(), ScorerError::Malformed(ref s) if s.contains("recordCount"))
    );
}

#[test]
fn the_experimental_reader_preserves_legacy_journals_without_using_them_as_a_cache() {
    use neat_ai_rebase::journal::{Journal, Record};
    let h = Harness::new();
    h.freeze();
    std::fs::create_dir_all(&h.output).unwrap();
    let path = h.output.join("ledger-v1.jsonl");
    Journal::new(&path)
        .append(&Record::Result {
            status: "nothingToDo".into(),
            detail: None,
            emitted_checksum: None,
        })
        .unwrap();
    let original = std::fs::read_to_string(&path).unwrap();
    let scorer = Scorer::winning();
    let result = ledger::run(&h.manifest, &h.output, &scorer).unwrap();
    assert_eq!(result["pairedRuns"][0]["control"]["calls"], 2);
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .starts_with(&original)
    );
    let report = neat_ai_rebase::report::report(&[path]).unwrap();
    assert!(report.contains("nothingToDo"), "{report}");
}

#[test]
fn budgets_censor_coverage_instead_of_inventing_a_measurement() {
    let h = Harness::new();
    h.edit(|d| d["budget"]["calls"] = json!(1));
    h.freeze();
    let scorer = Scorer::winning();
    let result = ledger::run(&h.manifest, &h.output, &scorer).unwrap();
    assert_eq!(result["status"], "incomplete");
    assert_eq!(result["maintenanceDecision"], "inconclusive");
    assert_eq!(result["pairedRuns"][0]["control"]["completed"], 1);
    assert_eq!(result["pairedRuns"][0]["ledger"]["completed"], 0);
    assert!(
        h.events()
            .iter()
            .any(|e| e["state"] == "budgetExhausted" && e["measurement"].is_null())
    );
    assert_eq!(scorer.calls.borrow().len(), 4, "one call per arm per seed");
}

#[test]
fn calibration_overlap_and_replacing_a_frozen_manifest_are_refused() {
    let h = Harness::new();
    h.edit(|d| d["calibrationTraining"] = d["training"].clone());
    let error = ledger::freeze(&h.draft, &h.manifest).unwrap_err();
    assert!(error.contains("share a record"), "{error}");
    let h = Harness::new();
    h.freeze();
    let before = std::fs::read(&h.manifest).unwrap();
    assert!(
        ledger::freeze(&h.draft, &h.manifest)
            .unwrap_err()
            .contains("exists")
    );
    assert_eq!(std::fs::read(&h.manifest).unwrap(), before);
}

struct BrokenScorer<'a> {
    inner: &'a Scorer,
    fault: &'a str,
}

impl DirectoryScorer for BrokenScorer<'_> {
    fn score_directory(
        &self,
        creatures: &Path,
        training: &Path,
        mode: ScorerMode,
    ) -> Result<BTreeMap<String, ScoreResult>, ScorerError> {
        let mut results = self.inner.score_directory(creatures, training, mode)?;
        match self.fault {
            "baseline" => {
                results.remove("baseline");
            }
            "candidate" => {
                results.remove("single-00");
            }
            "sampled" => {
                results.get_mut("single-00").unwrap().sample_rate = Some(0.5);
            }
            "nonfinite" => {
                results.get_mut("single-00").unwrap().score = f64::NAN;
            }
            "complexity" => {
                results.get_mut("single-00").unwrap().complexity_penalty = f64::INFINITY;
            }
            "records" => {
                results.get_mut("single-00").unwrap().record_count = 1;
            }
            other => panic!("unexpected test fault: {other}"),
        }
        Ok(results)
    }
    fn identity(&self) -> String {
        self.inner.identity()
    }
}

#[test]
fn incomplete_or_invalid_full_results_never_pass_the_real_acceptance_gate() {
    for (fault, cause) in [
        ("baseline", "no `baseline`"),
        ("candidate", "no entry for candidate"),
        ("sampled", "sampled measurement"),
        ("nonfinite", "non-finite result"),
        ("complexity", "non-finite or sampled measurement"),
        ("records", "incomplete"),
    ] {
        let h = Harness::new();
        h.freeze();
        let inner = Scorer::winning();
        let error = ledger::run(
            &h.manifest,
            &h.output,
            &BrokenScorer {
                inner: &inner,
                fault,
            },
        )
        .unwrap_err();
        assert!(error.contains(cause), "{fault}: {error}");
        let events = h.events();
        let failure = events
            .iter()
            .find(|e| e["state"] == "operationalFailure")
            .unwrap();
        assert_eq!(
            failure["stage"], "full",
            "the fixture must reach the authoritative gate"
        );
        assert!(failure["failure"].as_str().unwrap().contains(cause));
        assert!(
            !events
                .iter()
                .any(|e| e["stage"] == "complete" && e["state"] == "accepted")
        );
        let recovered = ledger::run(&h.manifest, &h.output, &inner).unwrap();
        assert_eq!(
            recovered["pairedRuns"][0]["control"]["wins"], 1,
            "the same input with just the scorer fault removed must be accepted"
        );
    }
}

#[test]
fn an_interrupted_call_is_charged_and_retried_while_a_partial_line_is_refused() {
    let h = Harness::new();
    h.freeze();
    let scorer = Scorer {
        fail_once: RefCell::new(true),
        ..Scorer::winning()
    };
    assert!(
        ledger::run(&h.manifest, &h.output, &scorer)
            .unwrap_err()
            .contains("missing error field")
    );
    let path = h.output.join("ledger-v1.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();
    // Simulate a process killed after dispatch, before the failure and accounting writes.
    lines.retain(|line| {
        let e: Value = serde_json::from_str(line).unwrap();
        e["event"]["state"] != "operationalFailure" && e["event"]["component"] != "arm"
    });
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    let result = ledger::run(&h.manifest, &h.output, &scorer).unwrap();
    assert_eq!(result["pairedRuns"][0]["control"]["calls"], 3);
    assert_eq!(
        result["pairedRuns"][0]["control"]["interruptedEvaluations"],
        1
    );
    assert_eq!(result["maintenanceDecision"], "inconclusive");
    let h = Harness::new();
    h.freeze();
    std::fs::create_dir_all(&h.output).unwrap();
    std::fs::write(
        h.output.join("ledger-v1.jsonl"),
        "{\"record\":\"experiment\"",
    )
    .unwrap();
    let inner = Scorer::winning();
    let error = ledger::run(&h.manifest, &h.output, &inner).unwrap_err();
    assert!(error.contains("incomplete final line"), "{error}");
    assert!(inner.calls.borrow().is_empty());
}
