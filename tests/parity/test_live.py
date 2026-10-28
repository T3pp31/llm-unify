import copy
import json
from pathlib import Path
import sys
import unittest
from unittest import mock
from urllib.error import HTTPError

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "tools/parity"))
from run_live import LiveFailure, codex_provider, request, validate_response, validate_settings, validate_stream, validate_tool  # noqa: E402


def response():
    return {"id": "resp-test", "object": "response", "model": "served-local-model", "status": "completed",
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "OK"}]}],
            "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3,
                      "provider_metadata": "must-not-be-recorded"}}


def stream(events):
    return "".join("event: " + item["type"] + "\ndata: " + json.dumps(item) + "\n\n" for item in events)


class LiveTests(unittest.TestCase):
    def setUp(self):
        self.settings = json.loads((Path(__file__).resolve().parents[2] / "config/parity/live-settings.json").read_text())

    def test_completed_response_records_only_metrics_and_served_identity(self):
        result = validate_response(response())
        self.assertEqual(result["served_model"], "served-local-model")
        self.assertEqual(result["output_text_characters"], 2)
        self.assertNotIn("provider_metadata", result["usage"])
        self.assertNotIn("output", result)

    def test_missing_unknown_negative_boolean_and_inconsistent_usage_are_rejected(self):
        for usage in (None, {}, {"input_tokens": True, "output_tokens": 1, "total_tokens": 2},
                      {"input_tokens": -1, "output_tokens": 1, "total_tokens": 0},
                      {"input_tokens": 2, "output_tokens": 1, "total_tokens": 5}):
            value = response(); value["usage"] = usage
            with self.assertRaises(LiveFailure):
                validate_response(value)

    def test_http_success_with_incomplete_status_or_empty_text_is_not_a_pass(self):
        for field, value in [("status", "incomplete"), ("output", []), ("model", ""), ("id", "")]:
            body = response(); body[field] = value
            with self.assertRaises(LiveFailure):
                validate_response(body)

    def test_stream_checks_creation_order_usage_and_delta_reconstruction(self):
        raw = stream([{"type": "response.created", "sequence_number": 0},
                      {"type": "response.output_text.delta", "sequence_number": 1, "delta": "OK"},
                      {"type": "response.completed", "sequence_number": 2, "response": response()}])
        self.assertEqual(validate_stream(raw)["event_count"], 3)

    def test_truncated_error_failed_and_missing_terminal_streams_are_rejected(self):
        for raw in ('data: {"type":"response.created"}',
                    stream([{"type": "response.created"}]),
                    stream([{"type": "response.created"}, {"type": "error"}]),
                    stream([{"type": "response.created"}, {"type": "response.failed"}])):
            with self.assertRaises(LiveFailure):
                validate_stream(raw)

    def test_stream_duplicate_sequence_terminal_and_changed_deltas_are_rejected(self):
        events = [{"type": "response.created", "sequence_number": 0},
                  {"type": "response.output_text.delta", "sequence_number": 1, "delta": "OK"},
                  {"type": "response.completed", "sequence_number": 2, "response": response()}]
        duplicate = copy.deepcopy(events); duplicate[1]["sequence_number"] = 0
        changed = copy.deepcopy(events); changed[1]["delta"] = "different"
        for items in (duplicate, changed, [*events, events[-1]]):
            with self.assertRaises(LiveFailure):
                validate_stream(stream(items))
        with self.assertRaises(LiveFailure):
            validate_stream(stream(events) + "data: [DONE]\n\ndata: [DONE]\n\n")

    def test_tool_call_is_verified_without_executing_external_actions(self):
        body = response()
        body["output"] = [{"type": "function_call", "name": self.settings["tool"]["name"],
                           "call_id": "call-test", "arguments": json.dumps(self.settings["expected_tool_arguments"])}]
        result, call = validate_tool(body, self.settings)
        self.assertEqual(result["tool_calls"], 1)
        self.assertEqual(call["call_id"], "call-test")

    def test_wrong_tool_arguments_and_missing_call_identity_are_rejected(self):
        body = response()
        for call in ({"type": "function_call", "name": "wrong", "call_id": "x", "arguments": "{}"},
                     {"type": "function_call", "name": self.settings["tool"]["name"], "arguments": "{}"},
                     {"type": "function_call", "name": self.settings["tool"]["name"], "call_id": "x", "arguments": "invalid"}):
            body["output"] = [call]
            with self.assertRaises(LiveFailure):
                validate_tool(body, self.settings)

    def provider(self, base, extra="", env=None):
        config = f'model = "local-model"\nmodel_provider = "test"\n[model_providers.test]\nbase_url = "{base}"\nwire_api = "responses"\n{extra}'
        with mock.patch.object(Path, "read_text", return_value=config) as read:
            result = codex_provider("unused-config.toml", env=env or {})
            read.assert_called_once()  # No auth.json or OAuth token discovery.
            return result

    def test_loopback_codex_router_requires_no_oauth_or_auth_file(self):
        result = self.provider("http://127.0.0.1:8787/v1")
        self.assertTrue(result["local"])
        self.assertIsNone(result["api_key"])

    def test_explicit_remote_environment_key_is_used_only_for_configured_provider(self):
        result = self.provider("https://example.invalid/v1", 'env_key = "TEST_KEY"', {"TEST_KEY": "test-only-key"})
        self.assertEqual(result["api_key"], "test-only-key")
        self.assertFalse(result["local"])

    def test_remote_oauth_fallback_and_missing_key_are_rejected(self):
        for base, extra in [("https://example.invalid/v1", ""), ("https://example.invalid/v1", 'env_key = "MISSING"'),
                            ("http://example.invalid/v1", 'env_key = "TEST_KEY"')]:
            with self.assertRaises(LiveFailure):
                self.provider(base, extra)

    def test_credential_urls_and_ignored_header_settings_are_rejected(self):
        for base, extra in [("https://user:password@example.invalid/v1", ""), ("https://example.invalid/v1?key=secret", ""),
                            ("http://127.0.0.1:8787/v1", 'http_headers = { Authorization = "test-only" }')]:
            with self.assertRaises(LiveFailure):
                self.provider(base, extra)

    def test_invalid_settings_and_public_bind_do_not_silently_run(self):
        validate_settings(self.settings)
        for field, value in [("bind_host", "0.0.0.0"), ("max_output_tokens", True), ("max_response_bytes", 0), ("ignored", 1)]:
            settings = copy.deepcopy(self.settings); settings[field] = value
            with self.assertRaises(LiveFailure):
                validate_settings(settings)

    def test_non_responses_codex_provider_is_not_silently_treated_as_responses(self):
        config = 'model = "model"\nmodel_provider = "test"\n[model_providers.test]\nbase_url = "http://127.0.0.1:8787/v1"\nwire_api = "chat"'
        with mock.patch.object(Path, "read_text", return_value=config), self.assertRaises(LiveFailure):
            codex_provider("unused-config.toml", env={})

    def test_http_errors_do_not_expose_secret_response_or_redirect_location(self):
        error = HTTPError("https://example.invalid/?key=test-secret", 401, "test-secret", {}, None)
        with mock.patch("run_live.build_opener") as opener:
            opener.return_value.open.side_effect = error
            with self.assertRaises(LiveFailure) as caught:
                request("http://127.0.0.1/v1/responses", {}, "test-token", self.settings)
        self.assertEqual(str(caught.exception), "live request returned HTTP 401")


if __name__ == "__main__":
    unittest.main()
