# Stream contract (schema 1)

This contract applies to the first implementation. The tool targets Unix/Linux.

## Input and bounds

Input is a byte stream, read in chunks of at most 8192 bytes. At most four chunks
are queued in front of the processor. A physical line ends with LF. A final
unterminated line is processed at EOF. CR is preserved; CR alone does not end a
line. For regex matching only, a terminating LF and its optional preceding CR are
removed, so anchored patterns work with LF and CRLF logs.

`--max-line-bytes` defaults to 64KiB, includes the terminator, and accepts values
from 4 bytes through 16MiB. A longer physical line is an explicit processing error,
not silent truncation. Filters apply only when a line completes; an unterminated
line does not start the batch timer. Literal watches can fire before a newline.
This is intended for line-oriented logs, not arbitrarily long progress records.

Regexes use Rust regex byte syntax against original bytes. Use `(?-u:...)` where
necessary to match invalid UTF-8. Included text is decoded with UTF-8 replacement
characters. Raw logging preserves the original bytes. Rendering is not a lossless
binary transport; the log is the source of truth for byte-exact inspection.

No more than 128 total filter patterns or 128 watch literals are accepted. Each
watch is 1–4096 UTF-8 bytes and cannot contain LF. Matches cannot span lines.
Each configured watch fires at most once per physical line, including if the
matching bytes arrive across reads. Matching runs before filtering. A watch event
includes the literal and up to 512 rendered bytes of the line prefix available
at detection, with an explicit truncation flag. The prefix need not contain the
match; the `watch` field always identifies the matched literal.

## Batching and filtering

Include all lines unless `--filter-in` is specified. Any inclusion match admits a
line; any exclusion match rejects it. Repeating either flag adds another pattern.
Exclusion wins. A watch bypasses these filters, but does not admit the entire line
into normal batches. Watch and batch output may consequently contain overlapping
text; concatenate only `batch.text` to reconstruct the rendered selected stream.

`--max-bytes` defaults to 10KiB and bounds UTF-8 bytes of batch **text**, excluding
JSON syntax and escaping. Values range from 4 bytes through 16MiB. Split selected
lines on UTF-8 scalar boundaries as necessary; there is no silent truncation.
A batch can flush slightly below capacity if the next scalar cannot fit.

`--max-delay` defaults to 10m. Durations are positive integers with `ms`, `s`, `m`,
or `h`, at most 24h. Sizes accept integer bytes, `B`, `KiB`, and `MiB`. Bare decimal
units such as `kb` and fractional values are rejected.

The monotonic deadline starts when the first text enters an empty batch.
Subsequent input does not reset it. Time, size, watch, and EOF can flush a batch;
empty batches are never emitted. A quiet stream with pending completed lines
flushes while stdin remains open. Output writes and notification hooks impose
backpressure: a blocked consumer can delay reading, watches, and timed delivery.
The delay is a scheduling target, not a guarantee under blocked output.
Stdout writes use one bounded worker with an acknowledged event; the controller
can abandon a blocked write on cancellation without joining that worker. A
cancelled stream may contain an incomplete final JSON record. This permits
bounded process cleanup even when a downstream reader stops consuming output.

## JSONL output

Each stdout line is one complete JSON object. Every event has:

- `schema`: integer `1`.
- `seq`: monotonically increasing integer starting at 1 for this invocation.
- `type`: `batch`, `watch`, or `end`.
- `reason`: `size`, `time`, `watch`, or `eof` for batches; `match` for watches;
  `eof`, `exit`, or `signal` for the final event.
- `input_bytes`, `input_lines`, `included_lines`, `excluded_lines`: cumulative
  counters at the time the event is constructed. Input bytes include any current
  incomplete line; line counters count completed lines (including the EOF tail).

Additional batch fields:

- `text`, `text_bytes`: rendered content and its UTF-8 byte length.
- `line`: one-based input line where this batch's text starts. It can start partway
  through that line when a previous batch split it.
- `continued`: whether the last line's text continues in the next batch.

Watch events have `watch`, one-based `line`, `text`, `text_bytes`, and
`excerpt_truncated`. End events have `exit_code` or `signal` when known. Absent
fields are omitted, not filled with sentinel values. A watch can precede the
normal batch containing that same physical line. Counters are ingestion snapshots,
not counts of just the event's text.

EOF emits exactly one end event, including for empty input. Processing errors
write diagnostics to stderr and exit nonzero; they may leave a stream without an
end event. Consumers should distinguish a clean end event from pipe closure.
There is no implicit claim of job success in filter mode.

## Logging and delivery

`--log PATH` creates a new file and refuses existing files, including symlinks.
Bytes are written before filtering and rendering. Log failures stop processing;
there is no fsync or promise of recovery after power loss. On interruption or
processing failure, the file contains the bytes accepted so far, possibly including
an unprocessed suffix of the last read chunk. It is not necessarily the producer's
complete output if Sluice exits early.

`--notify EXECUTABLE` invokes an executable for each event, in order, after the
stdout event has been written and flushed. Repeated `--notify-arg ARG` supplies
literal arguments; Sluice invokes no shell. The executable receives that one JSONL
event on stdin via a temporary seekable file. Its stdout is discarded and stderr
is inherited. The file is removed automatically. Events are not placed in argv.

A nonzero hook exit, failed spawn, or timeout stops processing with no retry.
`--notify-timeout` defaults to 30s. Timed-out hook process groups are killed and
the direct child is reaped. A signal also interrupts an active hook. Stdout may
already contain an event whose hook failed; there is no exactly-once or durable
delivery promise. Hook execution is synchronous, so there is no unbounded queue.
The hook must implement whatever notification actually wakes its consumer.

## Command ownership and exit status

`sluice [OPTIONS] run -- COMMAND [ARGS...]` executes directly without a shell.
The child's stdin is `/dev/null`; its stdout and stderr share a pipe. Their bytes
are combined in pipe arrival order without stream labels. Sluice cannot restore
application-level ordering lost to child buffering or concurrent writes.

The final event reports the child's exit code or terminating signal. Sluice
returns that code, or `128 + signal` for signal termination. Filter mode returns
0 on clean EOF. Configuration parsing returns 2; other Sluice failures return 1.
An upstream failure is invisible in filter mode; use shell `pipefail` if needed.

SIGINT and SIGTERM are forwarded to the owned command's process group. Sluice
allows up to two seconds to drain output, then kills the group and reaps the
leader. On processing errors, it kills the group immediately, unless a cancellation
signal was received: even errors during stdout or hook delivery first forward
that signal and allow the same grace period before group cleanup. When delivery
has failed, further output draining is not promised. Cancellation
returns `128 + received signal`; accepted partial input is flushed where possible.
In filter mode, Sluice stops without waiting for stdin to close. Other signals
have their normal OS behavior. There is no SIGKILL cleanup guarantee.

Normal completion waits for both output EOF and the direct child's exit. A
background descendant that keeps the output pipe open will delay completion.
Descendants that deliberately create a new process group escape group cleanup.
Sluice does not claim to be a process supervisor or a sandbox.
