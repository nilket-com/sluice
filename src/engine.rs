use crate::cli::Options;
use regex::bytes::RegexSet;
use serde::Serialize;
use std::{io, time::Instant};

#[derive(Debug, Serialize)]
pub struct Event {
    pub schema: u8,
    pub seq: u64,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continued: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt_truncated: Option<bool>,
    pub input_bytes: u64,
    pub input_lines: u64,
    pub included_lines: u64,
    pub excluded_lines: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

pub trait Sink {
    fn emit(&mut self, event: Event) -> io::Result<()>;
}

pub struct Engine<S> {
    pub sink: S,
    include: RegexSet,
    exclude: RegexSet,
    watches: Vec<String>,
    fired: Vec<bool>,
    line: Vec<u8>,
    max_line: usize,
    max_bytes: usize,
    delay: std::time::Duration,
    pending: String,
    pending_line: u64,
    continued: bool,
    deadline: Option<Instant>,
    seq: u64,
    input_bytes: u64,
    input_lines: u64,
    included_lines: u64,
    excluded_lines: u64,
}

impl<S: Sink> Engine<S> {
    pub fn new(options: &Options, sink: S) -> Result<Self, String> {
        if options.watch_for.len() > 128
            || options
                .watch_for
                .iter()
                .any(|s| s.is_empty() || s.len() > 4096 || s.contains('\n'))
        {
            return Err(
                "use at most 128 watch literals, each 1–4096 UTF-8 bytes without LF".into(),
            );
        }
        if options.filter_in.len() + options.filter_out.len() > 128 {
            return Err("use at most 128 total filter patterns".into());
        }
        let compile = |patterns: &[String]| {
            regex::bytes::RegexSetBuilder::new(patterns)
                .size_limit(8 * 1024 * 1024)
                .build()
                .map_err(|e| format!("invalid filter: {e}"))
        };
        Ok(Self {
            sink,
            include: compile(&options.filter_in)?,
            exclude: compile(&options.filter_out)?,
            watches: options.watch_for.clone(),
            fired: vec![false; options.watch_for.len()],
            line: Vec::new(),
            max_line: options.max_line_bytes,
            max_bytes: options.max_bytes,
            delay: options.max_delay,
            pending: String::new(),
            pending_line: 0,
            continued: false,
            deadline: None,
            seq: 0,
            input_bytes: 0,
            input_lines: 0,
            included_lines: 0,
            excluded_lines: 0,
        })
    }

