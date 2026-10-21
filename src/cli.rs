use clap::{Args, Parser, Subcommand, ValueEnum};
use std::{path::PathBuf, time::Duration};

#[derive(Debug, Parser)]
#[command(version, about, subcommand_precedence_over_arg = true)]
pub struct Cli {
    #[command(flatten)]
    pub options: Options,
    #[command(subcommand)]
    pub command: Option<Action>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Raw,
    Markdown,
    Jsonl,
}

#[derive(Debug, Args)]
pub struct Options {
    /// Maximum batch text bytes (output framing is additional)
    #[arg(long, default_value = "10KiB", value_parser = size)]
    pub max_bytes: usize,
    /// Maximum age of a nonempty batch; continued input does not reset it
    #[arg(long, default_value = "10m", value_parser = duration)]
    pub max_delay: Duration,
    /// Include lines matching any of these regular expressions
    #[arg(long, value_name = "REGEX")]
    pub filter_in: Vec<String>,
    /// Exclude lines matching any of these regular expressions
    #[arg(long, value_name = "REGEX")]
    pub filter_out: Vec<String>,
    /// Immediately emit an event for this literal text, once per physical line
    #[arg(long, value_name = "TEXT")]
    pub watch_for: Vec<String>,
    /// Create a new file containing the original input bytes (never overwrite)
    #[arg(long, value_name = "PATH")]
    pub log: Option<PathBuf>,
    /// Maximum physical line bytes, including LF; longer lines fail explicitly
    #[arg(long, default_value = "64KiB", value_parser = size)]
    pub max_line_bytes: usize,
    /// Plain text (default), Markdown, or structured JSONL (schema version 1)
    #[arg(long, default_value = "raw", value_enum)]
    pub format: Format,
    /// Deliver each event as JSONL to an executable, independent of stdout format
    #[arg(long, value_name = "EXECUTABLE")]
    pub notify: Option<PathBuf>,
    /// Literal argument to the notification executable (repeatable; no shell)
    #[arg(
        long,
        value_name = "ARG",
        allow_hyphen_values = true,
        requires = "notify"
    )]
    pub notify_arg: Vec<String>,
    /// Time allowed per notification; failures stop processing without retry
    #[arg(long, default_value = "30s", value_parser = duration)]
    pub notify_timeout: Duration,
}

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Run a command with merged stdout/stderr, forward signals, and report its status
    Run {
        #[arg(required = true, last = true, value_name = "COMMAND")]
        command: Vec<std::ffi::OsString>,
    },
}

pub fn size(input: &str) -> Result<usize, String> {
    let (digits, multiplier) = if let Some(s) = input.strip_suffix("MiB") {
        (s, 1024 * 1024)
    } else if let Some(s) = input.strip_suffix("KiB") {
        (s, 1024)
    } else if let Some(s) = input.strip_suffix('B') {
        (s, 1)
    } else {
        (input, 1)
    };
    let bytes = digits
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier));
    match bytes {
        Some(n @ 4..=16_777_216) => Ok(n),
        _ => Err("expected 4 bytes through 16MiB (integer, B, KiB, or MiB)".into()),
    }
}

pub fn duration(input: &str) -> Result<Duration, String> {
    let (digits, multiplier) = if let Some(s) = input.strip_suffix("ms") {
        (s, 1)
    } else if let Some(s) = input.strip_suffix('s') {
        (s, 1000)
    } else if let Some(s) = input.strip_suffix('m') {
        (s, 60_000)
    } else if let Some(s) = input.strip_suffix('h') {
        (s, 3_600_000)
    } else {
        return Err("expected an integer followed by ms, s, m, or h".into());
    };
    let ms = digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier));
    match ms {
        Some(n @ 1..=86_400_000) => Ok(Duration::from_millis(n)),
        _ => Err("duration must be between 1ms and 24h".into()),
    }
}
