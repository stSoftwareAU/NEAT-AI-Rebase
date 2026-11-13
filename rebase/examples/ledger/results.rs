//! Descriptive paired results; validation never feeds back into candidate selection.

use super::manifest::{EvidenceClass, Manifest, VERSION};
use super::runner::{Event, Ledger, State};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn arm(manifest: &Manifest, ledger: &Ledger, seed: u64, arm: &str, common: f64) -> Value {
    let events: Vec<&Event> = ledger
        .events
        .iter()
        .filter(|e| e.seed == seed && e.arm == arm)
        .collect();
    let complete: Vec<_> = events.iter().filter(|e| e.stage == "complete").collect();
    let wins: BTreeSet<_> = complete
        .iter()
        .filter(|e| e.state == State::Accepted)
        .map(|e| &e.enhancement_id)
        .collect();
    let best_gain = complete
        .iter()
        .filter_map(|e| e.measurement.as_ref().map(|m| m.score_delta))
        .reduce(f64::max);
    let improvement = best_gain.map(|gain| gain.max(0.0));
    let total_seconds =
        common + ledger.spent(seed, arm, manifest.protocol.evaluation_timeout_seconds);
    let failures = events
        .iter()
        .filter(|e| e.state == State::OperationalFailure)
        .count();
    let interrupted = events
        .iter()
        .filter(|e| {
            e.state == State::Started
                && !events
                    .iter()
                    .any(|r| r.id == e.id && r.state != State::Started)
        })
        .count();
    let full = events
        .iter()
        .filter(|e| e.stage == "full" && matches!(e.state, State::Accepted | State::Rejected))
        .count();
    json!({
        "planned": manifest.patches.len(), "completed": complete.len(), "wins": wins.len(),
        "winningEnhancements": wins, "transferableWins": if arm == "ledger" { Some(wins.len()) } else { None },
        "bestGain": best_gain, "authoritativeConfirmations": full,
        "calls": ledger.calls(seed, arm), "operationalFailures": failures, "interruptedEvaluations": interrupted,
        "duplicateFailuresAvoided": events.iter().filter(|e| e.state == State::ReusedRejection).count(),
        "unresolvedSamples": events.iter().filter(|e| e.stage == "screen" && e.state == State::Unresolved).count(),
        "screenSeconds": events.iter().filter(|e| e.stage == "screen").map(|e| e.seconds).sum::<f64>(),
        "peerSeconds": events.iter().filter(|e| e.stage == "peer1" || e.stage == "peer2").map(|e| e.seconds).sum::<f64>(),
        "fullSeconds": events.iter().filter(|e| e.stage == "full").map(|e| e.seconds).sum::<f64>(),
        "replaySeconds": events.iter().filter(|e| e.stage == "replay").map(|e| e.seconds).sum::<f64>(),
        "ledgerSeconds": events.iter().map(|e| e.ledger_seconds).sum::<f64>(),
        "sharedSeconds": common, "totalSeconds": total_seconds,
        "budgetOverrunSeconds": (total_seconds - manifest.protocol.budget.seconds).max(0.0),
        "scoredRecords": events.iter().filter(|e| e.state != State::ReusedRejection && e.stage != "complete")
            .filter_map(|e| e.measurement.as_ref()).map(|m| m.baseline.record_count + m.child.record_count).sum::<u64>(),
        "improvementPerEvaluationHour": improvement.map(|gain| gain * 3600.0 / total_seconds),
        "coverage": complete.len() as f64 / manifest.patches.len() as f64,
        "targetCoverage": full as f64 / manifest.patches.len() as f64,
    })
}

fn interval(deltas: &[f64]) -> Option<[f64; 2]> {
    if deltas.len() < 2 {
        return None;
    }
    let mut state = 0x140_u64;
    let mut means = Vec::with_capacity(2000);
    for _ in 0..2000 {
        let mut sum = 0.0;
        for _ in deltas {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            sum += deltas[(state >> 32) as usize % deltas.len()];
        }
        means.push(sum / deltas.len() as f64);
    }
    means.sort_by(f64::total_cmp);
    Some([means[49], means[1949]])
}

pub(super) fn summarise(manifest: &Manifest, hash: &str, ledger: &Ledger) -> Value {
    let common = (manifest.freeze_seconds
        + manifest.calibration_seconds
        + manifest.protocol.preparation_seconds)
        / (2.0 * manifest.protocol.seeds.len() as f64);
    let pairs: Vec<Value> = manifest
        .protocol
        .seeds
        .iter()
        .map(|seed| {
            let control = arm(manifest, ledger, *seed, "control", common);
            let assisted = arm(manifest, ledger, *seed, "ledger", common);
            let rate_delta = control["improvementPerEvaluationHour"]
                .as_f64()
                .zip(assisted["improvementPerEvaluationHour"].as_f64())
                .map(|(c, a)| a - c);
            let control_wins: BTreeSet<_> = control["winningEnhancements"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            let ledger_wins: BTreeSet<_> = assisted["winningEnhancements"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            json!({ "seed": seed, "lostWins": control_wins.difference(&ledger_wins).count(),
            "rateDelta": rate_delta, "control": control, "ledger": assisted })
        })
        .collect();
    let deltas: Vec<f64> = pairs
        .iter()
        .filter_map(|p| p["rateDelta"].as_f64())
        .collect();
    let ci = interval(&deltas);
    let complete = pairs.iter().all(|p| {
        ["control", "ledger"].iter().all(|a| {
            p[a]["coverage"] == 1.0
                && p[a]["budgetOverrunSeconds"] == 0.0
                && p[a]["operationalFailures"] == 0
                && p[a]["interruptedEvaluations"] == 0
        })
    });
    let lost: u64 = pairs.iter().map(|p| p["lostWins"].as_u64().unwrap()).sum();
    let enough = deltas.len() >= manifest.protocol.success.minimum_paired_runs;
    let decision = if !complete
        || pairs.len() < manifest.protocol.success.minimum_paired_runs
        || manifest.protocol.evidence_class != EvidenceClass::Representative
    {
        "inconclusive"
    } else if lost > manifest.protocol.success.maximum_lost_wins as u64 {
        "remove"
    } else if !enough {
        "inconclusive"
    } else if ci.is_some_and(|bounds| bounds[0] > manifest.protocol.success.minimum_rate_gain) {
        "retain"
    } else if ci.is_some_and(|bounds| bounds[1] < 0.0) {
        "remove"
    } else {
        "inconclusive"
    };
    json!({ "version": VERSION, "manifestSha256": hash, "pairedRuns": pairs,
        "status": if complete { "complete" } else { "incomplete" },
        "maintenanceDecision": decision, "rateDelta95PercentInterval": ci,
        "pairedRateMeasurements": deltas.len(),
        "uncertainty": "paired percentile bootstrap, 2000 resamples, seed 0x140; descriptive across fixed-input sample seeds",
        "evidenceClass": manifest.protocol.evidence_class,
        "representativeBenefitEstablished": decision == "retain",
        "timingBoundary": "freeze, declared preparation/calibration, input verification and per-arm wall time including sample preparation, replay, scorer and evaluation-ledger writes; excludes accounting-record writes and final results serialisation",
    })
}
