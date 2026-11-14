"""Deterministic checks for the archived study's descriptive uncertainty."""

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


if __name__ == "__main__":
    unittest.main()
