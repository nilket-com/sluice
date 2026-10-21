#![cfg(unix)]
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

struct Harness {
    child: Child,
    events: Receiver<Value>,
}
impl Harness {
    fn new(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_sluice"))
            .args(["--format", "jsonl"])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, events) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let event: Value = serde_json::from_str(&line.unwrap()).unwrap();
                if tx.send(event).is_err() {
                    break;
                }
            }
        });
        Self { child, events }
    }
    fn write(&mut self, bytes: &[u8]) {
        self.child.stdin.as_mut().unwrap().write_all(bytes).unwrap();
    }
    fn close(&mut self) {
        self.child.stdin.take();
    }
    fn event(&self) -> Value {
        self.events
            .recv_timeout(Duration::from_secs(5))
            .expect("event timeout")
    }
    fn wait(&mut self) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code().unwrap();
            }
            assert!(Instant::now() < deadline, "process timeout");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn timer_flushes_while_stdin_is_open() {
    let mut h = Harness::new(&["--max-delay", "30ms"]);
    h.write(b"hello\n");
    let event = h.event();
    assert_eq!(event["reason"], "time");
    assert_eq!(event["text"], "hello\n");
    assert!(h.child.try_wait().unwrap().is_none());
    h.close();
    assert_eq!(h.event()["type"], "end");
    assert_eq!(h.wait(), 0);
}

#[test]
fn watch_bypasses_filters_without_waiting_for_newline() {
    let mut h = Harness::new(&["--filter-out", ".*", "--watch-for", "done"]);
    h.write(b"done");
    let event = h.event();
    assert_eq!(event["type"], "watch");
    assert_eq!(event["watch"], "done");
    h.close();
    let end = h.event();
    assert_eq!(end["excluded_lines"], 1);
    assert_eq!(h.wait(), 0);
}

#[test]
fn raw_log_preserves_bytes_and_refuses_overwrite() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("raw");
    let mut h = Harness::new(&["--log", log.to_str().unwrap(), "--filter-out", ".*"]);
    h.write(b"\xff\r\nlast");
    h.close();
    assert_eq!(h.wait(), 0);
    assert_eq!(std::fs::read(&log).unwrap(), b"\xff\r\nlast");
    let mut second = Harness::new(&["--log", log.to_str().unwrap()]);
    second.close();
    assert_eq!(second.wait(), 1);
    assert_eq!(std::fs::read(&log).unwrap(), b"\xff\r\nlast");
}

#[test]
fn run_merges_output_and_reports_real_status() {
    let mut h = Harness::new(&[
        "run",
        "--",
        "sh",
        "-c",
        "printf 'out\\n'; printf 'err\\n' >&2; exit 7",
    ]);
    let batch = h.event();
    assert_eq!(batch["text"], "out\nerr\n");
    let end = h.event();
    assert_eq!(end["reason"], "exit");
    assert_eq!(end["exit_code"], 7);
    assert_eq!(h.wait(), 7);
}

#[test]
fn command_without_output_and_failed_exec_are_distinct() {
    let mut h = Harness::new(&["run", "--", "true"]);
    assert_eq!(h.event()["exit_code"], 0);
    assert_eq!(h.wait(), 0);
    let mut h = Harness::new(&["run", "--", "/nonexistent/sluice-test"]);
    assert_eq!(h.wait(), 1);
}

#[test]
fn hook_receives_json_and_literal_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("hook output; literal");
    let mut h = Harness::new(&[
        "--notify",
        "sh",
        "--notify-arg=-c",
        "--notify-arg=cat >> \"$1\"",
        "--notify-arg=hook",
        "--notify-arg",
        output.to_str().unwrap(),
    ]);
    h.write(b"message\n");
    h.close();
    assert_eq!(h.wait(), 0);
    let events: Vec<Value> = std::fs::read_to_string(output)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["type"], "batch");
    assert_eq!(events[1]["type"], "end");
}

#[test]
fn failed_and_nonreading_hooks_stop_within_timeout() {
    let mut h = Harness::new(&["--notify", "false"]);
    h.close();
    assert_eq!(h.event()["type"], "end"); // stdout precedes the failed delivery
    assert_eq!(h.wait(), 1);
    let mut h = Harness::new(&[
        "--notify",
        "sleep",
        "--notify-arg",
        "60",
        "--notify-timeout",
        "30ms",
    ]);
    h.close();
    assert_eq!(h.wait(), 1);
}

#[test]
fn invalid_config_is_rejected_before_starting_the_command() {
    let mut h = Harness::new(&["--filter-in", "[", "run", "--", "sleep", "60"]);
    assert_eq!(h.wait(), 1);
    for args in [
        vec!["--max-bytes", "0"],
        vec!["--max-delay", "0ms"],
        vec!["--max-delay", "10"],
    ] {
        let mut h = Harness::new(&args);
        assert_eq!(h.wait(), 2);
    }
}

