#!/usr/bin/env python3
"""Audit private study receipts and export aggregate, path-free evidence.

Usage: ledger-study-evidence.py STUDY_DIRECTORY REPLAY_BINARY NEW_OUTPUT_JSON
Requires the input layout produced by ledger_study at the registered source pin.
It invokes ordinary replay in dry-run mode; it never scores validation again.
"""

import collections
import datetime
import hashlib
import itertools
import json
import math
import pathlib
import statistics
import subprocess
import sys
import tempfile
import time


def read(path):
    return json.loads(path.read_text())


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def bootstrap_interval(values):
    """Exact descriptive percentile bootstrap for the three independent runs."""
    if len(values) < 2:
        return None
    means = sorted(statistics.mean(sample) for sample in itertools.product(values, repeat=len(values)))
    return [means[math.ceil(0.025 * len(means)) - 1], means[math.ceil(0.975 * len(means)) - 1]]


def comparison_window(preparation, executions):
    """Charge preparation gaps between timed commands through final validation."""
    costs = [preparation["setupSeconds"], preparation["seconds"],
             *(entry["seconds"] for entry in executions)]
    require(all(math.isfinite(value) and value >= 0 for value in costs),
            "comparison costs must be finite and non-negative")
    began = datetime.datetime.fromisoformat(preparation["beganAt"])
    finished = max(datetime.datetime.fromisoformat(entry["completedAt"])
                   for entry in executions if entry["phase"] == "run")
    window = (finished - began).total_seconds() + preparation["setupSeconds"]
    gap = window - sum(costs)
    require(gap >= 0, "comparison window is shorter than its timed commands")
    return window, gap


def replay(binary, parent, patch, training, workspace):
    with tempfile.TemporaryDirectory(dir=workspace) as temporary:
        temporary = pathlib.Path(temporary)
        payload = temporary / "patch.json"
        payload.write_text(json.dumps(patch))
        output = temporary / "replay"
        completed = subprocess.run(
            [str(binary), "--champion", str(parent), "--enhancements", str(payload),
             "--training-data", str(training), "--dry-run", "--max-candidates", "1",
             "--output-dir", str(output)],
            stdin=subprocess.DEVNULL, capture_output=True, timeout=10,
        )
        if completed.returncode not in (0, 3):
            raise RuntimeError(f"dry replay failed: {completed.stderr.decode(errors='replace')}")
        result = read(output / "rebase.json")
        require(completed.returncode != 3 or result["status"] == "nothingToDo",
                "dry replay returned an unexpected no-improvement result")
        return result


def verify_lineage(directory, lineage, binary, study):
    history = read(directory / f"history-{lineage}.json")
    receipt = directory / f"search-{lineage}.json"
    source_hash = history["source"].partition(" sha256:")[2].partition(";")[0]
    require(source_hash == digest(receipt), "search receipt checksum changed")
    attempts = read(receipt)
    require(len(attempts) == 48, "incomplete search history")
    ancestors = history["ancestors"]
    require(len(ancestors) == 7 and len(set(ancestors)) == 7, "unexpected accepted ancestry")
    parent = directory / f"lineage-{lineage}-0.json"
    for generation in range(1, 7):
        cohort = [entry for entry in attempts if entry["generation"] == generation]
        require(len(cohort) == 8, "incomplete generation")
        for entry in cohort:
            scores = entry["measurements"]
            require(entry["delta"] == scores["child-1"]["score"] - scores["baseline"]["score"],
                    "source delta differs from native scores")
            require(entry["parentHash"] == ancestors[generation - 1], "source parent changed")
        best = max(cohort, key=lambda entry: entry["delta"])
        require(best["delta"] > 1e-8, "accepted non-improving source mutation")
        result = replay(binary, parent, best["patch"], study / "validation", study)
        require(result["championChecksum"] == ancestors[generation - 1], "snapshot parent changed")
        require(result["candidates"][1]["checksum"] == best["childHash"] == ancestors[generation],
                "accepted source edge does not replay")
        parent = directory / f"lineage-{lineage}-{generation}.json"
    final = replay(binary, parent, best["patch"], study / "validation", study)
    require(final["championChecksum"] == ancestors[-1], "final lineage snapshot changed")
    require(digest(parent) == digest(directory / f"parent-{lineage}.json"), "receiving parent changed")
    return {"seed": history["seed"], "ancestors": ancestors, "searchReceiptSha256": digest(receipt),
            "acceptedEdgesReplayed": 6, "nativeSearchAttempts": 48}


