# Optional chatd integration

Sluice works without chatd. This adapter is only for notifying an agent after it
has stopped waiting on stdout. For batching and immediate watches, use Sluice
on its own as described in the [README](../README.md).

Configure a distinct sender in chatd first (for example, add `sluice` to the
`participants` list in `~/.config/chatd/config.toml` and restart chatd). Merely
passing `--sender sluice` does not register it. Check the delivery endpoint
**before** starting a job. This example assumes `codex` is already bound to the
session identified by `CODEX_THREAD_ID`:

```sh
: "${CODEX_THREAD_ID:?Set CODEX_THREAD_ID to the intended Codex session ID}"
python3 examples/chatd-notify.py \
  --sender sluice --to codex --conversation build-42 \
  --expect-session "$CODEX_THREAD_ID" --check

sluice --watch-for 'FAILED' \
  --notify ./examples/chatd-notify.py \
  --notify-arg=--sender --notify-arg=sluice \
  --notify-arg=--to --notify-arg=codex \
  --notify-arg=--conversation --notify-arg=build-42 \
  --notify-arg=--expect-session --notify-arg="$CODEX_THREAD_ID" \
  run -- cargo test
```

This optional example needs an existing `chatctl` installation and configured
recipient delivery endpoint. Sender and recipient must both be configured chatd
participants, and they must be distinct (chatd rejects self-messages). Choose
identities and the intended session ID appropriate for your installation.

It sends only watch and end events as **requests**, which wake chatd's default
push policy. `info` messages normally wait for a future waking request; use
`--kind info` explicitly only when that behavior is intended. `--check` verifies
the identities, delivery path, wake policy, and optional expected session without
sending a message. `--expect-session` also checks routing on each hook invocation.
A participant name identifies one endpoint, not all Codex sessions. Successful
storage/submission is not proof that an agent received or acted on the alert;
check chatd submission and acknowledgement receipts when diagnosing delivery.

