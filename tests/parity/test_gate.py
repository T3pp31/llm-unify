"""Gate behavior tests, independent of the large vendor inventory."""
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "tools/parity"))
from gate import evaluate, report_valid  # noqa: E402


class ReleaseGateTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.subject = "test-source-digest"
        self.config = {"version": "1", "commit": "commit", "archive_sha256": "sha",
                       "destination": "vendor", "allowed_exclusions": ["enterprise_only"],
                       "required_suites": ["differential"], "required_live_providers": ["openai"],
                       "load": {"concurrency": 100, "max_rss_growth_ratio": 0.2}}
        self.inventory = {"features": [{"id": "chat"}]}
        report = {"subject": self.subject, "status": "passed", "passed": 1, "failed": 0, "skipped": 0}
        report["baseline"] = {k: self.config[k] for k in ["version", "commit", "archive_sha256"]}
        report.update(suite="differential", provider="openai", cases=[{"id": "chat-1", "status": "passed", "differences": []}])
        path = self.root / "report.json"
        path.write_text(json.dumps(report))
        self.proof = {"status": "passed", "report": "report.json", "sha256": hashlib.sha256(path.read_bytes()).hexdigest(), "features": ["chat"]}
        self.implementation = {"schema_version": 1, "features": {"chat": {
            "scope": "oss", "status": "implemented", "contract": {"input": "JSON request", "output": "JSON response"},
            "verification_cases": ["chat-1"]}}}
        self.evidence = {"schema_version": 1, "subject": self.subject,
                         "baseline": {k: self.config[k] for k in ["version", "commit", "archive_sha256"]},
                         "cases": {"chat-1": self.proof}, "suites": {"differential": self.proof},
                         "live": {"openai": self.proof}, "load": {"concurrency": 100, "max_rss_growth_ratio": 0.1,
                         "unexpected_failures": 0, "duplicate_charges": 0, "authorization_leaks": 0, "leaked_reservations": 0}}

    def errors(self):
        return evaluate(self.config, self.inventory, self.implementation, self.evidence, self.root, self.subject)

    def change_report(self, update):
        path = self.root / "report.json"
        report = json.loads(path.read_text())
        update(report)
        path.write_text(json.dumps(report))
        self.proof["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()

    def test_complete_fixture_passes(self):
        self.assertEqual(self.errors(), [])

    def test_empty_implementation_cannot_pass(self):
        self.implementation["features"] = {}
        self.assertTrue(any("scope unreviewed" in error for error in self.errors()))

    def test_imported_code_does_not_mean_implemented(self):
        self.implementation["features"]["chat"]["status"] = "imported"
        self.assertTrue(any("unimplemented" in error for error in self.errors()))

    def test_missing_cases_cannot_pass(self):
        self.implementation["features"]["chat"]["verification_cases"] = []
        self.assertTrue(any("verification cases" in error for error in self.errors()))

    def test_stale_source_cannot_pass(self):
        self.evidence["subject"] = "older"
        self.assertTrue(any("stale" in error for error in self.errors()))

    def test_modified_report_cannot_pass(self):
        (self.root / "report.json").write_text("{}")
        self.assertFalse(report_valid(self.root, self.proof, self.subject))

    def test_non_object_report_is_rejected_without_crashing(self):
        path = self.root / "report.json"
        path.write_text("[]")
        self.proof["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
        self.assertFalse(report_valid(self.root, self.proof, self.subject))

    def test_case_counter_cannot_claim_more_passes_than_named_cases(self):
        self.change_report(lambda report: report.update(passed=2))
        self.assertTrue(any("no passing evidence" in error for error in self.errors()))

    def test_load_metrics_cannot_differ_from_verified_report(self):
        self.config["required_suites"].append("load")
        report = json.loads((self.root / "report.json").read_text())
        report.update(suite="load", metrics={**self.evidence["load"], "concurrency": 1})
        path = self.root / "load.json"
        path.write_text(json.dumps(report))
        self.evidence["suites"]["load"] = {"status": "passed", "report": "load.json", "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
        self.assertTrue(any("metrics are not bound" in error for error in self.errors()))

    def test_wrong_feature_cannot_reuse_evidence(self):
        self.proof["features"] = ["unrelated"]
        self.assertTrue(any("no passing evidence" in error for error in self.errors()))

    def test_another_passing_case_cannot_reuse_evidence(self):
        self.change_report(lambda report: report["cases"][0].update(id="another-case"))
        self.assertTrue(any("no passing evidence" in error for error in self.errors()))

    def test_failed_case_in_passing_aggregate_cannot_pass(self):
        self.change_report(lambda report: report["cases"][0].update(status="failed", differences=["usage differs"]))
        self.assertTrue(any("no passing evidence" in error for error in self.errors()))

    def test_duplicate_case_identity_cannot_pass(self):
        self.change_report(lambda report: report["cases"].append(dict(report["cases"][0])))
        self.assertTrue(any("no passing evidence" in error for error in self.errors()))

    def test_another_suite_cannot_reuse_a_passing_report(self):
        self.change_report(lambda report: report.update(suite="storage_foundation"))
        self.assertTrue(any(error.startswith("suite:") for error in self.errors()))

    def test_another_provider_cannot_reuse_a_live_report(self):
        self.change_report(lambda report: report.update(provider="anthropic"))
        self.assertTrue(any(error.startswith("live:") for error in self.errors()))

    def test_missing_live_provider_cannot_pass(self):
        self.evidence["live"] = {}
        self.assertTrue(any(error.startswith("live:") for error in self.errors()))

    def test_incomplete_load_cannot_pass(self):
        del self.evidence["load"]["authorization_leaks"]
        self.assertTrue(any("failure counters" in error for error in self.errors()))

    def test_nan_rss_cannot_pass(self):
        self.evidence["load"]["max_rss_growth_ratio"] = float("nan")
        self.assertTrue(any("exceeds limit" in error for error in self.errors()))

    def test_boolean_failure_counter_cannot_pass(self):
        self.evidence["load"]["authorization_leaks"] = False
        self.assertTrue(any("failure counters" in error for error in self.errors()))

    def test_unproven_exclusion_cannot_pass(self):
        self.implementation["features"]["chat"] = {"scope": "enterprise_only", "reason": "unverified"}
        self.assertTrue(any("exclusion" in error for error in self.errors()))

    def test_path_traversal_report_cannot_pass(self):
        self.proof["report"] = "../report.json"
        self.assertFalse(report_valid(self.root, self.proof, self.subject))


if __name__ == "__main__":
    unittest.main()
