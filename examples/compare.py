#!/usr/bin/env python3
"""Retain deterministic noisy input, delivered events, and byte-count evidence."""
import argparse
import json
from pathlib import Path
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--sluice", default="target/release/sluice")
parser.add_argument("--output-dir", type=Path, required=True)
args = parser.parse_args()
args.output_dir.mkdir(parents=True, exist_ok=False)
raw = b"".join(
    (f"ERROR case {n} failed\n" if n % 1000 == 0 else f"progress case {n} passed successfully\n").encode()
    for n in range(30000)
) + b"Finalization complete\n"
(args.output_dir / "raw.log").write_bytes(raw)
report = {"input_bytes": len(raw), "input_lines": raw.count(b"\n"), "scenarios": {}}
for label, flags in (
    ("batched", []),
    ("filtered", ["--filter-in", "^ERROR", "--watch-for", "Finalization complete"]),
):
    started = time.monotonic()
    result = subprocess.run([args.sluice, *flags], input=raw, capture_output=True, check=True, timeout=30)
    elapsed = time.monotonic() - started
    (args.output_dir / (label + ".jsonl")).write_bytes(result.stdout)
    (args.output_dir / (label + ".stderr")).write_bytes(result.stderr)
    events = [json.loads(line) for line in result.stdout.splitlines()]
    assert events[-1]["type"] == "end"
    assert events[-1]["input_bytes"] == len(raw)
    if label == "batched":
        assert "".join(e["text"] for e in events if e["type"] == "batch").encode() == raw
    else:
        assert events[-1]["included_lines"] == 30
        assert sum(e["type"] == "watch" for e in events) == 1
    report["scenarios"][label] = {
        "jsonl_bytes": len(result.stdout), "events": len(events),
        "batch_text_bytes": sum(e["text_bytes"] for e in events if e["type"] == "batch"),
        "elapsed_seconds": elapsed,
    }
rendered = json.dumps(report, indent=2) + "\n"
(args.output_dir / "report.json").write_text(rendered)
print(rendered, end="")
