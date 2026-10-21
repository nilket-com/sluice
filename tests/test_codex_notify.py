"""No real queue calls: these tests use a local fake Codex executable."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

HOOK = Path(__file__).resolve().parents[1] / "examples/codex-notify.py"
THREAD = "11111111-1111-4111-8111-111111111111"


class NativeAdapterTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        fake = self.root / "codex"
        fake.write_text('''#!/usr/bin/env python3
import json,os,sys
if sys.argv[1:] == ["queue", "--help"]:
    print("queue --thread ID --message TEXT")
    sys.exit(0)
with open(os.environ["RECORD"], "w") as f: json.dump(sys.argv[1:], f)
print("native stdout: submitted (not received)", flush=True)
print("native stderr diagnostic", file=sys.stderr, flush=True)
sys.exit(int(os.environ.get("FAKE_EXIT", "0")))
''')
        fake.chmod(0o755)
        self.env = {**os.environ, "PATH": str(self.root) + os.pathsep + os.environ["PATH"],
                    "RECORD": str(self.root / "record")}
        self.event = {"schema": 1, "type": "watch", "watch": "GATE_COMPLETE", "reason": "match",
                      "event_id": "run-abc:2", "run_id": "run-abc", "job": "synthetic",
                      "outbox": str(self.root / "outbox with spaces")}

    def invoke(self, *args, event=None):
        return subprocess.run([sys.executable, str(HOOK), "--thread", THREAD, *args],
                              input=json.dumps(self.event if event is None else event), text=True,
                              capture_output=True, env=self.env, timeout=5)

    def test_preflight_does_not_submit_or_claim_receipt(self):
        result = self.invoke("--check")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("receipt NOT checked", result.stdout)
        self.assertFalse((self.root / "record").exists())

    def test_explicit_session_and_evidence_in_notification(self):
        result = self.invoke()
        self.assertEqual(result.returncode, 0, result.stderr)
        args = json.loads((self.root / "record").read_text())
        self.assertEqual(args[:3], ["queue", "--thread", THREAD])
        message = args[args.index("--message") + 1]
        self.assertIn("event=run-abc:2", message)
        self.assertIn("receipt=PENDING audit=UNASSESSED", message)
        self.assertIn("sluice outbox ack", message)
        self.assertIn("'" + self.event["outbox"] + "'", message)
        self.assertIn("GATE_COMPLETE", message)
        self.assertIn("native stdout", result.stdout)
        self.assertIn("native stderr", result.stderr)

    def test_native_failure_status_and_output_are_not_hidden(self):
        self.env["FAKE_EXIT"] = "7"
        result = self.invoke()
        self.assertEqual(result.returncode, 7)
        self.assertIn("native stdout", result.stdout)
        self.assertIn("native stderr", result.stderr)

    def test_missing_stable_identity_refuses_to_send(self):
        del self.event["event_id"]
        result = self.invoke()
        self.assertEqual(result.returncode, 1)
        self.assertIn("stable event_id", result.stderr)
        self.assertFalse((self.root / "record").exists())

    def test_job_exit_zero_is_not_described_as_audit_pass(self):
        self.event.update(type="end", exit_code=0)
        result = self.invoke()
        self.assertEqual(result.returncode, 0)
        message = json.loads((self.root / "record").read_text())[-1]
        self.assertIn("process_exit=0", message)
        self.assertIn("audit=UNASSESSED", message)
        self.assertNotIn("PASS", message)

    def test_batch_is_not_queued(self):
        self.event["type"] = "batch"
        self.assertEqual(self.invoke().returncode, 0)
        self.assertFalse((self.root / "record").exists())

    def test_ambiguous_thread_name_is_rejected(self):
        result = self.invoke("--thread", "database")
        self.assertEqual(result.returncode, 2)
        self.assertFalse((self.root / "record").exists())


if __name__ == "__main__":
    unittest.main()
