#!/usr/bin/env python3
"""Synthetic upstream with a resettable request trace. For isolated tests only."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import threading


class State:
    def __init__(self):
        self.lock = threading.Lock()
        self.fixture = None
        self.requests = []


def server(address):
    state = State()

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        def respond(self, status, body, headers=None):
            data = body if isinstance(body, bytes) else json.dumps(body).encode()
            self.send_response(status)
            for key, value in (headers or {"Content-Type": "application/json"}).items():
                self.send_header(key, value)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            self.wfile.flush()

        def do_GET(self):
            with state.lock:
                if self.path == "/control/requests":
                    return self.respond(200, state.requests)
            self.respond(200 if self.path == "/health" else 404, {})

        def do_POST(self):
            data = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            try:
                body = json.loads(data)
            except ValueError:
                body = data.decode(errors="replace")
            with state.lock:
                if self.path == "/control/reset":
                    state.fixture = body
                    state.requests = []
                    return self.respond(200, {})
                state.requests.append({"method": self.command, "path": self.path, "body": body,
                                       "headers": {key.lower(): value for key, value in self.headers.items()
                                                   if key.lower() in {"content-type", "anthropic-version", "anthropic-beta"}}})
                fixture = state.fixture
            if not fixture:
                return self.respond(503, {"error": {"message": "no test fixture selected"}})
            payload = fixture["body"]
            if isinstance(payload, str):
                payload = payload.encode()
            self.respond(fixture["status"], payload, fixture.get("headers"))

    return ThreadingHTTPServer(address, Handler)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", required=True)
    parser.add_argument("--port", type=int, required=True)
    args = parser.parse_args()
    server((args.host, args.port)).serve_forever()


if __name__ == "__main__":
    main()
