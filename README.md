# Sluice

**Batch and filter log streams for efficient agent monitoring.**

Sluice (pronounced “sloos”) batches and filters streaming output for agents and
people. It writes **raw text to stdout by default** when a size limit, time limit,
or watched message is reached, while optionally preserving the full raw log.

It works as an ordinary Unix filter. **Watches flush directly to stdout; no daemon,
registration or notification hook is required.**

Agents should run it as one blocking tool call: the call returns once, with the
filtered summary, when the job ends or a watched line appears. Do not wire Sluice
to anything that wakes a model agent per event; that turns each notice into a
model turn.

```sh
build 2>&1 | sluice \
  --max-bytes 10KiB \
  --max-delay 10m \
  --filter-in 'ERROR|FAILED|panic' \
  --filter-out 'routine progress message' \
  --watch-for 'Finalization complete' \
  --log build.log
```

`--log` creates a **new** file; it refuses to overwrite an existing log. Watches
match literal text before filtering, so the finalization event is delivered even
when its line fails the inclusion filter. Flags use hyphens consistently.

For a simple completion watch:

```sh
job 2>&1 | sluice --watch-for 'Tests finished'
```

The match immediately prints `Tests finished` on its own line, even while
stdin remains open. Nothing needs to be registered or listening on a server.

## Install

The first implementation targets Linux/Unix and requires Rust 1.85 or newer.

```sh
git clone https://github.com/nilket-com/sluice.git
cd sluice
cargo install --path . --locked
sluice --help
```

There are no published release binaries or crates.io release yet.

## Batch, filter, watch

- `--max-bytes 10KiB`: maximum rendered text bytes in a batch. Output headings,
  fences, or JSON framing add overhead. Oversized selected lines can span multiple batches.
- `--max-delay 10m`: age of the first pending completed line, rather than an idle
  timeout. Continuous input cannot postpone the deadline. No empty batches.
- `--filter-in REGEX`: include lines matching any supplied pattern; default all.
- `--filter-out REGEX`: exclude lines matching any supplied pattern; exclusion wins.
- `--watch-for TEXT`: emit an immediate watch event, once per matching physical line,
  even when that line is filtered out or has not yet ended with a newline.
- `--max-line-bytes 64KiB`: bound physical line buffering; exceeding it is an error.
- `--format raw`: the default; batch text with no headers, fences, or end summaries.
- `--format markdown`: short headings with literal log text in code blocks.
- `--format jsonl`: structured events for programs that need fields and counters.

Filter flags and watch flags are repeatable. Filters see original bytes without
LF/CRLF terminators; delivered text preserves terminators and replaces invalid
UTF-8. A final partial line is processed at EOF. Until it completes, a partial
line can trigger a watch but does not start a normal batch's timer.

Raw output writes selected batch text directly, without adding separators or an
EOF summary. Watches flush the pending batch and immediately print only the
matched literal followed by a newline. This watch signal is additional to normal
selected log content: the phrase can appear again when its line is later batched.
Watches bypass filters, so even an excluded completion message can surface.

Without watches or filters, raw stdout is exactly the engine's rendered text,
with no formatting overhead. “Raw” means plain text, not a binary transport:
invalid UTF-8 is replaced; `--log` preserves exact original bytes.

Choose `--format markdown` for batch/watch headings and fenced log text, or
`--format jsonl` for structured events and counters. **Consumers expecting either
previous default must now select their format explicitly.** A raw pipe's EOF
says only that input closed; `run` returns the actual command status. Hook input
remains JSONL in every stdout format. The
[stream contract](docs/stream-contract.md) specifies all three formats.

## Run a command and report its status

```sh
sluice --max-delay 30s --watch-for 'FAILED' run -- cargo test
```

`run` combines child stdout/stderr into one pipe, supplies no child stdin, and
returns the child's exit code. Markdown/JSONL completion output records the exit code or signal; raw adds no status text.
SIGINT/SIGTERM are forwarded to the child's process group, with bounded cleanup.
Use `sh -c '...'` explicitly when shell syntax is needed.

