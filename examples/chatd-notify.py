#!/usr/bin/env python3
"""Forward watch/end events as waking requests to an existing chatd endpoint."""
import argparse
import json
import re
import subprocess
import sys


def chatctl(arguments, body=None):
    result = subprocess.run(
        ["chatctl", "--json", *arguments], input=body, text=True,
        capture_output=True, check=False,
    )
    if result.returncode:
        diagnostic = "\n".join(part.strip() for part in (result.stderr, result.stdout) if part.strip()) or "no diagnostic"
        raise RuntimeError(f"chatctl exited {result.returncode}: {diagnostic}")
    if result.stderr.strip():
        print(result.stderr.rstrip(), file=sys.stderr)
    data = json.loads(result.stdout)
    if isinstance(data, dict) and data.get("warning"):
        warning = str(data["warning"])
        if warning not in result.stderr:
            print(f"chatctl warning: {warning}", file=sys.stderr)
    return data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sender", required=True, help="existing chatd participant, not a new label")
    parser.add_argument("--to", help="participant bound to the intended session")
    parser.add_argument("--conversation")
    parser.add_argument("--lookup-event", help="look up a stable event ID without resending")
    parser.add_argument("--kind", choices=("request", "info"), default="request",
                        help="request wakes by default; info usually only stores a message")
    parser.add_argument("--expect-session", help="refuse a delivery endpoint bound to a different session")
    parser.add_argument("--check", action="store_true", help="check identities and wake path without sending")
    args = parser.parse_args()
    if args.lookup_event is not None:
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}", args.lookup_event):
            parser.error("invalid event ID for chatd idempotency key")
        receipt = chatctl(["lookup", "--sender", args.sender,
                           "--idempotency-key", args.lookup_event])
        print(json.dumps({"event_id": args.lookup_event, "receipt": receipt,
                          "meaning": "storage lookup only; not recipient acknowledgement"}))
        return
    if not args.to or (not args.check and not args.conversation):
        parser.error("--to and --conversation are required for sending (--check needs --to)")
    if args.expect_session is not None and not args.expect_session.strip():
        parser.error("--expect-session must not be empty; set the intended Codex session ID")
    if args.sender == args.to:
        parser.error("sender and recipient must be distinct configured chatd participants")
    if args.check or args.expect_session is not None:
        status = chatctl(["status", "--as", args.sender, "--limit", "1"])
        peers = {p["name"]: p for p in status.get("participants", [])}
        for name in (args.sender, args.to):
            if name not in peers:
                raise RuntimeError(f"unknown participant {name!r}; use a configured chatd identity")
        recipient = peers[args.to]
        if not recipient.get("path_ready"):
            raise RuntimeError(f"{args.to!r} has no ready delivery path; register the intended session endpoint")
        if args.expect_session is not None and recipient.get("session") != args.expect_session:
            raise RuntimeError(f"{args.to!r} is bound to session {recipient.get('session')!r}, "
                               f"expected {args.expect_session!r}; refusing misrouted notification")
        kinds = recipient.get("wake_kinds")
        if recipient.get("transport") == "push" and kinds is not None:
            if args.kind not in kinds and "all" not in kinds:
                raise RuntimeError(f"{args.kind!r} will not wake {args.to!r}; wake policy is {kinds}")
        if args.check:
            print(f"chatd notification path ready: {args.sender} -> {args.to}; kind={args.kind}; "
              f"session={recipient.get('session') or 'pull watcher'} (submission is not receipt)")
            return
    event = json.load(sys.stdin)
    if event["type"] in ("watch", "end"):
        event_id = event.get("event_id")
        key_args = []
        if event_id is not None:
            if not isinstance(event_id, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}", event_id):
                raise RuntimeError("invalid stable event_id for chatd idempotency key")
            key_args = ["--idempotency-key", event_id]
        observed = event.get("watch", event["reason"])
        if event["type"] == "end":
            observed = (f"signal={event['signal']}" if "signal" in event
                        else f"process exit={event['exit_code']}" if "exit_code" in event else "input closed")
        receipt = chatctl(
            ["send", "--as", args.sender, "--to", args.to,
             "--conversation", args.conversation, "--kind", args.kind,
             "--summary", "Sluice observed: " + str(observed)[:160], *key_args],
            json.dumps(event) + "\n",
        )
        print(f"chatd stored {receipt['id']}; kind={args.kind}; "
              "delivery/acknowledgement must be checked separately", file=sys.stderr)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError) as error:
        print(f"sluice chatd hook: {error}", file=sys.stderr)
        sys.exit(1)
