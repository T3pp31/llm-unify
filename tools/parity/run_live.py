#!/usr/bin/env python3
"""Test real Responses inference through the Rust Proxy using a Codex provider.

Reads only the active model/provider configuration. Never reads Codex OAuth or
auth.json. An environment API key is used only for its configured origin; an
unauthenticated loopback router can use the configured nonsecret placeholder.
This is a local-router component result, not proof for the seven cloud/Ollama
providers or a deterministic Python-vs-Rust differential comparison.
"""
import argparse
import hashlib
from http.client import HTTPException
import ipaddress
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import sys
import tempfile
import time
import tomllib
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener

from compare import sse_events
from gate import project_digest
from source import ROOT, baseline, verify


class LiveFailure(Exception):
    """Messages are fixed diagnostics, never upstream response bodies or URLs."""


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, new_url):
        return None


def validate_settings(settings):
    expected = {"schema_version", "bind_host", "public_model", "startup_timeout_seconds", "request_timeout_seconds",
                "max_response_bytes", "max_output_tokens", "reasoning_effort", "unauthenticated_local_api_key",
                "unary_prompt", "tool_prompt", "tool", "expected_tool_arguments", "tool_result"}
    if not isinstance(settings, dict) or set(settings) != expected or type(settings.get("schema_version")) is not int or settings["schema_version"] != 1:
        raise LiveFailure("invalid live settings schema or unknown/missing setting")
    for field in ("startup_timeout_seconds", "request_timeout_seconds", "max_response_bytes", "max_output_tokens"):
        if type(settings[field]) is not int or settings[field] <= 0:
            raise LiveFailure("live test limits must be positive integers")
    try:
        local = ipaddress.ip_address(settings["bind_host"]).is_loopback
    except (ValueError, TypeError):
        local = False
    if not local:
        raise LiveFailure("live test Proxy must bind a loopback IP address")
    for field in ("public_model", "reasoning_effort", "unauthenticated_local_api_key", "unary_prompt", "tool_prompt", "tool_result"):
        if not isinstance(settings[field], str) or not settings[field].strip():
            raise LiveFailure("live test string settings must be nonempty")
    if not isinstance(settings["tool"], dict) or not settings["tool"].get("name") or not isinstance(settings["expected_tool_arguments"], dict):
        raise LiveFailure("live tool definition and expected arguments are required")


def codex_provider(path, *, env=None):
    config = tomllib.loads(Path(path).read_text())
    provider = config.get("model_providers", {}).get(config.get("model_provider"), {})
    model = config.get("model")
    base = provider.get("base_url")
    if not isinstance(model, str) or not model or not isinstance(base, str):
        raise LiveFailure("Codex must select a model and a provider with base_url")
    if provider.get("wire_api") != "responses":
        raise LiveFailure("this live adapter requires a Responses provider")
    parsed = urlsplit(base)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise LiveFailure("provider URL must be HTTP(S), without userinfo, query or fragment")
    try:
        local = ipaddress.ip_address(parsed.hostname).is_loopback
    except ValueError:
        local = parsed.hostname.lower() == "localhost"
    if parsed.scheme == "http" and not local:
        raise LiveFailure("non-loopback provider requires HTTPS")
    # Custom headers could change authentication or tenancy; do not silently ignore them.
    for field in ("http_headers", "env_http_headers", "query_params", "experimental_bearer_token"):
        if provider.get(field):
            raise LiveFailure("provider uses additional authentication or query settings; an explicit adapter is required")
    environment = os.environ if env is None else env
    key_name = provider.get("env_key")
    key = environment.get(key_name) if isinstance(key_name, str) else None
    if key_name and (not isinstance(key, str) or not key.strip()):
        raise LiveFailure("configured upstream API key environment variable is missing or empty")
    if not key and not local:
        raise LiveFailure("remote provider requires an explicit API key environment variable; Codex OAuth is not reused")
    return {"base_url": base.rstrip("/"), "model": model, "api_key": key, "local": local,
            "provider": config["model_provider"], "wire_api": provider.get("wire_api")}


def text_output(response):
    return "".join(part.get("text", "") for item in response.get("output", [])
                   if isinstance(item, dict) and item.get("type") == "message"
                   for part in item.get("content", [])
                   if isinstance(part, dict) and part.get("type") == "output_text" and isinstance(part.get("text"), str))


