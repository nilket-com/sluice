#![cfg(target_os = "linux")]
use serde_json::Value;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sluice"))
}
fn collect(root: &Path) -> Vec<PathBuf> {
    let result = bin()
        .args(["--outbox", root.to_str().unwrap(), "--job", "synthetic"])
        .arg("run")
        .args(["--", "sh", "-c", "printf 'complete\\n'"])
        .output()
        .unwrap();
    assert!(result.status.success(), "{:?}", result);
    assert_eq!(result.stdout, b"complete\n");
    let mut events = Vec::new();
    for run in fs::read_dir(root).unwrap() {
        let run = run.unwrap().path();
        if !run.is_dir() {
            continue;
        }
        for event in fs::read_dir(run).unwrap() {
            events.push(event.unwrap().path());
        }
    }
    events.sort();
    events
}
fn dispatch(root: &Path, script: &str, timeout: &str) -> std::process::Output {
    bin()
        .args([
            "outbox",
            "dispatch",
            root.to_str().unwrap(),
            "--adapter",
            "sh",
            "--adapter-arg",
            "-c",
            "--adapter-arg",
            script,
            "--timeout",
            timeout,
        ])
        .output()
        .unwrap()
}
fn state(path: &Path) -> Value {
    let output = bin()
        .args([
            "outbox",
            "show",
            path.parent().unwrap().parent().unwrap().to_str().unwrap(),
            path.file_name().unwrap().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["delivery"].clone()
}

#[test]
fn submitted_requires_explicit_receipt_and_identity_survives_restart() {
    let temp = tempfile::tempdir().unwrap();
    let paths = collect(temp.path());
    let event: Value =
        serde_json::from_slice(&fs::read(paths[0].join("event.json")).unwrap()).unwrap();
    assert_eq!(event["job"], "synthetic");
    assert_eq!(event["outbox"], temp.path().to_str().unwrap());
    assert!(
        dispatch(temp.path(), "cat; printf receipt >&2", "1s")
            .status
            .success()
    );
    assert_eq!(state(&paths[0])["state"], "submitted");
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(paths[0].join("stdout")).unwrap()).unwrap()["event_id"],
        event["event_id"]
    );
    assert_eq!(fs::read(paths[0].join("stderr")).unwrap(), b"receipt");
    assert!(dispatch(temp.path(), "exit 99", "1s").status.success());
    assert_eq!(state(&paths[0])["state"], "submitted");
    let output = bin()
        .args([
            "outbox",
            "ack",
            temp.path().to_str().unwrap(),
            event["event_id"].as_str().unwrap(),
            "--proof",
            "mock recipient received stable ID",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(state(&paths[0])["state"], "received");
    assert!(paths[0].join("receipt.json").exists());
}
#[test]
fn timeout_keeps_evidence_unknown_and_does_not_retry() {
    let temp = tempfile::tempdir().unwrap();
    let paths = collect(temp.path());
    assert!(
        !dispatch(
            temp.path(),
            "printf queued; printf delayed >&2; exec sleep 5",
            "50ms"
        )
        .status
        .success()
    );
    assert_eq!(state(&paths[0])["state"], "unknown");
    assert_eq!(fs::read(paths[0].join("stdout")).unwrap(), b"queued");
    assert_eq!(fs::read(paths[0].join("stderr")).unwrap(), b"delayed");
    assert!(
        !dispatch(temp.path(), "printf DUPLICATE", "1s")
            .status
            .success()
    );
    assert_eq!(fs::read(paths[0].join("stdout")).unwrap(), b"queued");
}
#[test]
fn killed_dispatcher_recovers_without_duplicate_and_lock_prevents_parallel_dispatch() {
    let temp = tempfile::tempdir().unwrap();
    let paths = collect(temp.path());
    let mut child = bin()
        .args([
            "outbox",
            "dispatch",
            temp.path().to_str().unwrap(),
            "--adapter",
            "sh",
            "--adapter-arg",
            "-c",
            "--adapter-arg",
            "printf before-kill; exec sleep 60",
        ])
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if fs::read(paths[0].join("stdout")).unwrap_or_default() == b"before-kill" {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let blocked = dispatch(temp.path(), "printf DUPLICATE", "1s");
    assert!(!blocked.status.success());
    child.kill().unwrap();
    child.wait().unwrap();
    let status = bin()
        .args(["outbox", "status", temp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(status.status.success());
    assert_eq!(state(&paths[0])["state"], "attempted");
    assert!(
        !dispatch(temp.path(), "printf DUPLICATE", "1s")
            .status
            .success()
    );
    assert_eq!(fs::read(paths[0].join("stdout")).unwrap(), b"before-kill");
}
#[test]
fn capture_is_bounded_and_notification_failure_cannot_kill_observed_job() {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("job-finished");
    let outbox = temp.path().join("events");
    let mut collector = bin()
        .args([
            "--outbox",
            outbox.to_str().unwrap(),
            "--watch-for",
            "ready",
            "run",
            "--",
            "sh",
            "-c",
        ])
        .arg(format!(
            "printf 'ready\\n'; sleep .3; touch '{}'",
            marker.display()
        ))
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !outbox.exists() || fs::read_dir(&outbox).unwrap().next().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(50));
    assert!(!dispatch(&outbox, "yes overflow", "50ms").status.success());
    assert!(collector.wait().unwrap().success());
    assert!(marker.exists());
    for run in fs::read_dir(outbox).unwrap() {
        let p = run.unwrap().path();
        if !p.is_dir() {
            continue;
        }
        for event in fs::read_dir(p).unwrap() {
            let p = event.unwrap().path();
            if let Ok(meta) = fs::metadata(p.join("stdout")) {
                assert!(meta.len() <= 1048576);
            }
        }
    }
}
#[test]
fn unsafe_ids_and_empty_proof_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let paths = collect(temp.path());
    for id in ["../outside", "bad/id", "-"] {
        assert!(
            !bin()
                .args(["outbox", "show", temp.path().to_str().unwrap(), id])
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let id = paths[0].file_name().unwrap().to_str().unwrap();
    assert!(
        !bin()
            .args([
                "outbox",
                "ack",
                temp.path().to_str().unwrap(),
                id,
                "--proof",
                ""
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
}
#[test]
fn pipe_collection_does_not_run_adapter_and_preserves_raw_output() {
    let temp = tempfile::tempdir().unwrap();
    let mut child = bin()
        .args(["--outbox", temp.path().to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"plain\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"plain\n");
}

#[test]
fn independent_backstop_distinguishes_submission_receipt_and_age() {
    let temp = tempfile::tempdir().unwrap();
    let paths = collect(temp.path());
    let status = |extra: &[&str]| {
        bin()
            .args(["outbox", "status", temp.path().to_str().unwrap()])
            .args(extra)
            .output()
            .unwrap()
    };
    assert!(status(&[]).status.success());
    assert!(!status(&["--fail-unreceived"]).status.success());
    assert!(
        status(&["--fail-unreceived", "--older-than", "1h"])
            .status
            .success()
    );
    assert!(dispatch(temp.path(), "cat", "1s").status.success());
    assert!(!status(&["--fail-unreceived"]).status.success());
    for path in paths {
        assert!(
            bin()
                .args([
                    "outbox",
                    "ack",
                    temp.path().to_str().unwrap(),
                    path.file_name().unwrap().to_str().unwrap(),
                    "--proof",
                    "mock recipient confirms receipt"
                ])
                .output()
                .unwrap()
                .status
                .success()
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(path.join("delivery-at-ack.json")).unwrap())
                .unwrap()["state"],
            "submitted"
        );
    }
    assert!(status(&["--fail-unreceived"]).status.success());
}

#[test]
fn recipient_can_show_and_ack_before_adapter_returns_without_receipt_being_overwritten() {
    let temp = tempfile::tempdir().unwrap();
    let paths = collect(temp.path());
    let adapter = temp.path().join("adapter.py");
    let marker = temp.path().join("recipient-acked");
    fs::write(&adapter, r#"import json, sys, subprocess, time, pathlib
event = json.load(sys.stdin)
base = [sys.argv[1], 'outbox']
subprocess.run(base + ['show', event['outbox'], event['event_id']], check=True)
subprocess.run(base + ['status', event['outbox']], check=True)
subprocess.run(base + ['ack', event['outbox'], event['event_id'], '--proof', 'synthetic recipient confirms the exact event ID'], check=True)
pathlib.Path(sys.argv[2]).write_text('acked')
time.sleep(0.4)
"#).unwrap();
    let mut child = bin()
        .args([
            "outbox",
            "dispatch",
            temp.path().to_str().unwrap(),
            "--adapter",
            "python3",
            "--adapter-arg",
            adapter.to_str().unwrap(),
            "--adapter-arg",
            env!("CARGO_BIN_EXE_sluice"),
            "--adapter-arg",
            marker.to_str().unwrap(),
        ])
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(child.try_wait().unwrap().is_none());
    assert_eq!(state(&paths[0])["state"], "received");
    let receipt = fs::read(paths[0].join("receipt.json")).unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(state(&paths[0])["state"], "received");
    assert_eq!(fs::read(paths[0].join("receipt.json")).unwrap(), receipt);
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(paths[0].join("delivery-at-ack.json")).unwrap())
            .unwrap()["state"],
        "attempted"
    );
}

#[test]
fn outbox_contains_only_notification_events_not_routine_batches() {
    let temp = tempfile::tempdir().unwrap();
    let mut child = bin()
        .args([
            "--outbox",
            temp.path().to_str().unwrap(),
            "--max-bytes",
            "4",
            "--watch-for",
            "done",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for _ in 0..100 {
        input.write_all(b"routine\n").unwrap();
    }
    input.write_all(b"done\n").unwrap();
    drop(input);
    assert!(child.wait().unwrap().success());
    let mut kinds = Vec::new();
    for run in fs::read_dir(temp.path()).unwrap() {
        let run = run.unwrap().path();
        if !run.is_dir() {
            continue;
        }
        for event in fs::read_dir(run).unwrap() {
            let event: Value = serde_json::from_slice(
                &fs::read(event.unwrap().path().join("event.json")).unwrap(),
            )
            .unwrap();
            kinds.push(event["type"].as_str().unwrap().to_owned());
        }
    }
    kinds.sort();
    assert_eq!(kinds, ["end", "watch"]);
}

#[test]
fn hundred_routine_batches_create_only_one_end_notification() {
    let temp = tempfile::tempdir().unwrap();
    let mut child = bin()
        .args([
            "--outbox",
            temp.path().to_str().unwrap(),
            "--max-bytes",
            "4",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for _ in 0..100 {
        input.write_all(b"routine\n").unwrap();
    }
    drop(input);
    assert!(child.wait().unwrap().success());
    let output = bin()
        .args(["outbox", "status", temp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["states"]["pending"],
        1
    );
}

#[test]
fn adapter_can_write_large_session_files_while_both_output_captures_stay_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let paths = collect(temp.path());
    let session = temp.path().join("session.log");
    fs::write(&session, vec![b'x'; 2 * 1024 * 1024]).unwrap();
    let adapter = temp.path().join("large-output.py");
    fs::write(
        &adapter,
        r#"import sys, os
with open(sys.argv[1], 'ab') as file:
    file.write(b'x' * (2 * 1024 * 1024))
    file.flush()
    os.fsync(file.fileno())
sys.stdout.buffer.write(b'o' * (3 * 1024 * 1024))
sys.stdout.buffer.flush()
sys.stderr.buffer.write(b'e' * (3 * 1024 * 1024))
sys.stderr.buffer.flush()
"#,
    )
    .unwrap();
    let output = bin()
        .args([
            "outbox",
            "dispatch",
            temp.path().to_str().unwrap(),
            "--adapter",
            "python3",
            "--adapter-arg",
            adapter.to_str().unwrap(),
            "--adapter-arg",
            session.to_str().unwrap(),
            "--timeout",
            "5s",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(fs::metadata(session).unwrap().len(), 4 * 1024 * 1024);
    assert_eq!(state(&paths[0])["state"], "submitted");
    for name in ["stdout", "stderr"] {
        assert_eq!(
            fs::metadata(paths[0].join(name)).unwrap().len(),
            1024 * 1024
        );
    }
    let capture: Value =
        serde_json::from_slice(&fs::read(paths[0].join("capture.json")).unwrap()).unwrap();
    for stream in ["stdout", "stderr"] {
        assert_eq!(capture[stream]["total_bytes"], 3 * 1024 * 1024);
        assert_eq!(capture[stream]["truncated"], true);
        assert_eq!(capture[stream]["drain_complete"], true);
    }
}

#[test]
fn nested_outbox_parents_are_created_and_canonical_path_is_retained() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("one/two/three");
    let paths = collect(&root);
    let event: Value =
        serde_json::from_slice(&fs::read(paths[0].join("event.json")).unwrap()).unwrap();
    assert_eq!(
        event["outbox"],
        fs::canonicalize(root).unwrap().to_str().unwrap()
    );
    let file = temp.path().join("not-directory");
    fs::write(&file, b"x").unwrap();
    assert!(
        !bin()
            .arg("--outbox")
            .arg(file.join("child"))
            .args(["run", "--", "true"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn non_utf8_outbox_path_is_rejected_before_starting_the_observed_job() {
    use std::os::unix::ffi::OsStringExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp
        .path()
        .join(std::ffi::OsString::from_vec(b"bad-\xff".to_vec()));
    let marker = temp.path().join("must-not-run");
    let output = bin()
        .arg("--outbox")
        .arg(&root)
        .args(["run", "--", "touch"])
        .arg(&marker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!root.exists());
    assert!(!marker.exists());
    fs::create_dir(&root).unwrap();
    let output = bin()
        .current_dir(&root)
        .args(["--outbox", "ascii", "run", "--", "touch"])
        .arg(&marker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!marker.exists());
}
