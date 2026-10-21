#![cfg(unix)]
use serde_json::Value;
use std::{
    io::{Read, Write},
    process::{Child, Command, Output, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn output(args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sluice"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn default_raw_adds_no_framing_even_when_splitting_unicode_lines() {
    let text = "a🙂é\n# log heading\n```\nunterminated";
    let result = output(&["--max-bytes", "4"], text.as_bytes());
    assert!(result.status.success());
    assert_eq!(result.stdout, text.as_bytes());
    assert!(result.stderr.is_empty());
    assert_eq!(
        output(&["--format", "raw"], text.as_bytes()).stdout,
        text.as_bytes()
    );
}

#[test]
fn raw_filters_lines_without_eof_summary() {
    let result = output(
        &["--filter-in", "^ERROR", "--filter-out", "ignore"],
        b"routine\nERROR actual\nERROR ignore\n",
    );
    assert!(result.status.success());
    assert_eq!(result.stdout, b"ERROR actual\n");
    assert!(output(&[], b"").stdout.is_empty());
}

#[test]
fn raw_watch_flushes_pending_text_then_prints_only_the_matched_literal() {
    let result = output(
        &["--filter-in", "^ERROR", "--watch-for", "finished"],
        b"ERROR useful\nroutine job finished\n",
    );
    assert!(result.status.success());
    assert_eq!(result.stdout, b"ERROR useful\nfinished\n");
    // A watch signal is additional to selected log content, not deduplication.
    assert_eq!(
        output(&["--watch-for", "done"], b"done\n").stdout,
        b"done\ndone\n"
    );
}

fn live_output(args: &[&str], input: &[u8], expected: &[u8]) {
    let mut child = Running(
        Command::new(env!("CARGO_BIN_EXE_sluice"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut stdout = child.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let n = expected.len();
    std::thread::spawn(move || {
        let mut bytes = vec![0; n];
        stdout.read_exact(&mut bytes).unwrap();
        sender.send(bytes).unwrap();
        // Keep draining so closing stdin can complete normally.
        std::io::copy(&mut stdout, &mut std::io::sink()).unwrap();
    });
    child.0.stdin.as_mut().unwrap().write_all(input).unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
        expected
    );
    assert!(child.0.try_wait().unwrap().is_none());
    child.0.stdin.take();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "process timeout");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn raw_watch_is_immediate_before_newline_without_a_hook() {
    live_output(
        &["--filter-out", ".*", "--watch-for", "finished"],
        b"job finished",
        b"finished\n",
    );
}

#[test]
fn raw_timer_flushes_with_input_open() {
    live_output(&["--max-delay", "20ms"], b"hello\n", b"hello\n");
}

#[test]
fn raw_hooks_keep_structured_end_events_with_empty_stdout() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("events");
    let result = output(
        &[
            "--notify",
            "sh",
            "--notify-arg=-c",
            "--notify-arg=cat >> \"$1\"",
            "--notify-arg=hook",
            "--notify-arg",
            log.to_str().unwrap(),
        ],
        b"",
    );
    assert!(result.status.success());
    assert!(result.stdout.is_empty());
    let events: Vec<Value> = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["type"], "end");
}

#[test]
fn raw_reports_command_failure_via_exit_status_without_extra_text() {
    let result = output(&["run", "--", "sh", "-c", "printf failed; exit 7"], b"");
    assert_eq!(result.status.code(), Some(7));
    assert_eq!(result.stdout, b"failed");
}