def validate_response(response, *, require_text=True):
    if not isinstance(response, dict) or response.get("object") != "response" or response.get("status") != "completed":
        raise LiveFailure("Responses result is not a completed response")
    if not isinstance(response.get("id"), str) or not response["id"] or not isinstance(response.get("model"), str) or not response["model"]:
        raise LiveFailure("Responses result lacks response ID or served model identity")
    if not isinstance(response.get("output"), list):
        raise LiveFailure("Responses result lacks an output array")
    usage = response.get("usage")
    if not isinstance(usage, dict) or any(type(usage.get(field)) is not int or usage[field] < 0 for field in ("input_tokens", "output_tokens", "total_tokens")):
        raise LiveFailure("Responses usage is missing or invalid")
    if usage["total_tokens"] != usage["input_tokens"] + usage["output_tokens"]:
        raise LiveFailure("Responses usage totals are inconsistent")
    if require_text and not text_output(response).strip():
        raise LiveFailure("Responses completed without nonempty output text")
    # Record only whitelisted numeric metrics, never arbitrary provider fields.
    metrics = {field: usage[field] for field in ("input_tokens", "output_tokens", "total_tokens")}
    for name, field in (("input_tokens_details", "cached_tokens"), ("output_tokens_details", "reasoning_tokens")):
        details = usage.get(name)
        if isinstance(details, dict) and type(details.get(field)) is int and details[field] >= 0:
            metrics[name] = {field: details[field]}
    return {"served_model": response["model"], "usage": metrics, "output_items": len(response["output"]),
            "output_text_characters": len(text_output(response))}


def validate_stream(raw):
    if not raw.replace("\r\n", "\n").replace("\r", "\n").endswith("\n\n"):
        raise LiveFailure("Responses SSE ends in an incomplete event")
    try:
        events = sse_events(raw)
    except (ValueError, TypeError):
        raise LiveFailure("Responses SSE contains invalid JSON") from None
    last_sequence = -1
    completed = None
    done = False
    deltas = []
    kinds = []
    for event in events:
        data = event.get("data")
        if done:
            raise LiveFailure("Responses SSE has data after DONE")
        if data == "[DONE]":
            if completed is None:
                raise LiveFailure("Responses SSE finished before response.completed")
            done = True
            continue
        if not isinstance(data, dict) or not isinstance(data.get("type"), str):
            raise LiveFailure("Responses SSE event has no typed data")
        kind = data["type"]
        if completed is not None:
            raise LiveFailure("Responses SSE has data after its terminal response")
        if event.get("event", kind) != kind:
            raise LiveFailure("Responses SSE event name differs from its data type")
        if kind in {"error", "response.failed", "response.incomplete"} or data.get("error"):
            raise LiveFailure("Responses SSE reports an error or incomplete response")
        sequence = data.get("sequence_number")
        if sequence is not None:
            if type(sequence) is not int or sequence <= last_sequence:
                raise LiveFailure("Responses SSE sequence is not strictly increasing")
            last_sequence = sequence
        if kind == "response.output_text.delta":
            if not isinstance(data.get("delta"), str):
                raise LiveFailure("Responses SSE text delta is not a string")
            deltas.append(data["delta"])
        if kind == "response.completed":
            completed = data.get("response")
        kinds.append(kind)
    if not kinds or kinds[0] != "response.created" or completed is None:
        raise LiveFailure("Responses SSE lacks ordered created/completed events")
    result = validate_response(completed)
    if not deltas or "".join(deltas) != text_output(completed):
        raise LiveFailure("Responses SSE text deltas differ from its final response")
    result.update(event_count=len(events), event_types=kinds)
    return result


def validate_tool(response, settings):
    result = validate_response(response, require_text=False)
    calls = [item for item in response["output"] if isinstance(item, dict) and item.get("type") == "function_call"]
    if len(calls) != 1 or calls[0].get("name") != settings["tool"]["name"] or not isinstance(calls[0].get("call_id"), str) or not calls[0]["call_id"]:
        raise LiveFailure("forced tool response lacks the expected function call")
    try:
        arguments = json.loads(calls[0].get("arguments", ""))
    except (ValueError, TypeError):
        raise LiveFailure("tool call arguments are not valid JSON") from None
    if arguments != settings["expected_tool_arguments"]:
        raise LiveFailure("tool call arguments differ from the requested schema values")
    result["tool_calls"] = 1
    return result, calls[0]


