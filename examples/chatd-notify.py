#!/usr/bin/env python3
"""Forward watch/end events to an existing chatd delivery integration."""
import argparse
import json
import subprocess
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--sender", required=True)
parser.add_argument("--to", required=True)
parser.add_argument("--conversation", required=True)
args = parser.parse_args()
event = json.load(sys.stdin)
if event["type"] in ("watch", "end"):
    subprocess.run(
        ["chatctl", "send", "--as", args.sender, "--to", args.to,
         "--conversation", args.conversation, "--kind", "info",
         "--summary", "Sluice " + event["type"] + ": " + event["reason"]],
        input=json.dumps(event) + "\n", text=True, check=True,
    )
