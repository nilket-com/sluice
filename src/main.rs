mod cli;
mod delivery;
mod engine;

use clap::Parser;
use cli::{Action, Cli};
use delivery::{Output, kill_group};
use engine::Engine;
use std::{
    fs::OpenOptions,
    io::{self, Read, Write},
    os::unix::process::{CommandExt, ExitStatusExt},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    time::{Duration, Instant},
};

// At most four 8KiB chunks are queued ahead of the engine. Reading is allowed to
// block here: the main thread owns all timers and exits without joining stdin.
enum Input {
    Bytes(Vec<u8>),
    Eof,
    Error(io::Error),
}

struct Job {
    child: Option<Child>,
    input: Option<Receiver<Input>>,
    signal: Arc<AtomicUsize>,
    stop_deadline: Option<Instant>,
}
impl Drop for Job {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            // Delivery can be interrupted outside the main receive loop. Honor
            // signal forwarding and the same grace period on that error path.
            let signal = self.signal.load(Ordering::Relaxed) as i32;
            if signal != 0 && self.stop_deadline.is_none() {
                kill_group(child.id(), signal);
                self.stop_deadline = Some(Instant::now() + Duration::from_secs(2));
            }
            if let Some(deadline) = self.stop_deadline {
                // Do not reap early: the zombie leader pins the process-group
                // ID until cleanup and prevents reuse during the grace period.
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    // Discard queued output on delivery failure so a command
                    // blocked in write can actually execute its signal trap.
                    let result = self
                        .input
                        .as_ref()
                        .map(|input| input.recv_timeout(remaining.min(Duration::from_millis(50))));
                    match result {
                        Some(Ok(Input::Bytes(_))) | Some(Err(RecvTimeoutError::Timeout)) => (),
                        _ => {
                            std::thread::sleep(remaining);
                            break;
                        }
                    }
                }
            }
            // Also stop group members if the leader has already exited.
            kill_group(child.id(), libc::SIGKILL);
            let _ = child.wait();
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let signal = Arc::new(AtomicUsize::new(0));
    for value in [libc::SIGINT, libc::SIGTERM] {
        if let Err(error) = signal_hook::flag::register_usize(value, signal.clone(), value as usize)
        {
            eprintln!("sluice: cannot install signal handler: {error}");
            std::process::exit(1);
        }
    }
    let result = run(cli, signal.clone());
    let code = match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("sluice: {error}");
            let sig = signal.load(Ordering::Relaxed);
            if sig != 0 { 128 + sig as i32 } else { 1 }
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli, signal: Arc<AtomicUsize>) -> io::Result<i32> {
    let options = cli.options;
    // Validate filters before creating files or starting a child.
    let mut engine =
        Engine::new(&options, Output::new(&options, signal.clone())).map_err(io::Error::other)?;
    let mut log = options
        .log
        .as_ref()
        .map(|p| OpenOptions::new().write(true).create_new(true).open(p))
        .transpose()?;
    let mut job = Job {
        child: None,
        input: None,
        signal: signal.clone(),
        stop_deadline: None,
    };
    let reader: Box<dyn Read + Send> = match cli.command {
        None => Box::new(io::stdin()),
        Some(Action::Run { command }) => {
            let (reader, writer) = os_pipe::pipe()?;
            let child = Command::new(&command[0])
                .args(&command[1..])
                .stdin(Stdio::null())
                .stdout(writer.try_clone()?)
                .stderr(writer)
                .process_group(0)
                .spawn()?;
            job.child = Some(child);
            Box::new(reader)
        }
    };
    let (sender, receiver) = mpsc::sync_channel(4);
    std::thread::spawn(move || {
        let mut reader = reader;
        loop {
            let mut bytes = vec![0; 8192];
            let message = match reader.read(&mut bytes) {
                Ok(0) => Input::Eof,
                Ok(n) => {
                    bytes.truncate(n);
                    Input::Bytes(bytes)
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => Input::Error(e),
            };
            let done = !matches!(message, Input::Bytes(_));
            if sender.send(message).is_err() || done {
                break;
            }
        }
    });

    job.input = Some(receiver);
    let mut eof = false;
    loop {
        let now = Instant::now();
        let sig = signal.load(Ordering::Relaxed) as i32;
        if sig != 0 && job.stop_deadline.is_none() {
            if let Some(child) = &job.child {
                kill_group(child.id(), sig);
            }
            job.stop_deadline = Some(now + Duration::from_secs(2));
            if job.child.is_none() {
                break;
            }
        }
        if job.stop_deadline.is_some_and(|deadline| now >= deadline) {
            if let Some(child) = &job.child {
                kill_group(child.id(), libc::SIGKILL);
            }
            break;
        }
        engine.tick(now)?;
        let timeout = engine
            .deadline()
            .map(|d| d.saturating_duration_since(now))
            .unwrap_or(Duration::from_millis(50))
            .min(Duration::from_millis(50));
        if eof {
            if let Some(child) = &mut job.child {
                if child.try_wait()?.is_some() {
                    break;
                }
            } else {
                break;
            }
            std::thread::sleep(timeout);
            continue;
        }
        match job.input.as_ref().unwrap().recv_timeout(timeout) {
            Ok(Input::Bytes(bytes)) => {
                if let Some(log) = &mut log {
                    log.write_all(&bytes)?;
                }
                engine.feed(&bytes, Instant::now())?;
            }
            Ok(Input::Eof) => {
                engine.finish_input(Instant::now())?;
                eof = true;
            }
            Ok(Input::Error(error)) => return Err(error),
            Err(RecvTimeoutError::Timeout) => (),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("input reader disconnected"));
            }
        }
    }
    if !eof {
        // On cancellation, finish the bytes already accepted by the engine.
        engine.finish_input(Instant::now())?;
    }
    if let Some(log) = &mut log {
        log.flush()?;
    }
    let sig = signal.load(Ordering::Relaxed) as i32;
    let (reason, code, child_signal) = if let Some(child) = &mut job.child {
        if sig != 0 {
            kill_group(child.id(), libc::SIGKILL);
        }
        let status = child.wait()?;
        ("exit", status.code(), status.signal())
    } else {
        ("eof", None, None)
    };
    // Release the reaped leader before a final hook can block or fail. Its old
    // process-group ID must not be targeted after normal completion.
    job.child.take();
    engine.end(
        if sig != 0 { "signal" } else { reason },
        code,
        if sig != 0 { Some(sig) } else { child_signal },
    )?;
    Ok(if sig != 0 {
        128 + sig
    } else {
        code.unwrap_or_else(|| child_signal.map_or(0, |s| 128 + s))
    })
}
