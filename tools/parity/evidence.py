#!/usr/bin/env python3
"""Register only current, successful named cases and component-level test reports.

Partial component results never stand in for a full acceptance suite, live API
verification or load metrics. This tool does not promote implementation status.
"""
import argparse
import json
from pathlib import Path
import sys

from gate import digest, local_file, project_digest, report_valid
from source import ROOT, baseline


def build(config, implementation, case_reports, component_reports, *, root=ROOT, subject=None):
    subject = subject or project_digest(root)
    reference = {key: config[key] for key in ("version", "commit", "archive_sha256")}
    manifest = {"schema_version": 1, "baseline": reference, "subject": subject,
                "cases": {}, "components": {}, "suites": {}, "live": {}, "load": {}}
    mapping = {}
    for feature, record in implementation["features"].items():
        if record.get("scope") == "oss":
            for case in record.get("verification_cases", []):
                mapping.setdefault(case, []).append(feature)
    def proof(name):
        path = local_file(root, name)
        result = {"status": "passed", "report": name, "sha256": digest(path)}
        if not report_valid(root, result, subject, reference):
            raise ValueError("stale, failed, empty or mismatched report: " + name)
        return result, json.loads(path.read_text())
    for name in case_reports:
        result, report = proof(name)
        cases = report.get("cases")
        if not isinstance(cases, list) or not cases:
            raise ValueError("case report contains no named cases: " + name)
        if report.get("suite") != "differential":
            raise ValueError("case report must be a differential comparison: " + name)
        for case in cases:
            identifier = case.get("id") if isinstance(case, dict) else None
            if identifier not in mapping:
                raise ValueError("case has no reviewed feature mapping: " + str(identifier))
            if identifier in manifest["cases"]:
                raise ValueError("duplicate case proof: " + identifier)
            if not report_valid(root, result, subject, reference, case=identifier):
                raise ValueError("case did not pass comparison: " + identifier)
            manifest["cases"][identifier] = {**result, "features": sorted(mapping[identifier])}
    for name in component_reports:
        result, report = proof(name)
        suite = report.get("suite")
        if not isinstance(suite, str) or not suite:
            raise ValueError("component report needs a suite identity: " + name)
        if suite in config["required_suites"]:
            raise ValueError("use a component-specific suite identity instead of full acceptance: " + suite)
        if suite in manifest["components"]:
            raise ValueError("duplicate component proof: " + suite)
        manifest["components"][suite] = result
    if not manifest["cases"] and not manifest["components"]:
        raise ValueError("no passing case or component evidence supplied")
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cases-report", action="append", default=[])
    parser.add_argument("--component-report", action="append", default=[])
    args = parser.parse_args()
    try:
        config = baseline()
        implementation = json.loads((ROOT / config["implementation"]).read_text())
        manifest = build(config, implementation, args.cases_report, args.component_report)
        output = local_file(ROOT, config["evidence"])
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps({"cases": len(manifest["cases"]), "components": list(manifest["components"]), "full_acceptance": False}))
        return 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