In stdin-filter mode, EOF says only that input closed. Use shell `pipefail` to
retain upstream failure status, or use `run` when Sluice should own the command.

## Optional notification hooks

A consumer already waiting on stdout sees watch output immediately. For programs
that react to events (not model agents; see above), optionally connect a
notification executable:

```sh
sluice --watch-for 'FAILED' --notify /path/to/notifier run -- cargo test
```

The hook receives one **JSONL** event on stdin regardless of stdout format.
Repeated `--notify-arg` flags pass literal arguments without a shell. Hooks run
sequentially after stdout is flushed; failure stops processing, and the default
30s timeout kills a stuck hook's process group. There are no automatic retries.
Slow hooks or blocked stdout impose backpressure.

For recoverable delivery, prefer the optional disk outbox. The collector only
persists watch/end events; a separate process invokes adapters, so a notification timeout
cannot terminate the observed command:

```sh
sluice --outbox ./events --job build --watch-for 'finished' run -- cargo test
sluice outbox dispatch ./events --adapter /path/to/notifier --timeout 30s
sluice outbox status ./events
# Optional independent backstop: exit 1 if receipt remains absent after 10 minutes
sluice outbox status ./events --fail-unreceived --older-than 10m
sluice outbox show ./events EVENT_ID
# Only after the recipient actually confirms this event:
sluice outbox ack ./events EVENT_ID --proof 'recipient session received EVENT_ID'
```

The dispatcher delivers the same JSONL event protocol with additive `event_id`,
`run_id`, absolute `outbox`, and optional `job` fields. It retains adapter stdout,
stderr, invocation details and outcome. **Adapter exit zero means submitted,
not received.** Timeouts, interruption and nonzero exits mean unknown; they are
never automatically retried. Dispatch returns nonzero while unknown outcomes
remain; rerunning it sends only pending events. Routine batches stay on stdout.
An explicit recipient receipt is needed to mark an event received. This does not
mean the job passed, was audited, or was approved.

The outbox is local Linux storage, not an automatic wakeup service. Schedule or
invoke its dispatcher independently and supply an adapter for the intended
recipient. Dispatch is a one-shot snapshot, not a daemon. Disk retention is manual;
use a private directory on a local filesystem. See the
[outbox contract](docs/stream-contract.md#durable-outbox-linux) for bounds and
crash behavior. Existing `--notify` remains synchronous fail-fast compatibility
mode and cannot be combined with `--outbox`.

## Reading service logs

For a service running Sluice with the raw default (or explicit Markdown), view output without
journal prefixes or a JSON decoder:

```sh
journalctl --user -u my-job -o cat -n 40 --no-pager
```

Existing services started with an older Sluice binary keep their original format
until restarted. They do not need to be interrupted to update the installed CLI.

## Why this helps

Long jobs can produce thousands of successful progress lines. Repeatedly asking
an agent to read them wastes context and interactions. Batching reduces delivery
frequency; filtering reduces delivered content. Combining unchanged logs into
larger chunks alone does not reduce their token count.

Sluice uses deterministic rules and requires no LLM, daemon, or agent framework.
Run the reproducible local comparison to retain original input, raw, Markdown, and
JSONL outputs with and without filtering, and a measured byte/event report:

```sh
cargo build --release --locked
python3 examples/compare.py --sluice target/release/sluice --output-dir /tmp/sluice-demo
```

The comparison measures delivered bytes and events, not model tokens or agent
wakeups. A notification's real latency also depends on the transport and consumer.

## Development

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

The suite covers timers, Unicode splits, filtering, cross-read watches, raw logs,
notification failures, real pipes, command status, all three output formats, and cancellation. The
[plan](PLAN.md) records implementation stages and remaining work.

## License

Licensed under either the [MIT License](LICENSE-MIT) or the
[Apache License, Version 2.0](LICENSE-APACHE), at your option.

SPDX license expression: `MIT OR Apache-2.0`.

Unless explicitly stated otherwise, contributions intentionally submitted for
inclusion are provided under these same terms.
