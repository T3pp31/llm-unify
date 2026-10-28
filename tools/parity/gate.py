#!/usr/bin/env python3
"""Fail closed until every discovered feature and every acceptance suite has evidence."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import subprocess
import sys

from inventory import generate
from source import ROOT, baseline, verify


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def project_digest(root=ROOT):
    """Bind evidence to local, including dirty and untracked, implementation inputs."""
    paths = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=root
    ).decode().split("\0")
    state = hashlib.sha256()
    for name in sorted(set(paths) - {""}):
        # Vendor has its own complete verified manifest. Generated evidence lives in target.
        if name.startswith("vendor/"):
            continue
        path = root / name
        if path.is_symlink():
            raise ValueError("project input must not be a symlink: " + name)
        state.update(name.encode() + b"\0")
        state.update(bytes.fromhex(digest(path)) if path.is_file() else b"deleted")
    return state.hexdigest()


def local_file(root, name):
    if not isinstance(name, str) or not name or Path(name).is_absolute():
        raise ValueError("evidence path must be a relative local file")
    root = root.resolve()
    path = root / name
    descendants = [root.joinpath(*Path(name).parts[:i]) for i in range(1, len(Path(name).parts) + 1)]
    if ".." in Path(name).parts or any(p.is_symlink() for p in descendants):
        raise ValueError("unsafe evidence path")
    return path


def report_valid(root, proof, subject, expected_baseline=None, *, case=None, suite=None, provider=None):
    if not isinstance(proof, dict) or proof.get("status") != "passed":
        return False
    try:
        path = local_file(root, proof.get("report"))
        if digest(path) != proof.get("sha256"):
            return False
        report = json.loads(path.read_text())
        if not isinstance(report, dict):
            return False
        valid = (report.get("subject") == subject and report.get("status") == "passed"
                and (expected_baseline is None or report.get("baseline") == expected_baseline)
                and type(report.get("passed")) is int and report["passed"] > 0
                and type(report.get("failed")) is int and report["failed"] == 0
                and type(report.get("skipped")) is int and report["skipped"] == 0)
        if not valid or (suite is not None and report.get("suite") != suite) or (provider is not None and report.get("provider") != provider):
            return False
        if case is not None:
            cases = report.get("cases")
            if not isinstance(cases, list) or any(not isinstance(item, dict) for item in cases):
                return False
            ids = [item.get("id") for item in cases]
            if any(not isinstance(name, str) or not name for name in ids) or len(ids) != len(set(ids)):
                return False
            if len(cases) != report["passed"] or any(item.get("status") != "passed" or item.get("differences") != [] for item in cases):
                return False
            matches = [item for item in cases if item["id"] == case]
            if len(matches) != 1 or matches[0].get("status") != "passed" or matches[0].get("differences") != []:
                return False
        return True
    except (OSError, ValueError, TypeError):
        return False


def evaluate(config, inventory, implementation, evidence, root=ROOT, subject=None):
    errors = []
    subject = subject or project_digest(root)
    expected_baseline = {key: config[key] for key in ["version", "commit", "archive_sha256"]}
    if evidence.get("baseline") != expected_baseline:
        errors.append("evidence.baseline: missing or mismatched reference")
    if evidence.get("subject") != subject:
        errors.append("evidence.subject: stale or missing implementation digest")
    if implementation.get("schema_version") != 1 or evidence.get("schema_version") != 1:
        errors.append("schema_version: expected 1")
    features = implementation.get("features", {})
    if not isinstance(features, dict):
        return errors + ["implementation.features: expected object"]
    candidates = {f["id"]: f for f in inventory["features"]}
    for unknown in sorted(set(features) - set(candidates)):
        errors.append(unknown + ": not in pinned inventory")
    for identifier in candidates:
        item = features.get(identifier, {})
        if not isinstance(item, dict):
            errors.append(identifier + ": invalid implementation record")
            continue
        if item.get("scope") in config["allowed_exclusions"]:
            # A human reviewed exclusion needs an exact upstream location and a rationale.
            if not item.get("reason") or not item.get("scope_proof"):
                errors.append(identifier + ": exclusion needs reason and source proof")
            else:
                proof = item["scope_proof"]
                try:
                    path = local_file(root / config["destination"], proof.get("path"))
                    lines = path.read_text().splitlines()
                    number = proof.get("line")
                    if (not isinstance(number, int) or number < 1 or number > len(lines)
                            or not proof.get("text") or proof["text"] not in lines[number - 1]):
                        raise ValueError("invalid scope proof")
                except (OSError, ValueError, TypeError, AttributeError):
                    errors.append(identifier + ": invalid exclusion source proof")
            continue
        if item.get("scope") != "oss":
            errors.append(identifier + ": scope unreviewed")
            continue
        if item.get("status") != "implemented":
            errors.append(identifier + ": unimplemented")
        contract = item.get("contract", {})
        if not isinstance(contract, dict) or not all(isinstance(contract.get(k), str) and contract[k].strip() for k in ["input", "output"]):
            errors.append(identifier + ": missing reviewed input/output contract")
        cases = item.get("verification_cases")
        if not isinstance(cases, list) or not cases or len(cases) != len(set(cases)):
            errors.append(identifier + ": missing or duplicate verification cases")
            continue
        for case in cases:
            proof = evidence.get("cases", {}).get(case)
            if not report_valid(root, proof, subject, expected_baseline, case=case) or identifier not in proof.get("features", []):
                errors.append(identifier + ": no passing evidence for " + str(case))
    for suite in config["required_suites"]:
        if not report_valid(root, evidence.get("suites", {}).get(suite), subject, expected_baseline, suite=suite):
            errors.append("suite:" + suite + ": missing passing report")
    for provider in config["required_live_providers"]:
        if not report_valid(root, evidence.get("live", {}).get(provider), subject, expected_baseline, provider=provider):
            errors.append("live:" + provider + ": missing passing report")
    for component, proof in evidence.get("components", {}).items():
        if not report_valid(root, proof, subject, expected_baseline, suite=component):
            errors.append("component:" + component + ": missing passing report")
    load = evidence.get("load", {})
    if "load" in config["required_suites"]:
        proof = evidence.get("suites", {}).get("load")
        if report_valid(root, proof, subject, expected_baseline, suite="load"):
            report = json.loads(local_file(root, proof["report"]).read_text())
            if report.get("metrics") != load:
                errors.append("load: metrics are not bound to the verified load report")
    for field, expected in config["load"].items():
        actual = load.get(field)
        if field == "max_rss_growth_ratio":
            if type(actual) not in (int, float) or not 0 <= actual <= expected:
                errors.append("load:" + field + ": missing or exceeds limit")
        elif type(actual) is not int or actual != expected:
            errors.append("load:" + field + ": does not match acceptance conditions")
    if any(type(load.get(key)) is not int or load[key] != 0 for key in ["unexpected_failures", "duplicate_charges", "authorization_leaks", "leaked_reservations"]):
        errors.append("load: all failure counters must be present and zero")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--subject", action="store_true", help="print the current project digest")
    parser.add_argument("--output", default="target/parity/release.json")
    args = parser.parse_args()
    try:
        if args.subject:
            print(project_digest())
            return 0
        config = baseline()
        verify(config)
        inventory = json.loads((ROOT / config["inventory"]).read_text())
        if inventory != generate(config):
            raise ValueError("inventory is stale")
        implementation = json.loads((ROOT / config["implementation"]).read_text())
        evidence_path = ROOT / config["evidence"]
        evidence = json.loads(evidence_path.read_text()) if evidence_path.exists() else {}
        errors = evaluate(config, inventory, implementation, evidence)
        result = {"schema_version": 1, "release_ready": not errors, "baseline": inventory["baseline"],
                  "discovered": len(inventory["features"]), "counts": inventory["counts"],
                  "blocking_count": len(errors), "blocking": errors}
        output = local_file(ROOT, args.output)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps({k: v for k, v in result.items() if k != "blocking"}, ensure_ascii=False))
        return 0 if not errors else 1
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
