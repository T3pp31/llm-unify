#!/usr/bin/env python3
"""Exercise real PostgreSQL/Redis, two independent clients, crashes and recovery.

Creates and removes only its own disposable containers and volumes. No user's
database, Redis namespace or credentials are used. Python is test tooling only.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import re
import secrets
import socket
import subprocess
import sys
import threading
import time
import uuid

from gate import project_digest
from source import ROOT, baseline


def command(arguments, *, env=None, timeout=900):
    result = subprocess.run(arguments, cwd=ROOT, env=env, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout)
    if result.returncode:
        # Arguments never contain database URLs or passwords.
        raise RuntimeError(f"command failed ({result.returncode}): {' '.join(arguments)}\n{result.stdout}")
    return result.stdout


def tests(arguments, env, log):
    output = command(arguments, env=env)
    log.write_text(output)
    summaries = re.findall(r"test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;", output)
    if not summaries:
        raise RuntimeError("test command did not emit a successful summary")
    passed = sum(int(summary[0]) for summary in summaries)
    if passed == 0 or any(int(failed) or int(ignored) for _, failed, ignored in summaries):
        raise RuntimeError("storage test run is empty, failed, or skipped cases")
    return passed


def wait_ready(name, check, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        ready = subprocess.run(["docker", "exec", name, *check], cwd=ROOT,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=timeout)
        if ready.returncode == 0:
            return
        time.sleep(0.2)
    raise RuntimeError("storage service readiness deadline exceeded: " + name)


def probe_events(process):
    events = queue.Queue()
    def read():
        for line in process.stdout:
            events.put(line.rstrip())
        events.put(None)
    threading.Thread(target=read, daemon=True).start()
    return events


def host_port(host):
    # Publish an explicit port so Docker reuses it across crash/restart. Let the
    # OS choose the initial free port; a bind race is a setup failure, not a pass.
    with socket.socket() as listener:
        listener.bind((host, 0))
        return listener.getsockname()[1]


def expect(events, expected, timeout):
    deadline = time.monotonic() + timeout
    output = []
    while True:
        try:
            line = events.get(timeout=max(0, deadline - time.monotonic()))
        except queue.Empty as error:
            raise RuntimeError(f"fault probe deadline waiting for {expected}: {output}") from error
        if line is None:
            raise RuntimeError(f"fault probe ended waiting for {expected}: {output}")
        if line == expected:
            return output + [line]
        output.append(line)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, default=ROOT / "target/parity/storage.json")
    args = parser.parse_args()
    config = json.loads((ROOT / "config/parity/storage-test.json").read_text())
    subject = project_digest()
    base = baseline()
    report = {"schema_version": 1, "suite": "storage_foundation", "subject": subject,
              "baseline": {key: base[key] for key in ("version", "commit", "archive_sha256")},
              "status": "failed", "passed": 0, "failed": 1, "skipped": 0,
              "conditions": {key: config[key] for key in ("concurrency", "rate_window_ms")}}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    containers = []
    probe = None
    try:
        images = {}
        for key in ("postgres_image", "redis_image"):
            images[key] = {"tag": config[key], "image_id": command(["docker", "image", "inspect", "--format", "{{.Id}}", config[key]]).strip()}
        report["images"] = images
        postgres = "llm-unify-pg-" + uuid.uuid4().hex
        redis = "llm-unify-redis-" + uuid.uuid4().hex
        password = secrets.token_urlsafe(32)
        host = config["bind_host"]
        explicit_pg_port = host_port(host)
        explicit_redis_port = host_port(host)
        while explicit_redis_port == explicit_pg_port:
            explicit_redis_port = host_port(host)
        env = dict(os.environ)
        env["POSTGRES_PASSWORD"] = password
        command(["docker", "run", "--detach", "--pull=never", "--name", postgres,
                 "--publish", f"{host}:{explicit_pg_port}:5432", "--env", "POSTGRES_PASSWORD",
                 "--env", "POSTGRES_USER=" + config["postgres_user"],
                 "--env", "POSTGRES_DB=" + config["postgres_database"], config["postgres_image"]], env=env)
        containers.append(postgres)
        command(["docker", "run", "--detach", "--pull=never", "--name", redis,
                 "--publish", f"{host}:{explicit_redis_port}:6379", config["redis_image"], *config["redis_command"]])
        containers.append(redis)
        timeout = config["startup_timeout_seconds"]
        pg_check = ["pg_isready", "-U", config["postgres_user"], "-d", config["postgres_database"]]
        redis_check = ["redis-cli", "ping"]
        wait_ready(postgres, pg_check, timeout)
        wait_ready(redis, redis_check, timeout)
        pg_port = command(["docker", "port", postgres, "5432/tcp"]).strip().rsplit(":", 1)[1]
        redis_port = command(["docker", "port", redis, "6379/tcp"]).strip().rsplit(":", 1)[1]
        env.pop("POSTGRES_PASSWORD")
        env[config["postgres"]["url_environment"]] = f"postgresql://{config['postgres_user']}:{password}@{host}:{pg_port}/{config['postgres_database']}"
        env[config["redis"]["url_environment"]] = f"redis://{host}:{redis_port}/"
        print("PostgreSQL/Redis ready; running storage unit and integration tests", flush=True)
        report["passed"] += tests(["cargo", "test", "--locked", "-p", "llm-unify-storage", "--lib"], env, args.report.parent / "storage-unit.log")
        report["passed"] += tests(["cargo", "test", "--locked", "-p", "llm-unify-storage", "--test", "postgres", "--test", "redis", "--", "--ignored"], env, args.report.parent / "storage-integration.log")
        build_output = command(["cargo", "build", "--locked", "-p", "llm-unify-storage", "--example", "fault_probe", "--message-format=json-render-diagnostics"], env=env)
        executables = []
        for line in build_output.splitlines():
            try:
                item = json.loads(line)
            except ValueError:
                continue
            if item.get("reason") == "compiler-artifact" and item.get("target", {}).get("name") == "fault_probe" and item.get("executable"):
                executables.append(item["executable"])
        if len(executables) != 1:
            raise RuntimeError("cargo did not identify exactly one fault probe executable")
        probe = subprocess.Popen([executables[0]], cwd=ROOT, env=env,
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        events = probe_events(probe)
        fault_log = expect(events, "ready", timeout)
        print("Stopping the disposable PostgreSQL/Redis services to verify fail-closed recovery", flush=True)
        for container in containers:
            command(["docker", "kill", "--signal", config["fault_signal"], container], timeout=timeout)
        probe.stdin.write("offline\n"); probe.stdin.flush()
        fault_log += expect(events, "offline-verified", timeout)
        for container in containers:
            command(["docker", "start", container], timeout=timeout)
        wait_ready(postgres, pg_check, timeout)
        wait_ready(redis, redis_check, timeout)
        restarted_pg_port = command(["docker", "port", postgres, "5432/tcp"]).strip().rsplit(":", 1)[1]
        restarted_redis_port = command(["docker", "port", redis, "6379/tcp"]).strip().rsplit(":", 1)[1]
        if (pg_port, redis_port) != (restarted_pg_port, restarted_redis_port):
            raise RuntimeError("Docker changed ephemeral host ports after restart; test fixture must use stable explicit host ports")
        probe.stdin.write("recovered\n"); probe.stdin.flush()
        fault_log += expect(events, "recovery-verified", timeout)
        if probe.wait(timeout=timeout):
            raise RuntimeError("fault probe failed after recovery")
        (args.report.parent / "storage-fault.log").write_text("\n".join(fault_log) + "\n")
        report["passed"] += 2
        report["faults"] = {"signal": config["fault_signal"], "services": ["postgres", "redis"],
                            "offline_errors_observed": True, "durable_reservation_recovered": True,
                            "persistent_rate_counter_recovered": True, "duplicate_settlement_charges": 0}
        if project_digest() != subject:
            raise RuntimeError("source changed during the storage test run")
        report.update(status="passed", failed=0)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        report["error"] = str(error)
        print(str(error), file=sys.stderr)
    finally:
        if probe is not None and probe.poll() is None:
            probe.terminate()
            try:
                probe.wait(timeout=5)
            except subprocess.TimeoutExpired:
                probe.kill(); probe.wait()
        cleanup_errors = []
        for container in reversed(containers):
            try:
                command(["docker", "rm", "--force", "--volumes", container], timeout=60)
            except (OSError, RuntimeError, subprocess.SubprocessError) as error:
                cleanup_errors.append(str(error))
        if cleanup_errors:
            report.update(status="failed", failed=1, cleanup_errors=cleanup_errors)
        args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({key: report[key] for key in ("suite", "status", "passed", "failed", "skipped")}), flush=True)
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
