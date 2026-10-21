# Sluice

**Batch and filter log streams for efficient agent monitoring.**

Sluice (pronounced “sloos”) batches and filters streaming output for agents and
people. It writes readable **Markdown to stdout** when a size limit, time limit,
or watched message is reached, while optionally preserving the full raw log.

It works as an ordinary Unix filter. **Watches flush directly to stdout; chatd,
registration, and notification hooks are not required.**

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

The match immediately emits a short watch heading and log excerpt, even while
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
- `--format markdown`: the default; short headings with literal log text in code blocks.
- `--format jsonl`: structured events for programs that need fields and counters.

Filter flags and watch flags are repeatable. Filters see original bytes without
LF/CRLF terminators; delivered text preserves terminators and replaces invalid
UTF-8. A final partial line is processed at EOF. Until it completes, a partial
line can trigger a watch but does not start a normal batch's timer.

Markdown uses short batch/watch headings and a final line-count summary. Log text
is not summarized or rewritten; a code fence that cannot collide with the content
keeps it readable. Partial lines may need a presentation newline before the closing
fence. The optional raw log retains exact original bytes.

With `--format jsonl`, every output line is a complete JSON event (`batch`, `watch`,
or `end`) with schema version, sequence number, reason, and counters. **Existing
stdout consumers that parse JSON must now explicitly select `--format jsonl`.**
Hook input remains JSONL regardless of stdout format. The
[stream contract](docs/stream-contract.md) specifies both formats.

## Run a command and report its status

```sh
sluice --max-delay 30s --watch-for 'FAILED' run -- cargo test
```

`run` combines child stdout/stderr into one pipe, supplies no child stdin, and
returns the child's exit code. Its final event records the exit code or signal.
SIGINT/SIGTERM are forwarded to the child's process group, with bounded cleanup.
Use `sh -c '...'` explicitly when shell syntax is needed.

In stdin-filter mode, EOF says only that input closed. Use shell `pipefail` to
retain upstream failure status, or use `run` when Sluice should own the command.

## Optional notification hooks

A consumer already waiting on stdout sees watch output immediately. To wake an
agent that has stopped waiting, optionally connect a notification executable:

```sh
sluice --watch-for 'FAILED' --notify /path/to/notifier run -- cargo test
```

The hook receives one **JSONL** event on stdin even when stdout is Markdown.
Repeated `--notify-arg` flags pass literal arguments without a shell. Hooks run
sequentially after stdout is flushed; failure stops processing, and the default
30s timeout kills a stuck hook's process group. There are no automatic retries.
Slow hooks or blocked stdout impose backpressure.

The [optional chatd adapter guide](docs/chatd.md) covers one possible transport,
including registration, session routing, and wake policy. It is separate from
Sluice's normal stream processing.

## Reading service logs

For a service running Sluice with the Markdown default, view recent output without
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
Run the reproducible local comparison to retain original input, Markdown and
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
python3 -m unittest discover -s tests -p 'test_*.py'
```

The suite covers timers, Unicode splits, filtering, cross-read watches, raw logs,
notification failures, real pipes, command status, both output formats, and cancellation. The
[plan](PLAN.md) records implementation stages and remaining work.

## License

Licensed under either the [MIT License](LICENSE-MIT) or the
[Apache License, Version 2.0](LICENSE-APACHE), at your option.

SPDX license expression: `MIT OR Apache-2.0`.

Unless explicitly stated otherwise, contributions intentionally submitted for
inclusion are provided under these same terms.
