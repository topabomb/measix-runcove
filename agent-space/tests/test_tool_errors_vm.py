#!/usr/bin/env python3
"""Focused tool-result regression on an independent qualification Host.
Uses the same AGENT_SPACE_BROWSER_TEST_DIR fixture as test_browser_vm.py.
"""
import unittest
from browser_client import Client


def contents(result):
    return "\n".join(item["text"] for item in result.get("content", []) if item["type"] == "text")


class ToolErrors(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.client = Client("desktop")
        cls.client.initialize()
        cls.path = "/workspace/tool-error-qualification/input.txt"

    @classmethod
    def tearDownClass(cls):
        result = cls.client.tool("bash", {"cmd": "rm -rf -- /workspace/tool-error-qualification"})
        assert not result.get("isError"), contents(result)

    def test_file_errors_and_edit_guidance(self):
        result = self.client.tool("write", {"path": self.path, "content": "match\nmatch\n"})
        self.assertFalse(result.get("isError"), contents(result))
        result = self.client.tool("read", {"path": "/workspace/tool-error-qualification"})
        self.assertTrue(result.get("isError")); self.assertIn("directory", contents(result))
        result = self.client.tool("read", {"path": self.path + ".missing"})
        self.assertTrue(result.get("isError")); self.assertIn("File not found", contents(result))
        for old, expected in [("absent", "exact current text"), ("match", "surrounding text")]:
            result = self.client.tool("edit", {"path": self.path, "old_text": old, "new_text": "changed"})
            self.assertTrue(result.get("isError")); self.assertIn(expected, contents(result))
        result = self.client.tool("read", {"path": self.path})
        self.assertEqual(contents(result).count("match"), 2)

    def test_read_truncation_and_positive_ranges(self):
        result = self.client.tool("write", {"path": self.path, "content": "x" * 70000})
        self.assertFalse(result.get("isError"), contents(result))
        result = self.client.tool("read", {"path": self.path})
        self.assertFalse(result.get("isError"), contents(result))
        self.assertIn("[output truncated]", contents(result))
        self.assertGreater(len(contents(result)), 2048)
        for field in ["offset", "limit"]:
            try:
                result = self.client.tool("read", {"path": self.path, field: 0})
            except RuntimeError as error:
                self.assertIn("positive integers", str(error))
            else:
                self.assertTrue(result.get("isError")); self.assertIn("positive integers", contents(result))

    def test_command_output_is_not_shortened_as_an_error(self):
        result = self.client.tool("bash", {"cmd": "python3 -c \"print('x'*5000)\"; exit 7"})
        self.assertFalse(result.get("isError"), contents(result))
        self.assertIn("Exit code: 7", contents(result))
        self.assertGreater(len(contents(result)), 5000)
        result = self.client.tool("bash", {"cmd": "echo before-timeout; sleep 10", "timeout_secs": 1})
        self.assertTrue(result.get("isError"))
        self.assertIn("before-timeout", contents(result))
        self.assertIn("cleanup_confirmed", contents(result))
        self.assertIn("may_have_occurred", contents(result))

    def test_publication_errors_and_quoted_actions(self):
        result = self.client.tool("web_publish", {"action": "list"})
        self.assertFalse(result.get("isError"), contents(result))
        for extra, expected in [({}, "exactly one"), ({"port": 80}, "1024"), ({"port": 8080}, "reserved")]:
            try:
                result = self.client.tool("web_publish", {"action": "publish", "name": "tool-error-qualification", **extra})
            except RuntimeError as error:
                self.assertIn(expected, str(error))
            else:
                self.assertTrue(result.get("isError")); self.assertIn(expected, contents(result))


if __name__ == "__main__":
    unittest.main(verbosity=2)
