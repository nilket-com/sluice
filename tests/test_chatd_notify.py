"""Adapter regression tests using a fake chatctl; no real messages are sent."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

HOOK = Path(__file__).resolve().parents[1] / "examples/chatd-notify.py"


class ChatdHookTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name)
        fake = self.path / "chatctl"
        fake.write_text('''#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
if "status" in args:
    peers = [{"name": "sluice"}, {"name": "codex", "transport": "push", "path_ready": True,
              "wake_kinds": ["request"], "session": "intended-session"}]
    if os.environ.get("NO_PATH"): peers[1]["path_ready"] = False
    print(json.dumps({"participants": peers}))
elif "lookup" in args:
    with open(os.environ["RECORD"], "w") as f: json.dump({"args": args}, f)
    print(json.dumps({"id": "stored-event-id"}))
elif "send" in args:
    if os.environ.get("SEND_ERROR"):
        print("UnknownParticipant: unknown participant", file=sys.stderr)
        sys.exit(5)
    with open(os.environ["RECORD"], "w") as f:
        json.dump({"args": args, "body": json.load(sys.stdin)}, f)
    warning = os.environ.get("SEND_WARNING")
    if os.environ.get("SEND_STDERR"): print("health warning from chatctl", file=sys.stderr)
    print(json.dumps({"id": "test-message-id", "warning": warning}))
else: sys.exit(2)
''')
        fake.chmod(0o755)
        self.env = {**os.environ, "PATH": str(self.path) + os.pathsep + os.environ["PATH"],
                    "RECORD": str(self.path / "record.json")}

    def invoke(self, *extra, event=None):
        return subprocess.run(
            [sys.executable, str(HOOK), "--sender", "sluice", "--to", "codex",
             "--conversation", "test", *extra],
            input=json.dumps(event) if event else "", capture_output=True,
            text=True, env=self.env, timeout=5,
        )

    def test_default_end_alert_uses_waking_request(self):
        result = self.invoke(event={"type": "end", "reason": "exit", "exit_code": 0})
        self.assertEqual(result.returncode, 0, result.stderr)
        record = json.loads((self.path / "record.json").read_text())
        self.assertEqual(record["args"][record["args"].index("--kind") + 1], "request")
        self.assertEqual(record["body"]["exit_code"], 0)
        self.assertIn("test-message-id", result.stderr)

    def test_self_notification_is_rejected_before_chatctl(self):
        result = self.invoke("--sender", "codex", event={"type": "end", "reason": "exit"})
        self.assertEqual(result.returncode, 2)
        self.assertIn("must be distinct", result.stderr)
        self.assertFalse((self.path / "record.json").exists())

    def test_stable_event_id_is_used_as_transport_idempotency_key(self):
        result = self.invoke(event={"type": "watch", "reason": "match", "event_id": "run-abc:2", "watch": "GATE_COMPLETE"})
        self.assertEqual(result.returncode, 0, result.stderr)
        args = json.loads((self.path / "record.json").read_text())["args"]
        self.assertEqual(args[args.index("--idempotency-key") + 1], "run-abc:2")
        self.assertIn("GATE_COMPLETE", args[args.index("--summary") + 1])

    def test_cancellation_signal_takes_precedence_over_child_exit(self):
        result = self.invoke(event={"type": "end", "reason": "signal", "exit_code": 0, "signal": 15})
        self.assertEqual(result.returncode, 0, result.stderr)
        args = json.loads((self.path / "record.json").read_text())["args"]
        self.assertIn("signal=15", args[args.index("--summary") + 1])
        self.assertNotIn("exit=0", args[args.index("--summary") + 1])

    def test_lookup_is_read_only_and_does_not_resend(self):
        result = self.invoke("--lookup-event", "run-abc:2")
        self.assertEqual(result.returncode, 0, result.stderr)
        args = json.loads((self.path / "record.json").read_text())["args"]
        self.assertIn("lookup", args)
        self.assertNotIn("send", args)
        self.assertIn("not recipient acknowledgement", result.stdout)

    def test_invalid_event_id_is_rejected_without_transport(self):
        result = self.invoke(event={"type": "end", "reason": "exit", "event_id": "bad key"})
        self.assertEqual(result.returncode, 1)
        self.assertFalse((self.path / "record.json").exists())

    def test_info_is_an_explicit_opt_in(self):
        self.assertEqual(self.invoke("--kind", "info", event={"type": "watch", "reason": "match"}).returncode, 0)
        record = json.loads((self.path / "record.json").read_text())
        self.assertIn("info", record["args"])

    def test_routine_batch_does_not_send(self):
        self.assertEqual(self.invoke(event={"type": "batch"}).returncode, 0)
        self.assertFalse((self.path / "record.json").exists())

    def test_preflight_checks_target_without_sending(self):
        result = self.invoke("--check")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("intended-session", result.stdout)
        self.assertFalse((self.path / "record.json").exists())

    def test_preflight_rejects_unregistered_sender(self):
        result = self.invoke("--check", "--sender", "unregistered")
        self.assertEqual(result.returncode, 1)
        self.assertIn("unknown participant 'unregistered'", result.stderr)

    def test_preflight_rejects_nonwaking_kind(self):
        result = self.invoke("--check", "--kind", "info")
        self.assertEqual(result.returncode, 1)
        self.assertIn("will not wake", result.stderr)

    def test_preflight_rejects_missing_endpoint(self):
        self.env["NO_PATH"] = "1"
        result = self.invoke("--check")
        self.assertEqual(result.returncode, 1)
        self.assertIn("no ready delivery path", result.stderr)

    def test_expected_session_refuses_misrouting_before_send(self):
        result = self.invoke("--expect-session", "database-session",
                             event={"type": "end", "reason": "exit"})
        self.assertEqual(result.returncode, 1)
        self.assertIn("refusing misrouted notification", result.stderr)
        self.assertFalse((self.path / "record.json").exists())

    def test_expected_session_can_send_to_correct_endpoint(self):
        result = self.invoke("--expect-session", "intended-session",
                             event={"type": "end", "reason": "exit"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.path / "record.json").exists())

    def test_empty_expected_session_is_not_silently_disabled(self):
        result = self.invoke("--expect-session", "", event={"type": "end", "reason": "exit"})
        self.assertEqual(result.returncode, 2)
        self.assertIn("must not be empty", result.stderr)
        self.assertFalse((self.path / "record.json").exists())

    def test_successful_chatctl_stderr_is_preserved(self):
        self.env["SEND_STDERR"] = "1"
        result = self.invoke(event={"type": "end", "reason": "exit"})
        self.assertEqual(result.returncode, 0)
        self.assertIn("health warning from chatctl", result.stderr)

    def test_receipt_warning_is_preserved(self):
        self.env["SEND_WARNING"] = "recipient filtered"
        result = self.invoke(event={"type": "end", "reason": "exit"})
        self.assertEqual(result.returncode, 0)
        self.assertIn("recipient filtered", result.stderr)

    def test_send_failure_reports_underlying_error(self):
        self.env["SEND_ERROR"] = "1"
        result = self.invoke(event={"type": "end", "reason": "eof"})
        self.assertEqual(result.returncode, 1)
        self.assertIn("chatctl exited 5: UnknownParticipant", result.stderr)


if __name__ == "__main__":
    unittest.main()
