#!/usr/bin/env python3
"""Run identical HTTP cases against the pinned Python proxy and native proxy.

Only explicit generated IDs and timestamps may be excluded, per case. Missing
fields, nulls, arrays, usage, statuses and upstream request traces remain strict.
"""
import argparse
import copy
from http.client import HTTPException
import json
from pathlib import Path
import os
import sys
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen

from source import ROOT, baseline


def sse_events(raw):
    events = []
    # Compare events rather than packet or whitespace boundaries.
    for block in raw.replace("\r\n", "\n").replace("\r", "\n").split("\n\n")[:-1]:
        event = {}
        data = []
        for line in block.splitlines():
            key, separator, value = line.partition(":")
            if not separator or line.startswith(":"):
                continue
            if value.startswith(" "):
                value = value[1:]
            if key == "data":
                data.append(value)
            elif key in {"event", "id", "retry"}:
                event[key] = value
        if data:
            text = "\n".join(data)
            event["data"] = text if text == "[DONE]" else json.loads(text)
            events.append(event)
    return events


def wait_ready(url, seconds):
    deadline = time.monotonic() + seconds
    while True:
        try:
            http(url + "/health/liveliness", timeout=1)
            return
        except (OSError, ValueError, HTTPException):
            if time.monotonic() >= deadline:
                raise TimeoutError("proxy startup deadline exceeded")
            time.sleep(0.1)


def http(url, method="GET", body=None, token=None, timeout=30):
    headers = {"Content-Type": "application/json"}
    if token is not None:
        headers["Authorization"] = "Bearer " + token
    request = Request(url, data=None if body is None else json.dumps(body).encode(), headers=headers, method=method)
    try:
        response = urlopen(request, timeout=timeout)
    except HTTPError as error:
        response = error
    with response:
        raw = response.read().decode()
        media_type = response.headers.get("Content-Type", "").split(";")[0].lower()
        payload = json.loads(raw) if media_type == "application/json" else sse_events(raw) if media_type == "text/event-stream" else raw
        return {"status": response.status, "content_type": media_type, "body": payload}


def normalize(response, exclusions):
    output = copy.deepcopy(response)
    for pointer in exclusions:
        if not isinstance(pointer, str) or not pointer.startswith("/"):
            raise ValueError("exclusion must be an explicit JSON pointer")
        keys = [key.replace("~1", "/").replace("~0", "~") for key in pointer[1:].split("/")]
        if keys[-1] not in {"id", "created", "created_at", "request_id"}:
            raise ValueError("only generated IDs and timestamps may be excluded: " + pointer)
        current = output["body"]
        for key in keys[:-1]:
            if isinstance(current, list) and key.isdigit():
                current = current[int(key)] if int(key) < len(current) else None
            elif isinstance(current, dict):
                current = current.get(key)
            else:
                current = None
        if isinstance(current, dict):
            current.pop(keys[-1], None)
    return output


def differences(left, right, path=""):
    if type(left) is not type(right):
        return [path + ": different types"]
    if isinstance(left, dict):
        result = [path + "/" + key + ": missing field" for key in sorted(left.keys() ^ right.keys())]
        for key in sorted(left.keys() & right.keys()):
            result.extend(differences(left[key], right[key], path + "/" + key))
        return result
    if isinstance(left, list):
        result = [] if len(left) == len(right) else [path + ": different lengths"]
        for index, (one, two) in enumerate(zip(left, right)):
            result.extend(differences(one, two, path + "/" + str(index)))
        return result
    return [] if left == right else [path + ": different values"]


def run(cases, reference, native, mock, token):
    results = []
    timeout = cases["request_timeout_seconds"]
    for case in cases["cases"]:
        observations = []
        for url in [reference, native]:
            http(mock + "/control/reset", "POST", case["upstream"], timeout=timeout)
            response = http(url + case["path"], case["method"], case["request"], token, timeout)
            trace = http(mock + "/control/requests", timeout=timeout)["body"]
            observations.append({"response": response, "upstream": trace})
        one, two = observations
        response_errors = differences(normalize(one["response"], case.get("exclude_response", [])), normalize(two["response"], case.get("exclude_response", [])), "/response")
        trace_errors = differences(one["upstream"], two["upstream"], "/upstream")
        # Identical local errors are not a successful inference comparison.
        expected_status = case.get("expected_status", case["upstream"]["status"])
        status_errors = [] if all(o["response"]["status"] == expected_status for o in observations) else ["response status differs from expected fixture status"]
        for observation in observations:
            response = observation["response"]
            if response["content_type"] == "text/event-stream":
                events = response["body"]
                if not events or events[-1].get("data") != "[DONE]":
                    status_errors.append("SSE stream has no terminal DONE event")
                if any(isinstance(event.get("data"), dict) and "error" in event["data"] for event in events):
                    status_errors.append("SSE stream contains an error event")
        errors = response_errors + trace_errors + status_errors
        results.append({"id": case["id"], "status": "failed" if errors else "passed", "differences": errors,
                        "excluded_response_fields": case.get("exclude_response", []), "observations": observations})
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reference-url", required=True)
    parser.add_argument("--native-url", required=True)
    parser.add_argument("--mock-url", required=True)
    parser.add_argument("--cases", default="config/parity/cases.json")
    parser.add_argument("--output", default="target/parity/differential.json")
    parser.add_argument("--subject", required=True)
    parser.add_argument("--token-env", required=True)
    parser.add_argument("--startup-seconds", type=float, default=0)
    args = parser.parse_args()
    output = ROOT / args.output
    output.parent.mkdir(parents=True, exist_ok=True)
    output.unlink(missing_ok=True)
    cases = json.loads((ROOT / args.cases).read_text())
    config = baseline()
    try:
        token = os.environ[args.token_env]
        for url in [args.reference_url, args.native_url, args.mock_url]:
            wait_ready(url, args.startup_seconds)
        results = run(cases, args.reference_url.rstrip("/"), args.native_url.rstrip("/"), args.mock_url.rstrip("/"), token)
    except (OSError, ValueError, KeyError, HTTPException) as error:
        # No passing report is written for an environment/setup error.
        print(f"comparison could not run ({type(error).__name__})", file=sys.stderr)
        return 2
    failed = sum(result["status"] != "passed" for result in results)
    payload = {"schema_version": 1, "suite": "differential", "subject": args.subject,
               "baseline": {key: config[key] for key in ["version", "commit", "archive_sha256"]},
               "status": "failed" if failed or not results else "passed", "passed": len(results) - failed,
               "failed": failed, "skipped": 0, "cases": results}
    output.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({key: payload[key] for key in ["status", "passed", "failed", "skipped"]}))
    return 1 if failed or not results else 0


if __name__ == "__main__":
    sys.exit(main())
