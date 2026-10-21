# Sluice

**Batch and filter log streams for efficient agent monitoring.**

Sluice (pronounced “sloos”) is a planned Unix command that collects streaming
output and releases useful batches when a size limit, time limit, or watched
message is reached.

Long builds and test runs can produce thousands of routine lines. Having an
agent repeatedly poll those logs consumes attention, tokens, and tool calls.
Sluice will keep the original log available while delivering bounded updates
and promptly surfacing messages that matter.

## Proposed usage

```sh
build 2>&1 | sluice \
  --max-bytes 10KiB \
  --max-delay 10m \
  --filter-in 'ERROR|FAILED|panic' \
  --filter-out 'routine progress message' \
  --watch-for 'Finalization complete' \
  --log build.log \
  --format jsonl
```

**Status:** design stage. These examples describe the intended interface;
there is no executable or installation command yet.

## Intended behavior

- Release a batch when its size limit or maximum delay is reached, whichever
  comes first. Flush remaining data at EOF.
- Include lines matching `--filter-in REGEX`, then remove lines matching
  `--filter-out REGEX`. With no inclusion filter, include all lines.
- Match `--watch-for TEXT` literally against the original input before filtering.
  Emit an immediate watch event even when the matching line is filtered out.
- Preserve the complete original byte stream when `--log PATH` is specified.
- Frame output as JSONL so consumers can distinguish batches, watch events,
  and end-of-stream events without relying on pipe write boundaries.

Batching reduces interaction overhead. Filtering and bounded excerpts reduce
what an agent needs to read; batching unchanged logs alone does not reduce their
token count. Sluice will use deterministic rules, without requiring an LLM.

## Delivery and completion

A pipe carries output; it does not itself wake a sleeping agent. A later delivery
hook or adapter can notify chatd or another consumer when a batch is available.
The initial command will remain usable with ordinary shell pipelines.

EOF means the input stream closed. It does not reveal whether an upstream
command succeeded. A later `sluice run -- command ...` mode can own the child
process and report its exit status.

See [PLAN.md](PLAN.md) for milestones, boundary cases, and open decisions.

## License

Licensed under either the [MIT License](LICENSE-MIT) or the
[Apache License, Version 2.0](LICENSE-APACHE), at your option.

SPDX license expression: `MIT OR Apache-2.0`.

Unless explicitly stated otherwise, contributions intentionally submitted for
inclusion are provided under these same terms.