def analyse(study, binary):
    started = time.perf_counter()
    executions = read(study / "execution-commands.json")
    preparation = read(study / "preparation.json")
    outer_preparation = read(study.parent / "prepare-command.json")
    window_seconds, preparation_gap = comparison_window(outer_preparation, executions)
    data = read(study / "data.json")
    runs = []
    seen_ancestry = set()
    for number in range(1, 4):
        directory = study / f"run-{number}"
        manifest_path = directory / "manifest.json"
        manifest = read(manifest_path)
        result_path = directory / "validation-run/results.json"
        result = read(result_path)
        require(result["manifestSha256"] == digest(manifest_path), "result manifest changed")
        require(result["status"] == "complete", "validation incomplete")
        for artifact in [manifest["code"], manifest["scorer"], manifest["calibrationReport"],
                         *manifest["patches"],
                         *(file["artifact"] for file in manifest["corpus"] + manifest["calibrationCorpus"]),
                         *(parent[key] for parent in manifest["parents"] for key in ["creature", "provenance"])]:
            require(digest(pathlib.Path(artifact["path"])) == artifact["sha256"], "frozen input changed")
        histories = [verify_lineage(directory, lineage, binary, study) for lineage in range(1, 5)]
        for history in histories:
            require(not seen_ancestry.intersection(history["ancestors"]), "lineages share ancestry")
            seen_ancestry.update(history["ancestors"])
        ledger_path = directory / "validation-run/ledger-v1.jsonl"
        events = [json.loads(line)["event"] for line in ledger_path.read_text().splitlines()]
        evaluations = [entry for entry in events if "stage" in entry]
        pairs = result["pairedRuns"]
        for pair in pairs:
            for arm in ["control", "ledger"]:
                require(pair[arm]["coverage"] == 1 and pair[arm]["operationalFailures"] == 0
                        and pair[arm]["interruptedEvaluations"] == 0
                        and pair[arm]["budgetOverrunSeconds"] == 0, "invalid paired coverage")
        outer_run = next(entry["seconds"] for entry in executions if entry["run"] == number and entry["phase"] == "run")
        outer_freeze = next(entry["seconds"] for entry in executions if entry["run"] == number and entry["phase"] == "freeze")
        measured_run = sum(pair[arm]["totalSeconds"] - pair[arm]["sharedSeconds"]
                           for pair in pairs for arm in ["control", "ledger"])
        run_residual = outer_run - measured_run
        freeze_residual = outer_freeze - manifest["freezeSeconds"]
        require(run_residual >= 0 and freeze_residual >= 0, "outer accounting boundary is inconsistent")
        shared_residual = ((run_residual + freeze_residual) / (2 * len(pairs))
                           + preparation_gap / (3 * 2 * len(pairs)))
        lost_reasons = collections.Counter()
        for pair in pairs:
            seed = pair["seed"]
            lost = set(pair["control"]["winningEnhancements"]) - set(pair["ledger"]["winningEnhancements"])
            for identity in lost:
                terminal = next(entry for entry in evaluations if entry["seed"] == seed
                                and entry["arm"] == "ledger" and entry["stage"] == "complete"
                                and entry["enhancementId"] == identity)
                lost_reasons[terminal["state"]] += 1
            for arm in ["control", "ledger"]:
                seconds = pair[arm]["totalSeconds"] + shared_residual
                gain = pair[arm]["bestGain"]
                pair[arm]["outerInclusiveSeconds"] = seconds
                pair[arm]["outerInclusiveImprovementPerEvaluationHour"] = (
                    None if gain is None else max(gain, 0) * 3600 / seconds)
            control_rate = pair["control"]["outerInclusiveImprovementPerEvaluationHour"]
            ledger_rate = pair["ledger"]["outerInclusiveImprovementPerEvaluationHour"]
            pair["outerInclusiveRateDelta"] = None if control_rate is None or ledger_rate is None else ledger_rate - control_rate
        source = read(directory / "candidates.json")
        runs.append({
            "run": number, "manifestSha256": digest(manifest_path), "resultsSha256": digest(result_path),
            "ledgerSha256": digest(ledger_path), "runnerSha256": manifest["code"]["sha256"],
            "scorerSha256": manifest["scorer"]["sha256"], "codeRevision": manifest["codeRevision"],
            "dirtyCheckout": manifest["dirtyCheckout"], "cargoLockSha256": manifest["cargoLockSha256"],
            "scorerEnvironment": manifest["scorerEnvironment"],
            "validationCorpus": [{"sha256": file["artifact"]["sha256"], "records": file["records"]} for file in manifest["corpus"]],
            "calibrationCorpus": [{"sha256": file["artifact"]["sha256"], "records": file["records"]} for file in manifest["calibrationCorpus"]],
            "lineages": histories, "sourceSuccesses": sum(entry["delta"] > 1e-8 for entry in source),
            "sourceFailures": sum(entry["delta"] <= 1e-8 for entry in source),
            "orderedPayloadSha256": [artifact["sha256"] for artifact in manifest["patches"]],
            "sampleMembershipSha256": hashlib.sha256(json.dumps(manifest["samples"], sort_keys=True, separators=(",", ":")).encode()).hexdigest(),
            "outerRunSeconds": outer_run, "outerFreezeSeconds": outer_freeze,
            "runResidualSeconds": run_residual, "freezeResidualSeconds": freeze_residual,
            "pairedRuns": pairs, "lostWinReasons": dict(lost_reasons),
            "maintenanceDecision": result["maintenanceDecision"],
            "nativeRateDelta95PercentInterval": result["rateDelta95PercentInterval"],
        })
    lost_means = [statistics.mean(pair["lostWins"] for pair in run["pairedRuns"]) for run in runs]
    rate_means = [statistics.mean(pair["outerInclusiveRateDelta"] for pair in run["pairedRuns"]
                                 if pair["outerInclusiveRateDelta"] is not None)
                  for run in runs if any(pair["outerInclusiveRateDelta"] is not None for pair in run["pairedRuns"])]
    require(all(run["maintenanceDecision"] == "remove" for run in runs), "registered removal not established")
    return {
        "version": 1, "scope": "real-data fixed-stream replay, three independent discovery runs on one public regression dataset",
        "registeredSourceCommit": "7b26468e8e70af8597811bef14d6fe1a26e5f683",
        "dataSource": "https://archive.ics.uci.edu/dataset/186/wine+quality",
        "rawDataSha256": data["rawSha256"], "uniqueRows": data["uniqueRows"],
        "discoveryRows": data["discoveryRows"], "calibrationRows": data["calibrationRows"], "validationRows": data["validationRows"],
        "sampleCount": preparation["sampleCount"], "minImprovement": preparation["minImprovement"],
        "preparationCommandSeconds": outer_preparation["seconds"], "setupSeconds": outer_preparation["setupSeconds"],
        "comparisonWindowSeconds": window_seconds, "interCommandPreparationSeconds": preparation_gap,
        "searchAndDataPreparationSeconds": preparation["seconds"], "calibrationSeconds": preparation["calibrationSeconds"],
        "preparationNativeCalls": preparation["nativeCalls"], "preparationScoredRecords": preparation["scoredRecords"],
        "postValidationAuditSeconds": time.perf_counter() - started,
        "runs": runs, "maintenanceDecision": "remove", "benefitEstablished": False,
        "uncertainty": {
            "withinRun": "native paired percentile bootstrap, 2000 resamples; repeated sample seeds on fixed inputs",
            "betweenIndependentRuns": "exact descriptive percentile bootstrap of independent-run means, inverse empirical CDF endpoints; one fixed dataset and only three discovery runs",
            "lostWinMeanPerPairByRun": lost_means, "lostWinMean95PercentInterval": bootstrap_interval(lost_means),
            "outerInclusiveRateMeanByMeasuredRun": rate_means,
            "outerInclusiveRateMean95PercentInterval": bootstrap_interval(rate_means),
            "rateRunsMeasured": len(rate_means), "rateRunsPlanned": len(runs),
            "missingRate": "Run 3 has no ledger full-target measurement; its rate is unknown and excluded, never imputed as zero",
        },
        "accounting": "Primary comparison charges the complete setup-through-validation window, including preparation between commands, search/data preparation, calibration, freeze, replay, sample preparation, scoring, ledger and outer freeze/run commands. Shared preparation costs split equally across three runs and both arms/eight seeds. Timing-receipt writes within that window are included. Post-validation audit is reported separately; subsequent checks and research report generation are outside the comparison. Component timers overlap; outerInclusiveSeconds is authoritative, not their sum.",
        "limitations": [
            "Shared-host dev-build timings are indicative; removal has deterministic lost-win evidence in all three runs.",
            "Single public dataset, shallow custom residual-stump searches; no production-fleet or cross-application generalisation.",
            "Finite fixed stream with equal upper compute limits, not adaptive discovery at matched consumed compute.",
            "Source envelopes carry neutral zero score claims; actual discovery measurements are in private source receipts.",
            "Raw corpora, snapshots, receipts, path-bearing frozen manifests and ledgers remain outside the public checkout.",
        ],
    }


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit(__doc__)
    report = analyse(pathlib.Path(sys.argv[1]).resolve(), pathlib.Path(sys.argv[2]).resolve())
    with pathlib.Path(sys.argv[3]).open("x") as output:
        json.dump(report, output, indent=2, allow_nan=False)
        output.write("\n")
