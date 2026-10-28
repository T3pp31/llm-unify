#!/usr/bin/env python3
"""Launch isolated local comparison processes and always clean them up.

Python is used exclusively for the pinned reference and test harness. The native
server is the same Rust binary as deployment. Container tests are separate.
"""
import argparse
import hashlib
from http.client import HTTPException
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import sys
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.request import urlopen

from gate import project_digest
from source import ROOT, baseline, verify


def available_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_ready(url, process, seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if process is not None and process.poll() is not None:
            raise RuntimeError("server exited during startup")
        try:
            with urlopen(url, timeout=1):
                return
        except HTTPError:
            return
        except (URLError, OSError, HTTPException):
            time.sleep(0.1)
    raise RuntimeError("server readiness deadline exceeded")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    reference = parser.add_mutually_exclusive_group(required=True)
    reference.add_argument("--python", type=Path, help="test venv containing the frozen Python dependencies")
    reference.add_argument("--reference-image", help="run the Python reference inside this local Docker image")
    parser.add_argument("--uv-version", help="uv version to install inside the reference container")
    parser.add_argument("--native-bin", type=Path, help="must match the executable produced by this run's cargo build")
    parser.add_argument("--startup-seconds", type=float, default=60)
    args = parser.parse_args()
    output = ROOT / "target/parity"
    output.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="local-", dir=output))
    mock_port, reference_port, native_port = [available_port() for _ in range(3)]
    mock_url = f"http://127.0.0.1:{mock_port}"
    reference_url = f"http://127.0.0.1:{reference_port}"
    native_url = f"http://127.0.0.1:{native_port}"
    environment = {key: value for key, value in os.environ.items() if key in {"PATH", "HOME", "TMPDIR", "SYSTEMROOT", "SSL_CERT_FILE", "SSL_CERT_DIR"}}
    environment.update({"PYTHONPATH": os.pathsep.join([str(ROOT / "vendor/litellm"), str(ROOT / "vendor/litellm/litellm-proxy-extras")]),
                        "LITELLM_RUST": "false", "LITELLM_LOCAL_MODEL_COST_MAP": "True",
                        "PARITY_UPSTREAM_URL": mock_url + "/v1", "PARITY_UPSTREAM_KEY": "synthetic-upstream-key",
                        "PARITY_MASTER_KEY": "sk-" + secrets.token_urlsafe(48),
                        "CONFIG_FILE_PATH": str(ROOT / "config/parity/proxy.yaml")})
    processes = []
    logs = []
    container = None
    subject = project_digest()
    report_path = output / "differential.json"
    execution = {"reference": {"kind": "container" if args.reference_image else "local_test_venv"},
                 "cargo_lock_sha256": hashlib.sha256((ROOT / "Cargo.lock").read_bytes()).hexdigest()}
    # Never leave a previous passing result available when this run cannot start.
    report_path.unlink(missing_ok=True)

    def launch(name, command, url):
        log = (work / (name + ".log")).open("w")
        logs.append(log)
        process = subprocess.Popen([str(arg) for arg in command], cwd=ROOT, env=environment, stdout=log, stderr=subprocess.STDOUT)
        processes.append(process)
        wait_ready(url + "/health/liveliness", process, args.startup_seconds)

    try:
        verify(baseline())
        # Binding a report to current sources requires building the current binary.
        build_log = work / "build.log"
        with build_log.open("w") as log:
            subprocess.run(["cargo", "build", "--locked", "-p", "llm-unify-proxy", "--message-format=json-render-diagnostics"], cwd=ROOT,
                           stdout=log, stderr=subprocess.STDOUT, check=True)
        executables = []
        for line in build_log.read_text().splitlines():
            try:
                item = json.loads(line)
            except ValueError:
                continue
            if item.get("reason") == "compiler-artifact" and item.get("target", {}).get("name") == "llm-unify-proxy" and item.get("executable"):
                executables.append(Path(item["executable"]))
        if len(executables) != 1:
            raise RuntimeError("cargo did not identify exactly one native proxy executable")
        native_bin = executables[0]
        if args.native_bin is not None and args.native_bin.resolve() != native_bin.resolve():
            raise ValueError("--native-bin must match the executable built from the current source")
        launch("mock", [sys.executable, ROOT / "tools/parity/mock.py", "--host", "0.0.0.0" if args.reference_image else "127.0.0.1", "--port", mock_port], mock_url)
        if args.reference_image:
            if not args.uv_version:
                raise ValueError("--uv-version is required for the container reference")
            execution["reference"].update(image=args.reference_image, uv_version=args.uv_version,
                image_id=subprocess.check_output(["docker", "image", "inspect", "--format", "{{.Id}}", args.reference_image], text=True).strip())
            requirements = work / "requirements.txt"
            subprocess.run(["uv", "export", "--frozen", "--no-dev", "--extra", "proxy", "--no-emit-workspace", "--project", ROOT / "vendor/litellm", "--output-file", requirements, "--quiet"], check=True)
            container_environment = {key: environment[key] for key in ["PARITY_MASTER_KEY", "PARITY_UPSTREAM_KEY", "LITELLM_RUST", "LITELLM_LOCAL_MODEL_COST_MAP"]}
            container_environment.update({"PYTHONPATH": "/reference:/reference/litellm-proxy-extras", "PYTHONDONTWRITEBYTECODE": "1", "CONFIG_FILE_PATH": "/fixture/proxy.yaml",
                                          "PARITY_UPSTREAM_URL": f"http://host.docker.internal:{mock_port}/v1", "PARITY_UV_VERSION": args.uv_version})
            command = ["docker", "run", "--detach", "--pull=never", "--publish", f"127.0.0.1:{reference_port}:4000",
                       "--add-host", "host.docker.internal:host-gateway", "--volume", f"{ROOT / 'vendor/litellm'}:/reference:ro",
                       "--volume", f"{ROOT / 'config/parity/proxy.yaml'}:/fixture/proxy.yaml:ro", "--volume", f"{requirements}:/tmp/requirements.txt:ro"]
            for key, value in container_environment.items():
                command.extend(["--env", key + "=" + value])
            command.extend([args.reference_image, "/bin/sh", "-ec",
                            'apt-get update && apt-get install -y --no-install-recommends libsndfile1 && python -m pip install --no-cache-dir "uv==$PARITY_UV_VERSION" && uv pip sync --system --require-hashes /tmp/requirements.txt && python -m uvicorn litellm.proxy.proxy_server:app --host 0.0.0.0 --port 4000'])
            container = subprocess.check_output(command, text=True).strip()
            wait_ready(reference_url + "/health/liveliness", None, args.startup_seconds)
        else:
            execution["reference"]["python_version"] = subprocess.check_output([str(args.python), "--version"], text=True).strip()
            launch("reference", [args.python, "-m", "uvicorn", "litellm.proxy.proxy_server:app", "--host", "127.0.0.1", "--port", reference_port], reference_url)
        launch("native", [native_bin, "--config", ROOT / "config/parity/proxy.yaml", "--listen", f"127.0.0.1:{native_port}"], native_url)
        execution["native_binary_sha256"] = hashlib.sha256(native_bin.read_bytes()).hexdigest()
        result = subprocess.run([sys.executable, ROOT / "tools/parity/compare.py", "--reference-url", reference_url, "--native-url", native_url,
                               "--mock-url", mock_url, "--subject", subject, "--token-env", "PARITY_MASTER_KEY"], cwd=ROOT, env=environment).returncode
        if project_digest() != subject:
            report_path.unlink(missing_ok=True)
            raise RuntimeError("source changed during the comparison run")
        if report_path.exists():
            report = json.loads(report_path.read_text())
            report["execution"] = execution
            report_path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
        return result
    except (OSError, RuntimeError, ValueError, subprocess.CalledProcessError) as error:
        print(f"comparison environment failed: {error}; logs: {work}", file=sys.stderr)
        return 2
    finally:
        if container:
            with (work / "reference-container.log").open("w") as log:
                subprocess.run(["docker", "logs", container], stdout=log, stderr=subprocess.STDOUT)
            subprocess.run(["docker", "rm", "--force", container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for process in reversed(processes):
            process.terminate()
        for process in reversed(processes):
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        for log in logs:
            log.close()


if __name__ == "__main__":
    sys.exit(main())
