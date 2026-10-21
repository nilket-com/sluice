# Optional local Codex notifications

This adapter uses the installed CLI's native queue command, without chatd or bot
registration. It is a transport for observations, not an authoritative review,
audit clearance, or promotion record. Raw Sluice stdout remains unchanged.

The [official CLI reference](https://learn.chatgpt.com/docs/developer-commands?surface=cli#codex-queue)
documents `codex queue --thread ... --message ...`. The installed CLI's
`codex queue --help` is the local interface check. This example requires an
explicit session UUID to avoid ambiguous session names. The `--check` command
checks supported syntax only; it does not establish that a recipient exists or
is reading messages.

## Collect and dispatch separately

Observe an existing job's log; notification failures must not control that job:

```sh
tail -F existing-gate.log | sluice \
  --outbox /absolute/path/to/notifications \
  --job readiness-smoke \
  --watch-for GATE_COMPLETE
```

In another process, check the adapter, then dispatch:

```sh
python3 examples/codex-notify.py --thread SESSION_UUID --check

sluice outbox dispatch /absolute/path/to/notifications \
  --adapter ./examples/codex-notify.py \
  --adapter-arg=--thread --adapter-arg=SESSION_UUID \
  --timeout 30s
```

The collector persists events with stable IDs. The dispatcher owns each bounded
adapter attempt and retains native stdout/stderr. The adapter replaces itself
with the native queue process, preserving its output and exit status. It does
not add another timeout or discard the queue's output.

A successful queue exit establishes submission only. The notification includes
the stable event ID, job label, evidence directory, and commands for the recipient
to inspect the event and acknowledge actual receipt:

```sh
sluice outbox show /absolute/path/to/notifications EVENT_ID
sluice outbox ack /absolute/path/to/notifications EVENT_ID \
  --proof 'recipient session SESSION_UUID read EVENT_ID'
```

Run the acknowledgement in the receiving session after reading the evidence.
It records the explicit receipt assertion and proof; it is not a cryptographic
attestation. Do not have the dispatcher acknowledge itself on queue exit zero.
Receipt remains separate from job success, independent audit, and promotion.

Check delivery health independently of the collector or adapter:

```sh
sluice outbox status /absolute/path/to/notifications --fail-unreceived --older-than 10m
```

This exits nonzero for overdue unreceived notifications. Schedule it separately;
the outbox does not install a dispatcher or monitoring service. Only watch and
end events enter the outbox; ordinary batches remain on stdout.

## Uncertain delivery

A timeout or killed adapter can have submitted before losing its receipt. Preserve
that attempt as unknown and investigate the evidence; do not silently resend it.
The native queue adapter exposes no storage lookup or deduplication guarantee.
Stable event IDs support correlation and recipient-side checking, not an invented
exactly-once delivery promise. Recovery needs recipient evidence or an external
transport-supported lookup before an operator changes the recorded state.

Chatd is an alternative transport with a storage lookup by sender/idempotency key.
The [chatd guide](chatd.md) explains its registration and routing requirements.
When a Sluice event contains `event_id`, the chatd adapter passes that exact ID as
its idempotency key. A read-only lookup never sends another message:

```sh
python3 examples/chatd-notify.py --sender sluice --lookup-event EVENT_ID
```

A storage receipt from that lookup does not establish recipient receipt. Preserve
chatd's acknowledgement, independent review acceptance, and promotion as separate
facts.

## Progress and results

The native notification identifies what Sluice observed. It does not count tests
or infer PASS from a supervisor's exit zero. A producer can emit a compact record
such as `tests=51/51 rc_failures=0 audit=PENDING`, but those counts must come from
named step/result records. Cleanup, archival, DONE, or commit-unchanged markers
must not inflate the test denominator. Keep those milestones separate.

Tests use isolated fake queue executables and synthetic events. No production
job, real review request, or uncertain operation needs to be replayed to exercise
timeout, interruption, submission, and recipient-acknowledgement handling.
