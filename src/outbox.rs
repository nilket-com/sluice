//! Optional disk-backed notification delivery; collector never invokes adapters.
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, OpenOptionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Deliver pending events once; ambiguous attempts are never retried
    Dispatch {
        dir: PathBuf,
        #[arg(long)]
        adapter: PathBuf,
        #[arg(long, allow_hyphen_values = true)]
        adapter_arg: Vec<String>,
        #[arg(long, default_value = "30s", value_parser = crate::cli::duration)]
        timeout: Duration,
    },
    /// Print counts by delivery state
    Status {
        dir: PathBuf,
        /// Exit 1 when notifications remain unreceived (for independent backstops)
        #[arg(long)]
        fail_unreceived: bool,
        /// Only fail for events at least this old, measured from detection
        #[arg(long, requires = "fail_unreceived", value_parser = crate::cli::duration)]
        older_than: Option<Duration>,
    },
    /// Print event and delivery evidence paths
    Show { dir: PathBuf, event_id: String },
    /// Record explicit recipient receipt evidence (not adapter success)
    Ack {
        dir: PathBuf,
        event_id: String,
        #[arg(long)]
        proof: String,
    },
}

pub struct Collector {
    dir: PathBuf,
    run_id: String,
    job: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct State {
    state: String,
    updated_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}
fn state(value: &str) -> State {
    State {
        state: value.into(),
        updated_unix_ms: unix_ms(),
        exit_code: None,
        note: None,
    }
}
// Persist every newly created directory entry, including missing ancestors.
fn durable_mkdir_all(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() || path.is_dir() {
        return Ok(());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    durable_mkdir_all(parent)?;
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => (),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => (),
        Err(e) => return Err(e),
    }
    sync_dir(parent)
}
fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}
fn atomic(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let parent = path.parent().unwrap();
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut temp, value)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    sync_dir(parent)
}
fn load(path: &Path) -> io::Result<State> {
    Ok(serde_json::from_reader(File::open(path)?)?)
}
fn valid(id: &str) -> bool {
    id.split_once('-').is_some_and(|(run, seq)| {
        run.len() == 32
            && seq.len() == 16
            && run
                .bytes()
                .chain(seq.bytes())
                .all(|c| c.is_ascii_hexdigit())
    })
}
fn event_dir(root: &Path, id: &str) -> io::Result<PathBuf> {
    if !valid(id) {
        return Err(io::Error::other("invalid event ID"));
    }
    let (run, _) = id
        .rsplit_once('-')
        .ok_or_else(|| io::Error::other("invalid event ID"))?;
    Ok(root.join(run).join(id))
}
impl Collector {
    pub fn new(root: &Path, job: Option<String>) -> io::Result<Self> {
        if job.as_ref().is_some_and(|s| s.len() > 4096) {
            return Err(io::Error::other("job label exceeds 4096 bytes"));
        }
        if root.to_str().is_none() {
            return Err(io::Error::other("outbox path must be UTF-8"));
        }
        durable_mkdir_all(root)?;
        let root = fs::canonicalize(root)?;
        if root.to_str().is_none() {
            return Err(io::Error::other("canonical outbox path must be UTF-8"));
        }
        let mut random = [0u8; 16];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        let mut run_id = String::with_capacity(32);
        const HEX: &[u8] = b"0123456789abcdef";
        for byte in random {
            run_id.push(HEX[(byte >> 4) as usize] as char);
            run_id.push(HEX[(byte & 15) as usize] as char);
        }
        let dir = root.join(&run_id);
        fs::DirBuilder::new().mode(0o700).create(&dir)?;
        sync_dir(&root)?;
        Ok(Self { dir, run_id, job })
    }
    pub fn persist(&self, event: &crate::engine::Event) -> io::Result<()> {
        if event.kind == "batch" {
            return Ok(());
        }
        let id = format!("{}-{:016x}", self.run_id, event.seq);
        let dir = self.dir.join(&id);
        fs::DirBuilder::new().mode(0o700).create(&dir)?;
        // No pending state exists until the immutable event is durable.
        let mut value = serde_json::to_value(event)?;
        value["detected_unix_ms"] = unix_ms().into();
        value["event_id"] = id.into();
        value["run_id"] = self.run_id.clone().into();
        value["outbox"] = self
            .dir
            .parent()
            .unwrap()
            .to_str()
            .expect("validated UTF-8 outbox root")
            .to_owned()
            .into();
        if let Some(job) = &self.job {
            value["job"] = job.clone().into();
        }
        atomic(&dir.join("event.json"), &value)?;
        atomic(&dir.join("state.json"), &state("pending"))?;
        sync_dir(&self.dir)
    }
}
struct Lock(File);
impl Lock {
    fn acquire(root: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(root.join(".lock"))?;
        // SAFETY: valid owned fd, advisory exclusive nonblocking flock.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::other("outbox is busy"));
        }
        Ok(Self(file))
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
fn entries(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    for run in fs::read_dir(root)? {
        let run = run?;
        if !run.file_type()?.is_dir() {
            continue;
        }
        for event in fs::read_dir(run.path())? {
            let p = event?.path();
            if p.join("state.json").is_file() {
                result.push(p);
            }
        }
    }
    result.sort();
    Ok(result)
}
fn recover(path: &Path) -> io::Result<State> {
    let mut current = load(&path.join("state.json"))?;
    if current.state == "attempted" {
        for name in ["stdout", "stderr"] {
            if let Ok(file) = File::open(path.join(name)) {
                file.sync_all()?;
            }
        }
        current = state("unknown");
        current.note = Some("dispatcher interrupted; submission uncertain, not retried".into());
        atomic(&path.join("state.json"), &current)?;
    }
    Ok(current)
}
// Receipt is immutable, independent of dispatch state, and takes precedence.
fn effective(path: &Path, mut current: State) -> io::Result<State> {
    match File::open(path.join("receipt.json")) {
        Ok(file) => {
            let receipt: serde_json::Value = serde_json::from_reader(file)?;
            if !receipt["proof"]
                .as_str()
                .is_some_and(|p| !p.trim().is_empty() && p.len() <= 4096)
            {
                return Err(io::Error::other("invalid persisted recipient receipt"));
            }
            current.state = "received".into();
            current.note =
                Some("recipient receipt recorded; adapter evidence is retained separately".into());
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(e),
    }
    Ok(current)
}
pub fn run(
    action: Action,
    signal: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> io::Result<i32> {
    let root = match &action {
        Action::Dispatch { dir, .. }
        | Action::Status { dir, .. }
        | Action::Show { dir, .. }
        | Action::Ack { dir, .. } => dir,
    };
    let root = fs::canonicalize(root)?;
    match action {
        Action::Status {
            fail_unreceived,
            older_than,
            ..
        } => {
            let mut counts = std::collections::BTreeMap::new();
            let mut overdue = 0;
            for p in entries(&root)? {
                let current = effective(&p, load(&p.join("state.json"))?)?;
                if current.state != "received" {
                    let event: serde_json::Value =
                        serde_json::from_reader(File::open(p.join("event.json"))?)?;
                    let detected = event["detected_unix_ms"]
                        .as_u64()
                        .ok_or_else(|| io::Error::other("event lacks detection timestamp"))?;
                    if u128::from(unix_ms().saturating_sub(detected))
                        >= older_than.unwrap_or_default().as_millis()
                    {
                        overdue += 1;
                    }
                }
                *counts.entry(current.state).or_insert(0usize) += 1;
            }
            println!(
                "{}",
                serde_json::json!({"states":counts,"unreceived_past_threshold":overdue})
            );
            if fail_unreceived && overdue > 0 {
                return Ok(1);
            }
        }
        Action::Show { event_id, .. } => {
            let path = event_dir(&root, &event_id)?;
            let current = effective(&path, load(&path.join("state.json"))?)?;
            println!(
                "{}",
                serde_json::json!({"event":serde_json::from_reader::<_,serde_json::Value>(File::open(path.join("event.json"))?)?,"delivery":current,"delivery_attempt":load(&path.join("state.json"))?,"evidence_dir":path})
            );
        }
        Action::Ack {
            event_id, proof, ..
        } => {
            if proof.trim().is_empty() || proof.len() > 4096 {
                return Err(io::Error::other(
                    "proof must contain 1–4096 bytes of recipient receipt evidence",
                ));
            }
            let path = event_dir(&root, &event_id)?;
            let _receipt_lock = Lock::acquire(&path)?;
            let current = effective(&path, load(&path.join("state.json"))?)?;
            if !["attempted", "submitted", "unknown", "received"].contains(&current.state.as_str())
            {
                return Err(io::Error::other(
                    "cannot acknowledge an event never attempted",
                ));
            }
            if current.state == "received" {
                return Err(io::Error::other("receipt already recorded"));
            }
            atomic(&path.join("delivery-at-ack.json"), &current)?;
            let mut receipt = tempfile::NamedTempFile::new_in(&path)?;
            serde_json::to_writer(
                &mut receipt,
                &serde_json::json!({"proof":proof,"received_unix_ms":unix_ms(),"delivery_at_ack":current}),
            )?;
            receipt.write_all(b"\n")?;
            receipt.as_file().sync_all()?;
            receipt
                .persist_noclobber(path.join("receipt.json"))
                .map_err(|e| e.error)?;
            sync_dir(&path)?;
        }
        Action::Dispatch {
            adapter,
            adapter_arg,
            timeout,
            ..
        } => {
            let _dispatch_lock = Lock::acquire(&root)?;
            let mut uncertain = false;
            for path in entries(&root)? {
                if signal.load(std::sync::atomic::Ordering::Relaxed) != 0 {
                    break;
                }
                let current = effective(&path, recover(&path)?)?;
                uncertain |= current.state == "unknown";
                if current.state != "pending" {
                    continue;
                }
                if signal.load(std::sync::atomic::Ordering::Relaxed) != 0 {
                    break;
                }
                dispatch(&path, &adapter, &adapter_arg, timeout, &signal)?;
                uncertain |= effective(&path, load(&path.join("state.json"))?)?.state == "unknown";
                if signal.load(std::sync::atomic::Ordering::Relaxed) != 0 {
                    break;
                }
            }
            if uncertain && signal.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                return Ok(1);
            }
        }
    }
    let sig = signal.load(std::sync::atomic::Ordering::Relaxed) as i32;
    Ok(if sig == 0 { 0 } else { 128 + sig })
}
fn dispatch(
    path: &Path,
    adapter: &Path,
    args: &[String],
    timeout: Duration,
    signal: &std::sync::atomic::AtomicUsize,
) -> io::Result<()> {
    let input = File::open(path.join("event.json"))?;
    let stdout = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path.join("stdout"))?;
    let stderr = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path.join("stderr"))?;
    let mut attempted = state("attempted");
    attempted.note = Some("adapter invocation started; exit zero means submitted only".into());
    atomic(
        &path.join("attempt.json"),
        &serde_json::json!({"started_unix_ms":unix_ms(),"adapter":adapter,"args":args,"timeout_ms":timeout.as_millis(),"capture_limit_bytes":1048576}),
    )?;
    atomic(&path.join("state.json"), &attempted)?;
    let (stdout_reader, stdout_writer) = os_pipe::pipe()?;
    let (stderr_reader, stderr_writer) = os_pipe::pipe()?;
    let stdout_capture = capture(stdout_reader, stdout.try_clone()?);
    let stderr_capture = capture(stderr_reader, stderr.try_clone()?);
    let parent = std::process::id() as libc::pid_t;
    let mut command = Command::new(adapter);
    command
        .args(args)
        .stdin(input)
        .stdout(stdout_writer)
        .stderr(stderr_writer)
        .process_group(0);
    // SAFETY: only async-signal-safe libc calls run between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::_exit(125);
            }
            Ok(())
        });
    }
    let spawned = if signal.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        command.spawn()
    } else {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "dispatch interrupted before adapter spawn",
        ))
    };
    // Command owns the parent's pipe writers. Drop them before waiting for EOF.
    drop(command);
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            let mut result = state("unknown");
            result.note = Some(format!(
                "spawn failed: {error}; explicit reconciliation required"
            ));
            finish_capture(path, stdout_capture, stderr_capture)?;
            atomic(&path.join("state.json"), &result)?;
            return Ok(());
        }
    };
    let deadline = Instant::now() + timeout;
    let mut result;
    loop {
        if exited(child.id())? {
            // waitid leaves the leader unreaped, pinning its process-group ID.
            crate::delivery::kill_group(child.id(), libc::SIGKILL);
            let status = child.wait()?;
            result = state(if status.success() {
                "submitted"
            } else {
                "unknown"
            });
            result.exit_code = status.code();
            result.note = Some(format!(
                "adapter exit {status}; receipt requires recipient evidence"
            ));
            break;
        }
        if Instant::now() >= deadline || signal.load(std::sync::atomic::Ordering::Relaxed) != 0 {
            crate::delivery::kill_group(child.id(), libc::SIGKILL);
            let _ = child.wait();
            result = state("unknown");
            result.note =
                Some("adapter timeout or interruption; submission uncertain, not retried".into());
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    finish_capture(path, stdout_capture, stderr_capture)?;
    stdout.sync_all()?;
    stderr.sync_all()?;
    atomic(&path.join("state.json"), &result)
}

#[derive(Serialize)]
struct Capture {
    total_bytes: u64,
    captured_bytes: usize,
    truncated: bool,
    drain_complete: bool,
}
struct CaptureWorker {
    handle: std::thread::JoinHandle<io::Result<Capture>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
// Drain independently so either adapter pipe can fill without stalling its other
// stream. Keep only a bounded prefix; discard overflow without limiting the
// adapter's unrelated filesystem writes or affecting its exit status.
fn capture(mut reader: os_pipe::PipeReader, mut file: File) -> CaptureWorker {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopping = stop.clone();
    let handle = std::thread::spawn(move || {
        // Nonblocking reads let cleanup finish even if a misbehaving adapter
        // escaped its process group while retaining an output pipe.
        let flags = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut stop_deadline = None;
        let mut drain_complete = false;
        const LIMIT: usize = 1024 * 1024;
        let mut total_bytes = 0u64;
        let mut captured_bytes = 0;
        let mut buffer = [0; 8192];
        loop {
            if stopping.load(std::sync::atomic::Ordering::Relaxed) {
                let deadline = stop_deadline
                    .get_or_insert_with(|| Instant::now() + Duration::from_millis(100));
                if Instant::now() >= *deadline {
                    break;
                }
            }
            let n = match reader.read(&mut buffer) {
                Ok(0) => {
                    drain_complete = true;
                    break;
                }
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if stopping.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(e) => return Err(e),
            };
            total_bytes = total_bytes.saturating_add(n as u64);
            let retained = n.min(LIMIT - captured_bytes);
            file.write_all(&buffer[..retained])?;
            captured_bytes += retained;
        }
        file.sync_all()?;
        Ok(Capture {
            total_bytes,
            captured_bytes,
            truncated: total_bytes > captured_bytes as u64,
            drain_complete,
        })
    });
    CaptureWorker { handle, stop }
}
fn finish_capture(path: &Path, stdout: CaptureWorker, stderr: CaptureWorker) -> io::Result<()> {
    stdout
        .stop
        .store(true, std::sync::atomic::Ordering::Relaxed);
    stderr
        .stop
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let stdout = stdout
        .handle
        .join()
        .map_err(|_| io::Error::other("stdout capture worker panicked"))?;
    let stderr = stderr
        .handle
        .join()
        .map_err(|_| io::Error::other("stderr capture worker panicked"))?;
    atomic(
        &path.join("capture.json"),
        &serde_json::json!({"stdout":stdout?, "stderr":stderr?}),
    )
}

fn exited(pid: u32) -> io::Result<bool> {
    // SAFETY: zero initialization is valid for siginfo_t; waitid fills the value.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } != 0)
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
