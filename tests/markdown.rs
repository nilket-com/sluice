#![cfg(unix)]
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
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
fn markdown_is_default_and_preserves_readable_log_text() {
    let result = output(&[], b"a \"quoted\" path\\file\nsecond\n");
    assert!(result.status.success());
    let text = String::from_utf8(result.stdout).unwrap();
    assert_eq!(
        text,
        "### Batch 1 (eof)\n\n```text\na \"quoted\" path\\file\nsecond\n```\n\n### End (input closed)\n\n2 input lines; 2 included; 0 excluded.\n\n"
    );
    let explicit = output(
        &["--format", "markdown"],
        b"a \"quoted\" path\\file\nsecond\n",
    );
    assert_eq!(explicit.stdout, text.as_bytes());
}

#[test]
fn jsonl_remains_an_explicit_structured_output_option() {
    let result = output(&["--format", "jsonl"], b"hello\n");
    assert!(result.status.success());
    let events: Vec<Value> = result
        .stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(events[0]["schema"], 1);
    assert_eq!(events[0]["text"], "hello\n");
    assert_eq!(events[1]["type"], "end");
}

#[test]
fn markdown_hooks_still_receive_jsonl_independent_of_stdout() {
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
        b"log line\n",
    );
    assert!(result.status.success());
    assert!(result.stdout.starts_with(b"### Batch"));
    let events: Vec<Value> = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[0]["type"], "batch");
    assert_eq!(events[0]["text"], "log line\n");
    assert_eq!(events[1]["type"], "end");
}

fn live_header(args: &[&str], input: &[u8], expected: &str) {
    let mut child = Running(
        Command::new(env!("CARGO_BIN_EXE_sluice"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    child.0.stdin.as_mut().unwrap().write_all(input).unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
        expected
    );
    assert!(child.0.try_wait().unwrap().is_none()); // stdin remains open
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
fn markdown_watch_reaches_stdout_before_newline_without_any_hook() {
    live_header(
        &["--filter-out", ".*", "--watch-for", "Finalization complete"],
        b"Finalization complete",
        "### Watch 1: Finalization complete",
    );
}

#[test]
fn markdown_timer_flushes_with_input_still_open() {
    live_header(&["--max-delay", "20ms"], b"pending\n", "### Batch 1 (time)");
}

#[test]
fn markdown_command_completion_reports_failure_without_json() {
    let result = output(
        &["run", "--", "sh", "-c", "printf 'failed\\n'; exit 7"],
        b"",
    );
    assert_eq!(result.status.code(), Some(7));
    let text = String::from_utf8(result.stdout).unwrap();
    assert!(text.contains("failed\n"));
    assert!(text.contains("### End (exit 7)"));
}
