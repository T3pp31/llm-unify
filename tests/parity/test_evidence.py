import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "tools/parity"))
from evidence import build  # noqa: E402


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.config = {"version": "1", "commit": "c", "archive_sha256": "sha", "required_suites": ["differential", "load"]}
        self.implementation = {"features": {"chat": {"scope": "oss", "status": "partial", "verification_cases": ["chat-1"]}}}
        self.report = {"subject": "current", "baseline": {key: self.config[key] for key in ("version", "commit", "archive_sha256")},
                       "suite": "differential", "status": "passed", "passed": 1, "failed": 0, "skipped": 0,
                       "cases": [{"id": "chat-1", "status": "passed", "differences": []}]}
        self.write()

    def write(self):
        (self.root / "report.json").write_text(json.dumps(self.report))

    def build(self, cases=None, components=None):
        return build(self.config, self.implementation, ["report.json"] if cases is None else cases,
                     [] if components is None else components, root=self.root, subject="current")

    def test_partial_case_proof_does_not_promote_implementation_or_acceptance(self):
        before = copy.deepcopy(self.implementation)
        manifest = self.build()
        self.assertEqual(manifest["cases"]["chat-1"]["features"], ["chat"])
        self.assertEqual(manifest["suites"], {})
        self.assertEqual(manifest["live"], {})
        self.assertEqual(self.implementation, before)

    def test_component_proof_remains_separate_from_full_acceptance(self):
        self.report["suite"] = "storage_foundation"
        self.write()
        manifest = self.build(cases=[], components=["report.json"])
        self.assertIn("storage_foundation", manifest["components"])
        self.assertEqual(manifest["suites"], {})

    def test_stale_failed_skipped_and_zero_case_runs_are_rejected(self):
        for field, value in [("subject", "stale"), ("status", "failed"), ("skipped", 1), ("passed", 0)]:
            original = self.report[field]
            self.report[field] = value
            self.write()
            with self.assertRaises(ValueError):
                self.build()
            self.report[field] = original

    def test_missing_case_mapping_or_case_comparison_is_rejected(self):
        self.report["cases"][0]["id"] = "unknown"
        self.write()
        with self.assertRaises(ValueError):
            self.build()
        self.report["cases"] = []
        self.write()
        with self.assertRaises(ValueError):
            self.build()

    def test_duplicate_proofs_and_empty_input_are_rejected(self):
        with self.assertRaises(ValueError):
            self.build(cases=["report.json", "report.json"])
        with self.assertRaises(ValueError):
            self.build(cases=[])

    def test_full_suite_cannot_be_registered_as_a_component(self):
        with self.assertRaises(ValueError):
            self.build(cases=[], components=["report.json"])


if __name__ == "__main__":
    unittest.main()
