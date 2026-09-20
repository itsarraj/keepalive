use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

use keepalive::duration::parse_duration;
use keepalive::restart::{Config, RestartPolicy as Policy};
use keepalive::supervisor::run;

#[derive(ValueEnum, Clone, Copy, Debug)]
enum RestartArg {
    /// Restart only when the process exits nonzero or is killed by a signal.
    OnFailure,
    /// Restart no matter how the process exited, including a clean exit 0.
    Always,
}

/// Babysits a single command: restarts it on crash with exponential
/// backoff, and captures its stdout/stderr to a log file — a
/// `supervisord` for people who don't want Python, a config file, or a
/// whole process group.
#[derive(Parser, Debug)]
#[command(name = "keepalive", version, about)]
struct Cli {
    /// The command to run, handed to `sh -c` wholesale (quote it as one
    /// argument: keepalive "python3 worker.py --queue default")
    command: String,

    /// Where to append the child's stdout/stderr and keepalive's own
    /// start/restart banner lines
    #[arg(long, value_name = "FILE")]
    log: PathBuf,

    /// When to restart: only on failure, or always (even after a clean exit)
    #[arg(long, value_enum, default_value_t = RestartArg::OnFailure)]
    restart: RestartArg,

    /// Give up after this many restarts (unset = unlimited)
    #[arg(long, value_name = "N")]
    max_restarts: Option<u32>,

    /// Initial delay before the first restart (e.g. 500ms, 1s)
    #[arg(long, value_name = "DURATION", default_value = "1s")]
    backoff_base: String,

    /// Backoff never grows past this delay
    #[arg(long, value_name = "DURATION", default_value = "60s")]
    backoff_max: String,

    /// Multiply the delay by this after each consecutive failure
    #[arg(long, default_value_t = 2.0)]
    backoff_multiplier: f64,

    /// A run that stays up at least this long resets the backoff delay
    /// back to --backoff-base on its next crash
    #[arg(long, value_name = "DURATION", default_value = "60s")]
    reset_after: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let config = Config {
        policy: match cli.restart {
            RestartArg::OnFailure => Policy::OnFailure,
            RestartArg::Always => Policy::Always,
        },
        max_restarts: cli.max_restarts,
        backoff_base: parse_duration(&cli.backoff_base)?,
        backoff_max: parse_duration(&cli.backoff_max)?,
        backoff_multiplier: cli.backoff_multiplier,
        reset_after: parse_duration(&cli.reset_after)?,
    };

    let shutdown = Arc::new(AtomicBool::new(false));
    let current_pid = Arc::new(AtomicI32::new(0));

    spawn_signal_forwarder(Arc::clone(&shutdown), Arc::clone(&current_pid));

    println!(
        "keepalive: supervising '{}', logging to {}",
        cli.command,
        cli.log.display()
    );

    let code = run(&cli.command, &cli.log, &config, shutdown, current_pid)?;
    std::process::exit(code);
}

/// Watches for SIGINT/SIGTERM against `keepalive` itself; forwards the
/// same signal to whatever child is currently running (via
/// `current_pid`) and marks `shutdown` so the supervisor loop doesn't
/// restart the child that signal is about to terminate.
fn spawn_signal_forwarder(shutdown: Arc<AtomicBool>, current_pid: Arc<AtomicI32>) {
    thread::spawn(move || {
        let Ok(mut signals) = signal_hook::iterator::Signals::new([
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGTERM,
        ]) else {
            return;
        };
        for sig in signals.forever() {
            shutdown.store(true, Ordering::SeqCst);
            let pid = current_pid.load(Ordering::SeqCst);
            if pid > 0 {
                let signal = if sig == signal_hook::consts::SIGTERM {
                    Signal::SIGTERM
                } else {
                    Signal::SIGINT
                };
                let _ = kill(Pid::from_raw(pid), signal);
            }
        }
    });
}
