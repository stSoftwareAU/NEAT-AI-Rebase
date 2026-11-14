"""Deterministic checks for study uncertainty and cost accounting."""

import importlib.util
import json
import math
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "ledger_study_evidence", pathlib.Path(__file__).with_name("ledger-study-evidence.py")
)
evidence = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evidence)


class BootstrapTests(unittest.TestCase):
    def test_two_run_interval_keeps_both_empirical_tails(self):
        # Four equally likely bootstrap means: 1, 2, 2, 3.
        self.assertEqual(evidence.bootstrap_interval([1.0, 3.0]), [1.0, 3.0])

    def test_three_run_interval_keeps_extreme_resamples(self):
        # Each extreme has mass 1/27, exceeding the excluded 2.5% tail.
        self.assertEqual(evidence.bootstrap_interval([1.0, 2.0, 4.0]), [1.0, 4.0])

    def test_unreplicated_or_missing_rates_have_no_interval(self):
        self.assertIsNone(evidence.bootstrap_interval([]))
        self.assertIsNone(evidence.bootstrap_interval([1.0]))


class AccountingTests(unittest.TestCase):
    def fixture(self, finished="2026-11-14T00:00:10+00:00"):
        preparation = {"beganAt": "2026-11-14T00:00:00+00:00", "seconds": 2.0,
                       "setupSeconds": 10.0}
        executions = [{"phase": "freeze", "seconds": 1.0},
                      {"phase": "run", "seconds": 3.0, "completedAt": finished}]
        return preparation, executions

    def test_preparation_between_commands_is_charged(self):
        self.assertEqual(evidence.comparison_window(*self.fixture()), (20.0, 4.0))

    def test_contiguous_commands_have_no_additional_cost(self):
        self.assertEqual(evidence.comparison_window(
            *self.fixture("2026-11-14T00:00:06+00:00")), (16.0, 0.0))

    def test_inconsistent_clock_boundary_is_refused(self):
        with self.assertRaisesRegex(ValueError, "comparison window is shorter"):
            evidence.comparison_window(*self.fixture("2026-11-14T00:00:05+00:00"))

    def test_invalid_costs_cannot_reduce_the_charged_window(self):
        for invalid in [-1.0, float("nan"), float("inf")]:
            preparation, executions = self.fixture()
            preparation["setupSeconds"] = invalid
            with self.assertRaisesRegex(ValueError, "finite and non-negative"):
                evidence.comparison_window(preparation, executions)

    def test_missing_start_timestamp_is_not_treated_as_no_gap(self):
        preparation, executions = self.fixture()
        del preparation["beganAt"]
        with self.assertRaisesRegex(KeyError, "beganAt"):
            evidence.comparison_window(preparation, executions)


