"""Deterministic checks for study uncertainty and cost accounting."""

import importlib.util
import pathlib
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


if __name__ == "__main__":
    unittest.main()
