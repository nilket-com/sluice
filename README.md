# Sluice

**Batch and filter log streams for efficient agent monitoring.**

Sluice (pronounced “sloos”) collects streaming output and releases JSONL batches
when a size limit, time limit, or watched message is reached. It keeps routine
logs out of an agent's context while preserving full evidence on disk.

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

- `--max-bytes 10KiB`: maximum rendered text bytes in a batch. JSON framing and
  escaping add overhead. Oversized selected lines can span multiple batches.
- `--max-delay 10m`: age of the first pending completed line, rather than an idle
  timeout. Continuous input cannot postpone the deadline. No empty batches.
- `--filter-in REGEX`: include lines matching any supplied pattern; default all.
- `--filter-out REGEX`: exclude lines matching any supplied pattern; exclusion wins.
- `--watch-for TEXT`: emit an immediate watch event, once per matching physical line,
  even when that line is filtered out or has not yet ended with a newline.
- `--max-line-bytes 64KiB`: bound physical line buffering; exceeding it is an error.
- `--format jsonl`: explicit spelling of the only current output format.

Filter flags and watch flags are repeatable. Filters see original bytes without
LF/CRLF terminators; delivered text preserves terminators and replaces invalid
UTF-8. A final partial line is processed at EOF. Until it completes, a partial
line can trigger a watch but does not start a normal batch's timer.

Every output line is a complete JSON object with `schema: 1`, a sequence number,
a `type` (`batch`, `watch`, or `end`), a reason, and cumulative input/filter counts.
Batch events contain `text`, `text_bytes`, `line`, and a `continued` flag. A clean
stream ends with an `end` event, including for empty input. See the
[stream contract](docs/stream-contract.md) for exact field and boundary semantics.

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

## Notify an agent without polling

```sh
sluice --watch-for 'FAILED' \
  --notify ./examples/chatd-notify.py \
  --notify-arg=--sender --notify-arg=sluice \
  --notify-arg=--to --notify-arg=codex \
  --notify-arg=--conversation --notify-arg=build-42 \
  run -- cargo test
```

This optional example needs an existing `chatctl` installation and configured
recipient delivery endpoint. It sends only watch and end events, rather than every
routine batch. Choose a sender/recipient appropriate for your installation.

Any executable can be a hook. Sluice passes each JSONL event on stdin, with literal
`--notify-arg` values and no implicit shell. Hooks run sequentially after stdout
is flushed. A failed hook stops Sluice; a timeout (default 30s) kills its process
group. There are no automatic retries or durable-delivery guarantees. Slow hooks
or blocked stdout impose backpressure and can delay further notifications.

Writing batches to a pipe or file alone does not wake an agent. The hook and its
recipient integration must provide that wakeup. Sluice handles collection and
framing; the transport handles delivery.

## Why this helps

Long jobs can produce thousands of successful progress lines. Repeatedly asking
an agent to read them wastes context and interactions. Batching reduces delivery
frequency; filtering reduces delivered content. Combining unchanged logs into
larger chunks alone does not reduce their token count.

Sluice uses deterministic rules and requires no LLM, daemon, or agent framework.
Run the reproducible local comparison to retain original input, both JSONL
outputs, and a measured byte/event report:

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
notification failures, real pipes, command status, and cancellation. The
[plan](PLAN.md) records implementation stages and remaining work.

## License

Licensed under either the [MIT License](LICENSE-MIT) or the
[Apache License, Version 2.0](LICENSE-APACHE), at your option.

SPDX license expression: `MIT OR Apache-2.0`.

Unless explicitly stated otherwise, contributions intentionally submitted for
inclusion are provided under these same terms.