    fn event(&mut self, kind: &'static str, reason: &'static str) -> Event {
        self.seq += 1;
        Event {
            schema: 1,
            seq: self.seq,
            kind,
            reason,
            text: None,
            text_bytes: None,
            line: None,
            continued: None,
            watch: None,
            excerpt_truncated: None,
            input_bytes: self.input_bytes,
            input_lines: self.input_lines,
            included_lines: self.included_lines,
            excluded_lines: self.excluded_lines,
            exit_code: None,
            signal: None,
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub fn tick(&mut self, now: Instant) -> io::Result<()> {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.flush("time")?;
        }
        Ok(())
    }

    pub fn feed(&mut self, bytes: &[u8], now: Instant) -> io::Result<()> {
        self.tick(now)?;
        for segment in bytes.split_inclusive(|b| *b == b'\n') {
            if self.line.len() + segment.len() > self.max_line {
                return Err(io::Error::other(format!(
                    "physical line {} exceeds --max-line-bytes {}; increase the limit (maximum 16MiB)",
                    self.input_lines + 1,
                    self.max_line
                )));
            }
            let old_len = self.line.len();
            self.line.extend_from_slice(segment);
            self.input_bytes += segment.len() as u64;
            for i in 0..self.watches.len() {
                let pattern = self.watches[i].as_bytes();
                let start = old_len.saturating_sub(pattern.len() - 1);
                if !self.fired[i]
                    && self.line[start..]
                        .windows(pattern.len())
                        .any(|w| w == pattern)
                {
                    self.fired[i] = true;
                    self.flush("watch")?;
                    let text = String::from_utf8_lossy(&self.line);
                    let end = boundary(&text, 512);
                    let excerpt = text[..end].to_owned();
                    let truncated = end < text.len();
                    let mut event = self.event("watch", "match");
                    event.line = Some(self.input_lines + 1);
                    event.watch = Some(self.watches[i].clone());
                    event.text_bytes = Some(excerpt.len());
                    event.text = Some(excerpt);
                    event.excerpt_truncated = Some(truncated);
                    self.sink.emit(event)?;
                }
            }
            if segment.last() == Some(&b'\n') {
                self.finish_line(now)?;
            }
        }
        Ok(())
    }

    fn finish_line(&mut self, now: Instant) -> io::Result<()> {
        self.input_lines += 1;
        let line = std::mem::take(&mut self.line);
        self.fired.fill(false);
        // Filters see the raw physical line, with LF (and an optional preceding CR) removed.
        let content = line.strip_suffix(b"\n").unwrap_or(&line);
        let content = if line.ends_with(b"\n") {
            content.strip_suffix(b"\r").unwrap_or(content)
        } else {
            content
        };
        if (!self.include.is_empty() && !self.include.is_match(content))
            || self.exclude.is_match(content)
        {
            self.excluded_lines += 1;
            return Ok(());
        }
        self.included_lines += 1;
        let text = String::from_utf8_lossy(&line);
        let mut rest = text.as_ref();
        while !rest.is_empty() {
            if self.pending.is_empty() {
                self.pending_line = self.input_lines;
                self.deadline = Some(now + self.delay);
            }
            let n = boundary(rest, self.max_bytes - self.pending.len());
            if n == 0 {
                // A Unicode scalar cannot fit in the remaining space; flush before it.
                self.flush("size")?;
                continue;
            }
            self.pending.push_str(&rest[..n]);
            rest = &rest[n..];
            self.continued = !rest.is_empty();
            if self.pending.len() == self.max_bytes || !rest.is_empty() {
                self.flush("size")?;
            }
        }
        Ok(())
    }

    pub fn flush(&mut self, reason: &'static str) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut event = self.event("batch", reason);
        event.line = Some(self.pending_line);
        event.continued = Some(self.continued);
        event.text_bytes = Some(self.pending.len());
        event.text = Some(std::mem::take(&mut self.pending));
        self.deadline = None;
        self.continued = false;
        self.sink.emit(event)
    }

    pub fn finish_input(&mut self, now: Instant) -> io::Result<()> {
        if !self.line.is_empty() {
            self.finish_line(now)?;
        }
        self.flush("eof")
    }

    pub fn end(
        &mut self,
        reason: &'static str,
        exit_code: Option<i32>,
        signal: Option<i32>,
    ) -> io::Result<()> {
        let mut event = self.event("end", reason);
        event.exit_code = exit_code;
        event.signal = signal;
        self.sink.emit(event)
    }
}

fn boundary(text: &str, limit: usize) -> usize {
    let mut n = limit.min(text.len());
    while !text.is_char_boundary(n) {
        n -= 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;
    use std::time::Duration;

    #[derive(Default)]
    struct Saved(Vec<Event>);
    impl Sink for Saved {
        fn emit(&mut self, event: Event) -> io::Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    fn engine(args: &[&str]) -> Engine<Saved> {
        let cli =
            Cli::try_parse_from(std::iter::once("sluice").chain(args.iter().copied())).unwrap();
        Engine::new(&cli.options, Saved::default()).unwrap()
    }
    fn text(engine: &Engine<Saved>) -> String {
        engine
            .sink
            .0
            .iter()
            .filter(|e| e.kind == "batch")
            .map(|e| e.text.as_deref().unwrap())
            .collect()
    }

    #[test]
    fn timer_is_age_not_idle_and_never_emits_empty_batches() {
        let mut e = engine(&["--max-delay", "10ms"]);
        let now = Instant::now();
        e.tick(now + Duration::from_secs(1)).unwrap();
        assert!(e.sink.0.is_empty());
        e.feed(b"one\n", now).unwrap();
        e.feed(b"two\n", now + Duration::from_millis(9)).unwrap();
        e.tick(now + Duration::from_millis(10)).unwrap();
        assert_eq!(e.sink.0.len(), 1);
        assert_eq!(e.sink.0[0].reason, "time");
        assert_eq!(text(&e), "one\ntwo\n");
        e.tick(now + Duration::from_secs(10)).unwrap();
        assert_eq!(e.sink.0.len(), 1);
    }

    #[test]
    fn size_splits_unicode_without_loss() {
        let mut e = engine(&["--max-bytes", "4"]);
        let now = Instant::now();
        e.feed("a🙂é\ntail".as_bytes(), now).unwrap();
        e.finish_input(now).unwrap();
        assert_eq!(text(&e), "a🙂é\ntail");
        assert!(e.sink.0.iter().all(|e| e.text_bytes.unwrap() <= 4));
        assert!(e.sink.0.iter().any(|e| e.continued == Some(true)));
        assert_eq!(e.sink.0.last().unwrap().continued, Some(false));
    }

    #[test]
    fn excluded_partial_line_watches_fire_before_eof_and_across_reads() {
        let mut e = engine(&[
            "--filter-in",
            "^ERROR",
            "--filter-out",
            "ignore",
            "--watch-for",
            "finished",
        ]);
        let now = Instant::now();
        e.feed(b"ERROR real\nERROR ignore\njob fini", now).unwrap();
        assert!(e.sink.0.is_empty());
        e.feed(b"shed finished", now).unwrap();
        assert_eq!(e.sink.0.len(), 2);
        assert_eq!(e.sink.0[0].reason, "watch");
        assert_eq!(e.sink.0[1].kind, "watch");
        assert_eq!(e.sink.0[1].line, Some(3));
        e.finish_input(now).unwrap();
        e.end("eof", None, None).unwrap();
        assert_eq!(text(&e), "ERROR real\n");
        let end = e.sink.0.last().unwrap();
        assert_eq!(
            (end.input_lines, end.included_lines, end.excluded_lines),
            (3, 1, 2)
        );
        assert_eq!(end.seq as usize, e.sink.0.len());
    }

    #[test]
    fn anchored_filters_work_with_crlf_and_invalid_utf8_is_explicitly_rendered() {
        let mut e = engine(&["--filter-in", "^(?-u:bad\\xFF)$"]);
        let now = Instant::now();
        e.feed(b"bad\xff\r\nother\n", now).unwrap();
        e.finish_input(now).unwrap();
        assert_eq!(text(&e), "bad�\r\n");
    }

    #[test]
    fn line_limit_is_enforced_even_for_filtered_input() {
        let mut e = engine(&["--max-line-bytes", "4", "--filter-in", "never"]);
        let now = Instant::now();
        e.feed(b"1234", now).unwrap();
        assert!(
            e.feed(b"5", now)
                .unwrap_err()
                .to_string()
                .contains("physical line 1")
        );
    }

    #[test]
    fn empty_stream_has_one_end_and_no_batch() {
        let mut e = engine(&[]);
        e.finish_input(Instant::now()).unwrap();
        e.end("eof", None, None).unwrap();
        assert_eq!(e.sink.0.len(), 1);
        assert_eq!(e.sink.0[0].input_bytes, 0);
        assert_eq!(e.sink.0[0].kind, "end");
    }
}
