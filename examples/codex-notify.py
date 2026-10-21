#!/usr/bin/env python3
"""Optional local Codex queue adapter. A successful submission is not receipt."""
import argparse
import json
import os
import re
import shlex
import subprocess
import sys
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--thread", required=True, help="exact recipient session UUID")
    parser.add_argument("--codex", default="codex", help="installed CLI executable")
    parser.add_argument("--check", action="store_true", help="check CLI syntax without sending")
    args = parser.parse_args()
    try:
        uuid.UUID(args.thread)
    except ValueError:
        parser.error("--thread must be an explicit session UUID")
    if args.check:
        result = subprocess.run([args.codex, "queue", "--help"], capture_output=True, text=True, timeout=10)
        if result.returncode or "--thread" not in result.stdout or "--message" not in result.stdout:
            raise RuntimeError("installed CLI does not expose the expected queue interface")
        print("Queue interface supported; recipient existence and receipt NOT checked. No message sent.")
        return
    event = json.load(sys.stdin)
    if event["type"] not in ("watch", "end"):
        return
    event_id = event.get("event_id")
    if not isinstance(event_id, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}", event_id):
        raise RuntimeError("a stable event_id is required; collect with --outbox first")
    outbox = event.get("outbox")
    if not isinstance(outbox, str) or not os.path.isabs(outbox) or any(c in outbox for c in "\n\r\0"):
        raise RuntimeError("event must identify its absolute outbox evidence path")
    job = str(event.get("job", "unspecified"))[:160]
    if event["type"] == "watch":
        observed = "watch=" + json.dumps(str(event["watch"])[:512], ensure_ascii=False)
    elif "signal" in event:
        observed = f"process_signal={event['signal']}"
    elif "exit_code" in event:
        observed = f"process_exit={event['exit_code']}"
    else:
        observed = "input_closed"
    show = shlex.join(["sluice", "outbox", "show", outbox, event_id])
    ack = shlex.join(["sluice", "outbox", "ack", outbox, event_id, "--proof",
                      f"recipient {args.thread} received {event_id}"])
    message = (f"SLUICE_{event['type'].upper()} event={event_id} job={json.dumps(job)} "
               f"{observed} receipt=PENDING audit=UNASSESSED evidence={json.dumps(outbox)}\n"
               f"Read: {show}\nAfter reading, acknowledge receipt: {ack}\n"
               "Detection only: no audit acceptance or promotion is implied. "
               "Do not repeat an uncertain submission or start unrelated work.")
    print(f"native queue attempt event={event_id} thread={args.thread}; outcome requires recipient receipt",
          file=sys.stderr, flush=True)
    # Replace the adapter rather than wrapping a second timeout/capture layer.
    # Outbox dispatch owns the bounded attempt and preserves native stdout/stderr.
    os.execvp(args.codex, [args.codex, "queue", "--thread", args.thread, "--message", message])


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        print(f"sluice native queue adapter: {error}", file=sys.stderr)
        sys.exit(1)
