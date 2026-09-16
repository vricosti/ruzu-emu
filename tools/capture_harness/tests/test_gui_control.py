"""Regression tests for the opt-in GUI control client."""
import importlib.util
import copy
import json
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


class RecordingTests(unittest.TestCase):
    def recording(self):
        return {"version": 1, "kind": "ruzu-gui-buttons", "complete": True,
                "program_id": 42, "duration_us": 100000,
                "events": [{"at_us": 0, "buttons": []},
                           {"at_us": 1000, "buttons": ["L", "R"]},
                           {"at_us": 2000, "buttons": []}]}

    def status(self):
        return {"has_session": True, "program_id": 42, "generation": 1,
                "pid": 123, "buttons": [False]*22}

    def test_native_button_order(self):
        status = self.status()
        for index in (0, 6, 7):
            status["buttons"][index] = True
        self.assertEqual(CONTROL.snapshot(status), ["A", "L", "R"])
        status["buttons"][18] = True
        self.assertEqual(CONTROL.snapshot(status), ["A", "L", "R"])

    def test_rejects_invalid_recordings(self):
        valid = self.recording()
        self.assertEqual(CONTROL.validate_recording(valid), valid["events"])
        for invalid in [None, [], dict(valid, events=[None])]:
            with self.assertRaises(ValueError):
                CONTROL.validate_recording(invalid)
        for changes in [{"complete": False}, {"version": 2}, {"duration_us": -1},
                        {"events": []}, {"program_id": 0}]:
            with self.assertRaises(ValueError):
                CONTROL.validate_recording(dict(valid, **changes))
        for event in [{"at_us": 0, "buttons": []}, {"at_us": -1, "buttons": []},
                      {"at_us": 500, "buttons": ["A", "A"]},
                      {"at_us": 500, "buttons": ["HOME"]}]:
            invalid = copy.deepcopy(valid)
            invalid["events"][1] = event
            with self.assertRaises(ValueError):
                CONTROL.validate_recording(invalid)

    def test_replay_releases_on_injection_error(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "buttons.json"
            source.write_text(json.dumps(self.recording()))
            sent = []
            def send(_, payload):
                sent.append(payload)
                if payload["command"] == "set_buttons":
                    raise RuntimeError("injection failed")
                return self.status()
            with patch.object(CONTROL, "request", side_effect=send):
                with self.assertRaisesRegex(RuntimeError, "injection failed"):
                    CONTROL.replay(Path(directory), source)
            self.assertEqual(sent[-1], {"command": "release"})

    def test_record_interrupt_saves_and_does_not_overwrite(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "buttons.json"
            with patch.object(CONTROL, "request", return_value=self.status()), \
                 patch.object(CONTROL.time, "sleep", side_effect=KeyboardInterrupt):
                CONTROL.record(Path(directory), destination, 1)
                data = destination.read_bytes()
                self.assertTrue(json.loads(data)["complete"])
                with self.assertRaises(FileExistsError):
                    CONTROL.record(Path(directory), destination, 1)
                self.assertEqual(destination.read_bytes(), data)

    def test_replay_refuses_wrong_game_before_injection(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "buttons.json"
            source.write_text(json.dumps(self.recording()))
            status = dict(self.status(), program_id=43)
            with patch.object(CONTROL, "request", return_value=status) as send:
                with self.assertRaisesRegex(ValueError, "wrong game"):
                    CONTROL.replay(Path(directory), source)
                self.assertEqual(send.call_count, 1)

    def test_session_change_leaves_incomplete_recording(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "buttons.json"
            statuses = [self.status(), dict(self.status(), generation=2)]
            with patch.object(CONTROL, "request", side_effect=statuses), \
                 patch.object(CONTROL.time, "sleep"):
                with self.assertRaisesRegex(RuntimeError, "session changed"):
                    CONTROL.record(Path(directory), output, 1)
            self.assertFalse(json.loads(output.read_text())["complete"])


if __name__ == "__main__":
    unittest.main()