def request(url, body, token, settings):
    data = json.dumps(body).encode()
    headers = {"Content-Type": "application/json", "Authorization": "Bearer " + token}
    started = time.monotonic()
    try:
        with build_opener(ProxyHandler({}), NoRedirect()).open(Request(url, data=data, headers=headers), timeout=settings["request_timeout_seconds"]) as response:
            raw = response.read(settings["max_response_bytes"] + 1)
            if len(raw) > settings["max_response_bytes"]:
                raise LiveFailure("live response exceeds the configured size limit")
            if response.status != 200:
                raise LiveFailure("live request did not return HTTP 200")
            media = response.headers.get("Content-Type", "").split(";")[0].lower()
            expected = "text/event-stream" if body.get("stream") else "application/json"
            if media != expected:
                raise LiveFailure("live response has an unexpected Content-Type")
            decoded = raw.decode("utf-8")
            return (decoded if body.get("stream") else json.loads(decoded)), round(time.monotonic() - started, 3)
    except HTTPError as error:
        # Do not echo authentication headers, response bodies or redirect Location.
        raise LiveFailure("live request returned HTTP " + str(error.code)) from None
    except (URLError, OSError, HTTPException, ValueError):
        raise LiveFailure("live transport, deadline or response decoding failed") from None


def run_cases(url, token, settings, requested_model):
    common = {"model": settings["public_model"], "max_output_tokens": settings["max_output_tokens"],
              "reasoning": {"effort": settings["reasoning_effort"]}}
    results = []
    tool_response = None
    call = None
    for name in ("responses.unary", "responses.stream", "responses.tool", "responses.tool_result"):
        observation = {"id": "local_router." + requested_model + "." + name, "requested_model": requested_model, "status": "failed"}
        try:
            if name == "responses.tool_result":
                if tool_response is None or call is None:
                    raise LiveFailure("tool result case cannot run because its prerequisite failed")
                body = {**common, "input": [{"role": "user", "content": settings["tool_prompt"]}, *tool_response["output"],
                        {"type": "function_call_output", "call_id": call["call_id"], "output": settings["tool_result"]}],
                        "tools": [settings["tool"]], "tool_choice": "none", "stream": False}
            elif name == "responses.tool":
                body = {**common, "input": settings["tool_prompt"], "tools": [settings["tool"]],
                        "tool_choice": {"type": "function", "name": settings["tool"]["name"]}, "stream": False}
            else:
                body = {**common, "input": settings["unary_prompt"], "stream": name == "responses.stream"}
            response, elapsed = request(url, body, token, settings)
            if name == "responses.stream":
                metrics = validate_stream(response)
            elif name == "responses.tool":
                metrics, call = validate_tool(response, settings)
                tool_response = response
            else:
                metrics = validate_response(response)
            observation.update(status="passed", duration_seconds=elapsed, **metrics)
        except LiveFailure as error:
            observation["error"] = str(error)
        results.append(observation)
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex-config", type=Path, required=True)
    parser.add_argument("--settings", type=Path, default=ROOT / "config/parity/live-settings.json")
    parser.add_argument("--output", type=Path, default=ROOT / "target/parity/live-local-router.json")
    parser.add_argument("--model", action="append", help="explicit configured upstream model; defaults to Codex's active model")
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.unlink(missing_ok=True)
    config = baseline()
    subject = project_digest()
    report = {"schema_version": 1, "suite": "local_router_live", "subject": subject,
              "baseline": {key: config[key] for key in ("version", "commit", "archive_sha256")},
              "status": "failed", "passed": 0, "failed": 1, "skipped": 0, "cases": [],
              "scope": "real Responses inference via Codex's configured router; not seven-provider acceptance"}
    process = None
    try:
        verify(config)
        settings = json.loads(args.settings.read_text())
        validate_settings(settings)
        upstream = codex_provider(args.codex_config)
        models = args.model or [upstream["model"]]
        if not models or any(not isinstance(model, str) or not model.strip() for model in models) or len(models) != len(set(models)):
            raise LiveFailure("upstream model names must be nonempty and unique")
        environment = {key: value for key, value in os.environ.items() if key in {"PATH", "HOME", "TMPDIR", "SYSTEMROOT", "SSL_CERT_FILE", "SSL_CERT_DIR"}}
        environment.update(LIVE_MASTER_KEY="sk-" + secrets.token_urlsafe(48),
                           LIVE_UPSTREAM_API_KEY=upstream["api_key"] or settings["unauthenticated_local_api_key"])
        report["upstream"] = {key: upstream[key] for key in ("base_url", "provider", "wire_api", "local")}
        report["authentication"] = "environment_key" if upstream["api_key"] else "local_router_no_credential_required"
        report["conditions"] = {key: settings[key] for key in ("max_output_tokens", "request_timeout_seconds", "max_response_bytes", "reasoning_effort")}
        with tempfile.TemporaryDirectory(prefix="llm-unify-live-") as directory:
            work = Path(directory)
            log_path = work / "build.log"
            with log_path.open("w") as log:
                subprocess.run(["cargo", "build", "--locked", "-p", "llm-unify-proxy", "--message-format=json-render-diagnostics"], cwd=ROOT,
                               stdout=log, stderr=subprocess.STDOUT, check=True)
            artifacts = []
            for line in log_path.read_text().splitlines():
                try:
                    item = json.loads(line)
                except ValueError:
                    continue
                if item.get("reason") == "compiler-artifact" and item.get("target", {}).get("name") == "llm-unify-proxy" and item.get("executable"):
                    artifacts.append(Path(item["executable"]))
            if len(artifacts) != 1:
                raise LiveFailure("cargo did not identify exactly one current Proxy executable")
            report["native_binary_sha256"] = hashlib.sha256(artifacts[0].read_bytes()).hexdigest()
            for model in models:
                with socket.socket() as listener:
                    listener.bind((settings["bind_host"], 0)); port = listener.getsockname()[1]
                fixture = {"model_list": [{"model_name": settings["public_model"], "litellm_params": {
                    "model": "openai/" + model, "api_base": upstream["base_url"], "api_key": "os.environ/LIVE_UPSTREAM_API_KEY"}}],
                    "general_settings": {"master_key": "os.environ/LIVE_MASTER_KEY"}}
                fixture_path = work / "proxy.json"
                fixture_path.write_text(json.dumps(fixture)); fixture_path.chmod(0o600)
                # No raw inference or Proxy logs are persisted; error output may contain provider data.
                process = subprocess.Popen([str(artifacts[0]), "--config", str(fixture_path), "--listen", f"{settings['bind_host']}:{port}"],
                                           cwd=ROOT, env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                url = f"http://{settings['bind_host']}:{port}"
                deadline = time.monotonic() + settings["startup_timeout_seconds"]
                while True:
                    if process.poll() is not None:
                        raise LiveFailure("Proxy exited before readiness")
                    try:
                        with build_opener(ProxyHandler({}), NoRedirect()).open(url + "/health/liveliness", timeout=1) as response:
                            if response.status == 200:
                                break
                    except (OSError, URLError, HTTPException):
                        pass
                    if time.monotonic() >= deadline:
                        raise LiveFailure("Proxy readiness deadline exceeded")
                    time.sleep(0.1)
                report["cases"].extend(run_cases(url + "/v1/responses", environment["LIVE_MASTER_KEY"], settings, model))
                process.terminate()
                try:
                    process.wait(timeout=settings["startup_timeout_seconds"])
                except subprocess.TimeoutExpired:
                    process.kill(); process.wait()
                process = None
        report["passed"] = sum(case["status"] == "passed" for case in report["cases"])
        report["failed"] = sum(case["status"] != "passed" for case in report["cases"])
        if project_digest() != subject:
            raise LiveFailure("source changed during live inference")
        report["status"] = "passed" if report["passed"] and not report["failed"] else "failed"
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError, LiveFailure) as error:
        # Arbitrary exception text can include a credential-bearing URL or source line.
        report.update(status="failed", failed=max(1, report["failed"]), error=str(error) if isinstance(error, LiveFailure) else "live setup or execution failed")
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait()
        args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({key: report[key] for key in ("suite", "status", "passed", "failed", "skipped")}))
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