class ReceiptCliTests(unittest.TestCase):
    """Exercise the public command with small, self-contained receipt files.

    Only ordinary replay is replaced by a deterministic executable double;
    the auditor reads and validates all manifests, histories and costs itself.
    """

    def write_json(self, path, value):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value))

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="ledger-study-test-")
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.study = self.root / "study"
        self.binary = self.root / "replay-double"
        self.binary.write_text('''#!/bin/sh
while [ "$#" -gt 0 ]; do
    case "$1" in
        --champion) parent=$2; shift 2 ;;
        --output-dir) output=$2; shift 2 ;;
        *) shift ;;
    esac
done
mkdir "$output"
cp "$parent.replay.json" "$output/rebase.json"
''')
        self.binary.chmod(0o755)
        artifact = {"path": str(self.binary), "sha256": evidence.digest(self.binary)}
        self.write_json(self.root / "prepare-command.json", {
            "beganAt": "2026-11-14T00:00:00+00:00", "setupSeconds": 10, "seconds": 2,
        })
        self.write_json(self.study / "preparation.json", {
            "seconds": 1, "calibrationSeconds": 1, "sampleCount": 128,
            "minImprovement": 0.01, "nativeCalls": 0, "scoredRecords": 0,
        })
        self.write_json(self.study / "data.json", {
            "rawSha256": "fixture", "uniqueRows": 3, "discoveryRows": 1,
            "calibrationRows": 1, "validationRows": 1,
        })
        executions = []
        for number in range(1, 4):
            executions.extend([
                {"run": number, "phase": "freeze", "seconds": 1},
                {"run": number, "phase": "run", "seconds": 3,
                 "completedAt": f"2026-11-14T00:00:{14 + 2 * number}+00:00"},
            ])
            directory = self.study / f"run-{number}"
            manifest_path = directory / "manifest.json"
            self.write_json(manifest_path, {
                "protocol": {"seeds": list(range(41, 49))},
                "code": artifact, "scorer": artifact, "calibrationReport": artifact,
                "patches": [], "corpus": [], "calibrationCorpus": [], "parents": [],
                "samples": [], "freezeSeconds": 0.5, "codeRevision": "fixture",
                "dirtyCheckout": False, "cargoLockSha256": "fixture", "scorerEnvironment": {},
            })
            pairs = []
            for seed in range(41, 49):
                pair = {"seed": seed, "lostWins": 0}
                for arm in ["control", "ledger"]:
                    # (12 shared setup/preparation seconds / 3 runs + 0.5 freeze) / 16 arms.
                    pair[arm] = {
                        "coverage": 1, "operationalFailures": 0, "interruptedEvaluations": 0,
                        "budgetOverrunSeconds": 0, "sharedSeconds": 0.28125,
                        "totalSeconds": 0.38125, "bestGain": 1, "winningEnhancements": [],
                    }
                pairs.append(pair)
            self.write_json(directory / "validation-run/results.json", {
                "manifestSha256": evidence.digest(manifest_path), "status": "complete",
                "pairedRuns": pairs, "maintenanceDecision": "remove", "rateDelta95PercentInterval": None,
            })
            (directory / "validation-run/ledger-v1.jsonl").write_text("")
            self.write_json(directory / "candidates.json", [{"delta": 1}])
            for lineage in range(1, 5):
                ancestors = [f"{number}-{lineage}-{generation}" for generation in range(7)]
                attempts = []
                for generation in range(7):
                    parent = directory / f"lineage-{lineage}-{generation}.json"
                    self.write_json(parent, {"checksum": ancestors[generation]})
                    self.write_json(pathlib.Path(str(parent) + ".replay.json"), {
                        "championChecksum": ancestors[generation],
                        "candidates": [{}, {"checksum": ancestors[min(generation + 1, 6)]}],
                    })
                    if generation:
                        attempts.extend([{
                            "generation": generation, "delta": 1,
                            "parentHash": ancestors[generation - 1], "childHash": ancestors[generation],
                            "measurements": {"baseline": {"score": 0}, "child-1": {"score": 1}},
                            "patch": {},
                        }] * 8)
                shutil.copyfile(parent, directory / f"parent-{lineage}.json")
                receipt = directory / f"search-{lineage}.json"
                self.write_json(receipt, attempts)
                self.write_json(directory / f"history-{lineage}.json", {
                    "source": f"fixture sha256:{evidence.digest(receipt)};",
                    "seed": 1000 * number + lineage, "ancestors": ancestors,
                })
        self.write_json(self.study / "execution-commands.json", executions)

    def change_result(self, number, change):
        path = self.study / f"run-{number}/validation-run/results.json"
        result = evidence.read(path)
        change(result["pairedRuns"])
        self.write_json(path, result)

    def invoke(self):
        output = self.root / "report.json"
        completed = subprocess.run(
            [sys.executable, "-B", str(pathlib.Path(evidence.__file__)),
             str(self.study), str(self.binary), str(output)],
            stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=10,
        )
        return completed, output

    def assert_rejected(self, message):
        completed, output = self.invoke()
        self.assertNotEqual(completed.returncode, 0, completed.stdout)
        self.assertIn(message, completed.stderr)
        self.assertFalse(output.exists(), "invalid receipts must not export a report")

    def test_complete_seed_sets_reconcile_with_comparison_window(self):
        self.change_result(2, lambda pairs: pairs.reverse())
        completed, output = self.invoke()
        self.assertEqual(completed.returncode, 0, completed.stderr)
        report = evidence.read(output)
        self.assertEqual([len(run["pairedRuns"]) for run in report["runs"]], [8, 8, 8])
        allocated = math.fsum(pair[arm]["outerInclusiveSeconds"]
                              for run in report["runs"] for pair in run["pairedRuns"]
                              for arm in ["control", "ledger"])
        self.assertAlmostEqual(report["comparisonWindowSeconds"], 30)
        self.assertAlmostEqual(allocated, report["comparisonWindowSeconds"])

    def test_missing_frozen_seed_is_rejected(self):
        self.change_result(1, lambda pairs: pairs.pop())
        self.assert_rejected("paired seeds must match frozen protocol exactly once")

    def test_empty_pair_set_is_rejected(self):
        self.change_result(1, lambda pairs: pairs.clear())
        self.assert_rejected("paired seeds must match frozen protocol exactly once")

    def test_duplicate_seed_is_rejected(self):
        self.change_result(2, lambda pairs: pairs.append(pairs[0]))
        self.assert_rejected("paired seeds must match frozen protocol exactly once")

    def test_duplicate_replacing_missing_seed_is_rejected(self):
        self.change_result(3, lambda pairs: pairs[-1].update(seed=pairs[0]["seed"]))
        self.assert_rejected("paired seeds must match frozen protocol exactly once")

    def test_unexpected_seed_is_rejected(self):
        self.change_result(1, lambda pairs: pairs[-1].update(seed=49))
        self.assert_rejected("paired seeds must match frozen protocol exactly once")

    def test_unreconciled_shared_costs_are_rejected(self):
        self.change_result(1, lambda pairs: pairs[0]["control"].update(sharedSeconds=0))
        self.assert_rejected("allocated costs do not reconcile with comparison window")


if __name__ == "__main__":
    unittest.main()
