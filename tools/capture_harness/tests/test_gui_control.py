"""Regression tests for the opt-in GUI control client."""
import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "gui_control", Path(__file__).resolve().parents[1] / "gui_control.py"
)
CONTROL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONTROL)


class ReplyAddressTests(unittest.TestCase):
    def test_darwin_reply_address_includes_terminator(self):
        self.check_address("darwin", True)

    def test_linux_reply_address_uses_normal_path(self):
        self.check_address("linux", False)

    def check_address(self, platform, terminated):
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(CONTROL.sys, "platform", platform):
                with patch.object(CONTROL.socket, "socket") as factory:
                    client = factory.return_value.__enter__.return_value
                    client.recv.return_value = b'{"ok":true,"result":{"received":true}}'
                    result = CONTROL.request(Path(directory), {"command": "status"})
                    self.assertEqual(result, {"received": True})
                    bound = client.bind.call_args.args[0]
                    self.assertEqual(bound.endswith("\0"), terminated)
                    self.assertTrue(bound.rstrip("\0").endswith("/reply"))
                    self.assertEqual(
                        client.sendto.call_args.args[1],
                        str(Path(directory) / "control.sock"),
                    )


if __name__ == "__main__":
    unittest.main()