#[test]
fn line_overflow_and_broken_output_are_errors() {
    let mut h = Harness::new(&["--max-line-bytes", "4"]);
    h.write(b"12345\n");
    h.close();
    assert_eq!(h.wait(), 1);
    let mut command = Command::new(env!("CARGO_BIN_EXE_sluice"));
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    child.stdin.take().unwrap().write_all(b"hello\n").unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(1));
}

#[test]
fn sigterm_cancels_filter_even_when_input_stays_open() {
    let mut h = Harness::new(&["--watch-for", "ready"]);
    h.write(b"ready");
    assert_eq!(h.event()["type"], "watch"); // handlers installed and actively reading
    unsafe {
        libc::kill(h.child.id() as i32, libc::SIGTERM);
    }
    let _batch = h.event();
    let end = h.event();
    assert_eq!(end["reason"], "signal");
    assert_eq!(end["signal"], 15);
    assert_eq!(h.wait(), 143);
}

#[test]
fn sigterm_reaches_owned_process_group() {
    let mut h = Harness::new(&[
        "--watch-for",
        "ready",
        "run",
        "--",
        "sh",
        "-c",
        "trap 'exit 23' TERM; printf ready; while :; do sleep 1; done",
    ]);
    assert_eq!(h.event()["type"], "watch");
    unsafe {
        libc::kill(h.child.id() as i32, libc::SIGTERM);
    }
    let mut end = h.event();
    while end["type"] != "end" {
        end = h.event();
    }
    assert_eq!(end["reason"], "signal");
    assert_eq!(end["signal"], 15);
    assert_eq!(h.wait(), 143);
}

fn wait_for_file(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "file readiness timeout: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn interruption_during_hook_still_forwards_term_to_owned_command() {
    let directory = tempfile::tempdir().unwrap();
    let ready = directory.path().join("hook-ready");
    let caught = directory.path().join("caught-term");
    let mut h = Harness::new(&[
        "--watch-for",
        "ready",
        "--notify",
        "sh",
        "--notify-arg=-c",
        "--notify-arg=printf ready > \"$1\"; sleep 60",
        "--notify-arg=hook",
        "--notify-arg",
        ready.to_str().unwrap(),
        "run",
        "--",
        "sh",
        "-c",
        "trap 'printf caught > \"$1\"; exit 23' TERM; printf 'ready\\n'; while :; do sleep 1; done",
        "job",
        caught.to_str().unwrap(),
    ]);
    wait_for_file(&ready);
    unsafe {
        libc::kill(h.child.id() as i32, libc::SIGTERM);
    }
    assert_eq!(h.wait(), 143);
    assert_eq!(std::fs::read(caught).unwrap(), b"caught");
}

#[test]
#[cfg(target_os = "linux")]
fn blocked_stdout_cancellation_is_bounded_and_forwards_term() {
    use std::os::fd::AsRawFd;
    let directory = tempfile::tempdir().unwrap();
    let pid_path = directory.path().join("pid");
    let caught = directory.path().join("caught-term");
    // Clean up even if a regression makes the test assertion fail.
    struct GroupCleanup(std::path::PathBuf);
    impl Drop for GroupCleanup {
        fn drop(&mut self) {
            if let Ok(pid) = std::fs::read_to_string(&self.0) {
                if let Ok(pid) = pid.trim().parse::<i32>() {
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                }
            }
        }
    }
    let _cleanup = GroupCleanup(pid_path.clone());
    let child = Command::new(env!("CARGO_BIN_EXE_sluice"))
        .args(["--max-bytes", "4", "run", "--", "sh", "-c",
            "trap 'printf caught > \"$2\"; exit 23' TERM; printf '%s' \"$$\" > \"$1\"; while :; do printf 'abcd\\n'; done",
            "job", pid_path.to_str().unwrap(), caught.to_str().unwrap()])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let (_tx, events) = mpsc::channel();
    let mut h = Harness { child, events }; // Deliberately do not read stdout.
    let fd = h.child.stdout.as_ref().unwrap().as_raw_fd();
    let capacity = unsafe { libc::fcntl(fd, libc::F_GETPIPE_SZ) };
    assert!(capacity >= 4096);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut pending: libc::c_int = 0;
        assert_eq!(unsafe { libc::ioctl(fd, libc::FIONREAD, &mut pending) }, 0);
        if pending >= (capacity - 4096).max(1) {
            break;
        }
        assert!(Instant::now() < deadline, "stdout did not fill");
        std::thread::sleep(Duration::from_millis(10));
    }
    unsafe {
        libc::kill(h.child.id() as i32, libc::SIGTERM);
    }
    assert_eq!(h.wait(), 143);
    assert_eq!(std::fs::read(caught).unwrap(), b"caught");
}
