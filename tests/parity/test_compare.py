import copy
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "tools/parity"))
from compare import differences, normalize, sse_events  # noqa: E402


class ComparatorTests(unittest.TestCase):
    def test_missing_is_not_equivalent_to_null(self):
        self.assertTrue(differences({"usage": None}, {}))

    def test_usage_is_never_an_allowed_exclusion(self):
        with self.assertRaises(ValueError):
            normalize({"body": {"usage": 4}}, ["/usage"])

    def test_per_case_generated_id_exclusions_do_not_change_original(self):
        response = {"body": {"id": "first", "usage": {"prompt_tokens": 3}}}
        other = copy.deepcopy(response)
        other["body"]["id"] = "second"
        self.assertEqual(normalize(response, ["/id"]), normalize(other, ["/id"]))
        self.assertIn("id", response["body"])

    def test_sse_multiline_data_and_crlf_are_decoded_in_order(self):
        events = sse_events('event: message\r\ndata: {"delta":\r\ndata: "English 🌍"}\r\n\r\ndata: [DONE]\r\n\r\n')
        self.assertEqual(events, [{"event": "message", "data": {"delta": "English 🌍"}}, {"data": "[DONE]"}])

    def test_unterminated_sse_is_not_a_complete_event(self):
        self.assertEqual(sse_events('data: {"x":1}'), [])

    def test_event_order_and_tool_ids_are_significant(self):
        left = [{"data": {"tool_call_id": "a"}}, {"data": {"tool_call_id": "b"}}]
        self.assertTrue(differences(left, list(reversed(left))))


if __name__ == "__main__":
    unittest.main()
