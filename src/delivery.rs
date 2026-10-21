use crate::{
    cli::Options,
    engine::{Event, Sink},
};
use std::{
    fs::File,
    io::{self, Seek, Write},
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender},
    },
    time::{Duration, Instant},
};

pub struct Output {
    writer: SyncSender<PendingWrite>,
    notify: Option<PathBuf>,
    args: Vec<String>,
    timeout: Duration,
    signal: Arc<AtomicUsize>,
}

struct PendingWrite {
    bytes: Arc<Vec<u8>>,
    result: SyncSender<io::Result<()>>,
}

impl Output {
    pub fn new(options: &Options, signal: Arc<AtomicUsize>) -> Self {
        // Only this worker can block on stdout. The controller waits for an
        // acknowledgement, but can abandon the write when cancellation arrives.
        // There is at most one outstanding event; no output queue can grow.
        let (writer, receiver) = mpsc::sync_channel::<PendingWrite>(1);
        std::thread::spawn(move || {
            let mut stdout = io::BufWriter::new(io::stdout());
            for pending in receiver {
                let result = stdout
                    .write_all(&pending.bytes)
                    .and_then(|()| stdout.flush());
                let failed = result.is_err();
                let _ = pending.result.send(result);
                if failed {
                    break;
                }
            }
        });
        Self {
            writer,
            notify: options.notify.clone(),
            args: options.notify_arg.clone(),
            timeout: options.notify_timeout,
            signal,
        }
    }
}

impl Sink for Output {
    fn emit(&mut self, event: Event) -> io::Result<()> {
        let mut json = serde_json::to_vec(&event)?;
        json.push(b'\n');
        let json = Arc::new(json);
        let (result, receiver) = mpsc::sync_channel(1);
        self.writer
            .send(PendingWrite {
                bytes: json.clone(),
                result,
            })
            .map_err(|_| io::Error::other("stdout worker disconnected"))?;
        loop {
            match receiver.recv_timeout(Duration::from_millis(10)) {
                Ok(result) => {
                    result?;
                    break;
                }
                Err(RecvTimeoutError::Timeout) => {
                    if self.signal.load(Ordering::Relaxed) != 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "stdout delivery interrupted; pending event may be incomplete",
                        ));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("stdout worker disconnected"));
                }
            }
        }
        let Some(executable) = &self.notify else {
            return Ok(());
        };
        // A seekable temporary stdin avoids deadlock when a hook never reads stdin.
        // The file is unnamed and removed automatically, including on failures.
        let mut input: File = tempfile::tempfile()?;
        input.write_all(&json)?;
        input.rewind()?;
        let mut hook = Command::new(executable)
            .args(&self.args)
            .stdin(input)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0)
            .spawn()?;
        let deadline = Instant::now() + self.timeout;
        loop {
            if let Some(status) = hook.try_wait()? {
                if status.success() {
                    return Ok(());
                }
                return Err(io::Error::other(format!(
                    "notification executable failed: {status}"
                )));
            }
            if Instant::now() >= deadline || self.signal.load(Ordering::Relaxed) != 0 {
                kill_group(hook.id(), libc::SIGKILL);
                let _ = hook.wait();
                return Err(io::Error::other(
                    "notification executable timed out or was interrupted; event not retried",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

pub fn kill_group(pid: u32, signal: i32) {
    // SAFETY: kill accepts a numeric process-group ID; no memory is dereferenced.
    // Each child is started in its own group before exec.
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}
