# Sluice implementation plan

## Motivation and success criteria

An agent should be able to leave a long-running job alone, receive a useful
update when something changes, and inspect the full evidence when necessary.
The tool should also serve humans and conventional scripts.

Success means fewer polling interactions, bounded delivered output, prompt watch
notifications, and a complete optional log. Measure tool calls and delivered
bytes separately from wall-clock notification latency. Do not claim token
savings solely from combining identical output into larger chunks.

## 1. Define the stream contract

Write down the CLI, output schema, and edge behavior before implementation.
The README's interface is provisional until this milestone is complete.

Proposed rules:

- Accept stdin; reserve stdout for framed output and stderr for diagnostics.
- Use separate size and time options. The delay starts with the first pending
  deliverable content and uses a monotonic clock; do not emit empty periodic
  batches. Continued input must not reset the deadline indefinitely.
- Apply line-oriented regex filters, with exclusion taking precedence. Repeated
  inclusion patterns mean any match; repeated exclusions mean any match.
- Run literal watch matching before filters. Watch events include the matched
  text and a bounded excerpt from the original input, even if excluded by filters.
  Record watch and filtering precedence explicitly in the schema documentation.
- Flush pending content on watch matches and EOF. EOF produces a distinct final
  event, including for an empty stream; it carries no upstream success claim.
- Treat the byte threshold as a payload bound, with JSON framing overhead
  accounted for separately. Oversized lines must not require unbounded memory.
- Preserve raw input bytes in the optional log. Define decoding of invalid UTF-8,
  partial lines, and carriage-return progress output independently of raw logging.
- Define stable event types, sequence numbers, flush reasons, byte and line
  counts, and truncation or continuation indicators. Never silently hide dropped
  content. Avoid ambiguous reliance on pipe write boundaries.

Decisions to settle: default size/delay, whether to support a human output mode,
maximum line/watch-pattern sizes, bounded continuation semantics, and whether
watch patterns can span physical lines. A maximum batch age and an idle timeout
are different controls; the first release needs only the former.

## 2. Implement the small local filter

Prefer a single Rust executable with a small dependency set. Confirm this choice
while implementing the stream contract; no daemon is needed for the first release.

Implement stdin ingestion, simultaneous size/time triggers, filters, literal
watches, JSONL framing, raw logging, EOF handling, and helpful argument errors.
Use bounded buffers and explicit backpressure. A stalled consumer may delay
further reading; document that the delay target cannot override blocked output.

Test observable behavior rather than internal implementation:

- A quiet stream flushes on time while stdin remains open.
- Continuous input does not postpone the deadline.
- Size boundaries, oversized lines, and partial writes preserve framing.
- Filters and watches interact as documented; filtered-only input is bounded.
- EOF flushes once; empty input and a trailing partial line behave predictably.
- Raw logging preserves bytes, including invalid UTF-8 and carriage returns.
- Broken pipes, log write failures, and interrupted reads have defined outcomes.

Use controlled clocks where practical and a small number of real-pipe integration
checks. Include a synthetic noisy job demonstrating delivered-byte reduction.

## 3. Deliver without agent polling

Add an executable delivery hook with explicit arguments, without implicit shell
interpolation. Specify ordering, bounded queues, failure handling, and whether
retries can duplicate deliveries before advertising reliable delivery.

Start with a documented chatd adapter example. Keep transport integration separate
from stream processing. Notification integration must actually wake the consumer;
saving another file that the agent must poll is insufficient.

A durable spool with acknowledgments can follow if the first integration needs
recovery across restarts. Do not promise durable delivery before implementing it.

## 4. Own the command lifecycle

Add `sluice run -- command ...` to capture child stdout/stderr and real exit status.
Specify signal forwarding, cancellation, output ordering, and exit-code behavior.
Preserve the simple stdin filter as a supported mode. Cross-stream ordering is
only as precise as the capture mechanism permits.

## 5. Package and measure

Add CI, reproducible usage examples, release binaries, and installation guidance
once there is a working executable. Test Linux first; document portability limits.

Compare raw streaming, timed batching, and filtered batching using the same job.
Report notification latency, delivered bytes/events, memory use, and consumer
interactions. Keep complete raw evidence for the comparison.

## Initial boundaries

No semantic AI summarization, hosted service, or mandatory agent framework.
Do not equate a watch phrase with process success. The user chooses filters;
Sluice should make its output rules inspectable and predictable.
