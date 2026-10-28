#!/usr/bin/env python3
"""Smoke-test the shipped Compose in an isolated project, without upstream inference."""
import argparse
import json
from pathlib import Path
import secrets
import subprocess
import tempfile
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', required=True)
    parser.add_argument('--settings', type=Path, default=ROOT / 'config/container-test.json')
    parser.add_argument('--output', type=Path, default=ROOT / 'target/container-smoke.json')
    args = parser.parse_args()
    settings = json.loads(args.settings.read_text())
    for name in ['request_timeout_seconds', 'startup_timeout_seconds', 'poll_interval_seconds']:
        if type(settings.get(name)) not in (int, float) or not 0 < settings[name] < float('inf'):
            raise ValueError('invalid container smoke-test timing: ' + name)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.unlink(missing_ok=True)
    values = dict(line.split('=', 1) for line in (ROOT / 'deploy/.env.example').read_text().splitlines()
                  if line and not line.startswith('#'))
    master = 'sk-' + secrets.token_urlsafe(32)
    upstream = secrets.token_urlsafe(32)
    values.update(LLM_UNIFY_IMAGE=args.image, PROXY_PORT='0', LLM_UNIFY_MASTER_KEY=master,
                  UPSTREAM_API_BASE='http://127.0.0.1:9/v1', UPSTREAM_API_KEY=upstream)
    project = 'llm-unify-smoke-' + secrets.token_hex(6)
    checks = []
    report = {'status': 'failed', 'image': args.image, 'checks': checks}
    with tempfile.TemporaryDirectory(prefix='llm-unify-container-') as directory:
        env_file = Path(directory) / '.env'
        env_file.write_text(''.join(f'{key}={value}\n' for key, value in values.items()))
        env_file.chmod(0o600)
        compose = ['docker', 'compose', '--env-file', str(env_file), '-f', str(ROOT / 'deploy/compose.yaml'), '-p', project]
        log_file = args.output.with_suffix('.log')
        with log_file.open('w') as log:
            def command(*command_args):
                return subprocess.run([str(arg) for arg in command_args], cwd=ROOT, check=True, capture_output=True, text=True).stdout.strip()

            def request(path, token=None):
                headers = {'Authorization': 'Bearer ' + token} if token else {}
                try:
                    with urlopen(Request(base + path, headers=headers), timeout=settings['request_timeout_seconds']) as response:
                        return response.status, response.headers, response.read()
                except HTTPError as error:
                    return error.code, error.headers, error.read()

            def wait_ready():
                deadline = time.monotonic() + settings['startup_timeout_seconds']
                while time.monotonic() < deadline:
                    try:
                        status, _, body = request('/health/readiness')
                        if status == 200 and json.loads(body).get('status') == 'ready':
                            return
                    except (OSError, ValueError):
                        pass
                    time.sleep(settings['poll_interval_seconds'])
                raise RuntimeError('container readiness timed out')

            try:
                subprocess.run(compose + ['up', '-d', '--no-build', '--pull', 'never'], cwd=ROOT, stdout=log, stderr=log, check=True)
                address = command(*compose, 'port', 'proxy', values['PROXY_CONTAINER_PORT'])
                base = 'http://127.0.0.1:' + address.rsplit(':', 1)[1]
                wait_ready()
                checks.append('readiness')
                for token in [None, 'invalid-test-key']:
                    status, _, body = request('/ui/api/status', token)
                    assert status == 401, 'unauthorized console access accepted'
                    assert master.encode() not in body and upstream.encode() not in body
                checks.append('unauthorized_access_denied')
                status, headers, body = request('/ui/api/status', master)
                payload = json.loads(body)
                assert status == 200 and payload['product'] == 'LLM Unify'
                assert payload['models'] == ['main']
                assert headers.get('Cache-Control') == 'no-store'
                assert all(secret.encode() not in body for secret in [master, upstream, values['UPSTREAM_API_BASE']])
                checks.append('authenticated_configuration_without_secrets')
                for path in ['/ui/', '/ui/help', '/ui/app.js', '/ui/style.css']:
                    status, headers, body = request(path)
                    assert status == 200, 'console resource unavailable: ' + path
                    assert headers.get('X-Content-Type-Options') == 'nosniff'
                    assert 'frame-ancestors' in headers.get('Content-Security-Policy', '')
                    assert master.encode() not in body and upstream.encode() not in body
                    if path.endswith('app.js'):
                        assert b'__REQUEST_TIMEOUT_MS__' not in body, 'unrendered request timeout'
                checks.append('console_resources_and_security_headers')
                container = command(*compose, 'ps', '-q', 'proxy')
                inspect = json.loads(command('docker', 'inspect', container))[0]
                assert inspect['Config']['User'] == '65532:65532'
                assert inspect['HostConfig']['ReadonlyRootfs'] is True
                assert 'ALL' in inspect['HostConfig']['CapDrop']
                assert any(value.startswith('no-new-privileges') for value in inspect['HostConfig']['SecurityOpt'])
                checks.append('nonroot_readonly_restricted_runtime')
                subprocess.run(compose + ['restart', 'proxy'], cwd=ROOT, stdout=log, stderr=log, check=True)
                # Docker can allocate a different ephemeral host port on restart.
                address = command(*compose, 'port', 'proxy', values['PROXY_CONTAINER_PORT'])
                base = 'http://127.0.0.1:' + address.rsplit(':', 1)[1]
                wait_ready()
                assert request('/ui/api/status', master)[0] == 200
                checks.append('restart_recovery')
                report['status'] = 'passed'
            except (OSError, ValueError, AssertionError, RuntimeError, subprocess.CalledProcessError) as error:
                report['error'] = str(error).replace(master, '[REDACTED]').replace(upstream, '[REDACTED]')
            finally:
                cleanup = subprocess.run(compose + ['down', '--volumes', '--remove-orphans'], cwd=ROOT, stdout=log, stderr=log)
                if cleanup.returncode:
                    report.update(status='failed', error='owned test project cleanup failed')
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'status': report['status'], 'checks': len(checks), 'report': str(args.output)}))
    return 0 if report['status'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
