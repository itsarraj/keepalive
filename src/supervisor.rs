//! The actual run loop: spawn the command via `sh -c`, wait for it,
//! decide what to do next, sleep (interruptibly) for the backoff delay,
//! repeat. This is the one module that genuinely touches the OS — real
//! `Command::spawn`, real `wait()`, real file descriptors for the log —
//! so its tests spawn real (tiny, fast) child processes rather than
//! mocking any of that.

use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::logfile::{open_append, write_banner};
use crate::restart::{decide, Config, Decision, RestartState};

/// Runs `command` (handed to `sh -c` wholesale, so pipes/`&&`/env
/// expansion all work) under supervision until it either exits cleanly
/// under `RestartPolicy::OnFailure`, hits `max_restarts`, or a shutdown
/// is requested via `shutdown`. Returns the process exit code
/// `keepalive` itself should use.
///
/// `current_pid` is kept up to date with the live child's PID (`0` when
/// none is running) so a caller — typically a signal handler thread —
/// can forward a real signal to whichever child is currently alive.
pub fn run(
    command: &str,
    log_path: &Path,
    config: &Config,
    shutdown: Arc<AtomicBool>,
    current_pid: Arc<AtomicI32>,
) -> Result<i32> {
    let mut log = open_append(log_path)?;
    let mut state = RestartState::default();
    let mut attempt: u64 = 0;

    loop {
        if shutdown.load(Ordering::SeqCst) {
            write_banner(&mut log, "shutdown requested before starting — exiting")?;
            return Ok(0);
        }

        attempt += 1;
        write_banner(
            &mut log,
            &format!("starting '{command}' (attempt {attempt})"),
        )?;

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdout(Stdio::from(
                log.try_clone().context("cloning log fd for stdout")?,
            ))
            .stderr(Stdio::from(
                log.try_clone().context("cloning log fd for stderr")?,
            ))
            .spawn()
            .with_context(|| format!("spawning '{command}'"))?;

        let pid = child.id();
        current_pid.store(pid as i32, Ordering::SeqCst);
        let start = Instant::now();
        let status = child.wait().context("waiting for child")?;
        current_pid.store(0, Ordering::SeqCst);
        let uptime = start.elapsed();

        write_banner(
            &mut log,
            &format!(
                "'{command}' (pid {pid}) exited {} after {:.3}s",
                describe_status(&status),
                uptime.as_secs_f64()
            ),
        )?;

        if shutdown.load(Ordering::SeqCst) {
            write_banner(&mut log, "shutdown requested — not restarting")?;
            return Ok(status.code().unwrap_or(1));
        }

        match decide(&mut state, status.success(), uptime, config) {
            Decision::Stop => {
                write_banner(&mut log, "exited cleanly — stopping supervision")?;
                return Ok(0);
            }
            Decision::GiveUp => {
                // `state.total_restarts()` is already one past the
                // configured limit here — it's incremented for the
                // failure that triggered giving up, which itself never
                // actually got restarted. `config.max_restarts` (the
                // limit that was reached) is the number to report, not
                // the internal counter.
                write_banner(
                    &mut log,
                    &format!(
                        "giving up after {} restart(s) (--max-restarts reached)",
                        config.max_restarts.unwrap_or(state.total_restarts())
                    ),
                )?;
                return Ok(1);
            }
            Decision::Restart(delay) => {
                write_banner(
                    &mut log,
                    &format!(
                        "restarting in {:.3}s (consecutive failures: {})",
                        delay.as_secs_f64(),
                        state.consecutive_failures()
                    ),
                )?;
                sleep_interruptible(delay, &shutdown);
            }
        }
    }
}

fn describe_status(status: &ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("with code {code}"),
        None => "via signal".to_string(),
    }
}

/// Sleeps for `delay`, but wakes up early (in <= 50ms) if `shutdown`
/// flips true mid-wait, so Ctrl-C during a long backoff delay doesn't
/// have to wait out the whole thing.
fn sleep_interruptible(delay: Duration, shutdown: &AtomicBool) {
    let step = Duration::from_millis(50);
    let mut remaining = delay;
    while remaining > Duration::ZERO {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        let chunk = remaining.min(step);
        thread::sleep(chunk);
        remaining = remaining.saturating_sub(chunk);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logfile::BANNER_TIME_FORMAT;
    use crate::restart::RestartPolicy;
    use chrono::DateTime;

    fn scratch_paths(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::time::{SystemTime, UNIX_EPOCH};
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "keepalive-supervisor-test-{}-{name}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (dir.join("out.log"), dir.join("counter"))
    }

    fn fast_config(policy: RestartPolicy, max_restarts: Option<u32>) -> Config {
        Config {
            policy,
            max_restarts,
            backoff_base: Duration::from_millis(10),
            backoff_max: Duration::from_millis(200),
            backoff_multiplier: 2.0,
            reset_after: Duration::from_secs(30),
        }
    }

    fn fresh_flags() -> (Arc<AtomicBool>, Arc<AtomicI32>) {
        (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicI32::new(0)),
        )
    }

    #[test]
    fn a_real_process_that_exits_nonzero_is_actually_restarted() {
        let (log_path, counter_path) = scratch_paths("restart-count");
        let command = format!("printf x >> '{}'; exit 1", counter_path.display());
        let config = fast_config(RestartPolicy::OnFailure, Some(3));
        let (shutdown, pid) = fresh_flags();

        let code = run(&command, &log_path, &config, shutdown, pid).unwrap();

        // 1 initial run + 3 restarts = 4 real spawned processes, each
        // appending one real byte to a real counter file on disk.
        let counter = std::fs::read_to_string(&counter_path).unwrap();
        assert_eq!(
            counter, "xxxx",
            "expected 4 real runs, counter was {counter:?}"
        );
        assert_eq!(code, 1, "should exit nonzero after giving up");

        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(log.contains("giving up after 3 restart"));
        std::fs::remove_dir_all(log_path.parent().unwrap()).ok();
    }

    #[test]
    fn a_real_process_that_exits_zero_is_not_restarted_under_on_failure() {
        let (log_path, counter_path) = scratch_paths("clean-exit");
        let command = format!("printf x >> '{}'; exit 0", counter_path.display());
        let config = fast_config(RestartPolicy::OnFailure, None);
        let (shutdown, pid) = fresh_flags();

        let code = run(&command, &log_path, &config, shutdown, pid).unwrap();

        let counter = std::fs::read_to_string(&counter_path).unwrap();
        assert_eq!(counter, "x", "a clean exit should run exactly once");
        assert_eq!(code, 0);
        std::fs::remove_dir_all(log_path.parent().unwrap()).ok();
    }

    #[test]
    fn always_policy_restarts_even_a_cleanly_exiting_process() {
        let (log_path, counter_path) = scratch_paths("always-policy");
        let command = format!("printf x >> '{}'; exit 0", counter_path.display());
        let config = fast_config(RestartPolicy::Always, Some(2));
        let (shutdown, pid) = fresh_flags();

        run(&command, &log_path, &config, shutdown, pid).unwrap();

        let counter = std::fs::read_to_string(&counter_path).unwrap();
        assert_eq!(
            counter, "xxx",
            "1 initial + 2 restarts under --restart always"
        );
        std::fs::remove_dir_all(log_path.parent().unwrap()).ok();
    }

    #[test]
    fn stdout_and_stderr_both_genuinely_land_in_the_log_file() {
        let (log_path, _counter) = scratch_paths("capture");
        let command = "echo from-stdout; echo from-stderr 1>&2; exit 1".to_string();
        let config = fast_config(RestartPolicy::OnFailure, Some(0));
        let (shutdown, pid) = fresh_flags();

        run(&command, &log_path, &config, shutdown, pid).unwrap();

        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(log.contains("from-stdout"), "log was:\n{log}");
        assert!(log.contains("from-stderr"), "log was:\n{log}");
        std::fs::remove_dir_all(log_path.parent().unwrap()).ok();
    }

    #[test]
    fn real_backoff_delay_between_attempts_actually_grows() {
        let (log_path, _counter) = scratch_paths("backoff-grows");
        let command = "exit 1".to_string();
        let config = Config {
            policy: RestartPolicy::OnFailure,
            max_restarts: Some(3),
            backoff_base: Duration::from_millis(80),
            backoff_max: Duration::from_secs(5),
            backoff_multiplier: 3.0,
            reset_after: Duration::from_secs(30),
        };
        let (shutdown, pid) = fresh_flags();

        run(&command, &log_path, &config, shutdown, pid).unwrap();

        let log = std::fs::read_to_string(&log_path).unwrap();
        let starts: Vec<DateTime<chrono::FixedOffset>> = log
            .lines()
            // "starting '" (with the trailing quote from the real spawn
            // banner's format string) rather than a bare "starting" —
            // the latter also matches every "restarting in ...s" backoff
            // banner, since "restarting" literally contains "starting"
            // as a substring. That double-counted every backoff decision
            // as if it were a second spawn (7 matches instead of the
            // real 4) until caught by actually running this test.
            .filter(|l| l.contains("starting '"))
            .filter_map(|l| {
                let rest = l.strip_prefix("[keepalive ")?;
                let ts = rest.split(']').next()?;
                DateTime::parse_from_str(ts, BANNER_TIME_FORMAT).ok()
            })
            .collect();

        assert_eq!(
            starts.len(),
            4,
            "expected 4 real spawned attempts, got {}",
            starts.len()
        );

        let gap = |a: usize, b: usize| (starts[b] - starts[a]).num_milliseconds();
        let gap1 = gap(0, 1); // after attempt 1 fails: ~80ms backoff
        let gap2 = gap(1, 2); // after attempt 2 fails: ~240ms backoff
        let gap3 = gap(2, 3); // after attempt 3 fails: ~720ms backoff

        assert!(gap1 >= 60, "first gap too short for real backoff: {gap1}ms");
        assert!(
            gap2 > gap1,
            "second real gap ({gap2}ms) should exceed the first ({gap1}ms)"
        );
        assert!(
            gap3 > gap2,
            "third real gap ({gap3}ms) should exceed the second ({gap2}ms)"
        );
        std::fs::remove_dir_all(log_path.parent().unwrap()).ok();
    }

    #[test]
    fn shutdown_flag_set_mid_run_stops_supervision_without_restart() {
        let (log_path, counter_path) = scratch_paths("shutdown");
        // The child sleeps briefly so the main thread has time to flip
        // the shutdown flag while it's still genuinely running.
        let command = format!(
            "printf x >> '{}'; sleep 0.2; exit 1",
            counter_path.display()
        );
        let config = fast_config(RestartPolicy::OnFailure, None);
        let (shutdown, pid) = fresh_flags();

        let shutdown_clone = Arc::clone(&shutdown);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            shutdown_clone.store(true, Ordering::SeqCst);
        });

        run(&command, &log_path, &config, shutdown, pid).unwrap();

        // Give a moment for any (incorrect) restart to have happened.
        thread::sleep(Duration::from_millis(150));
        let counter = std::fs::read_to_string(&counter_path).unwrap();
        assert_eq!(counter, "x", "shutdown mid-run should prevent any restart");
        std::fs::remove_dir_all(log_path.parent().unwrap()).ok();
    }

    #[test]
    fn current_pid_reflects_the_real_live_child_pid() {
        let (log_path, _counter) = scratch_paths("pid-tracking");
        let command = "sleep 0.15; exit 1".to_string();
        let config = fast_config(RestartPolicy::OnFailure, Some(0));
        let (shutdown, pid) = fresh_flags();
        let pid_clone = Arc::clone(&pid);

        let observed = Arc::new(AtomicI32::new(-1));
        let observed_clone = Arc::clone(&observed);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            observed_clone.store(pid_clone.load(Ordering::SeqCst), Ordering::SeqCst);
        });

        run(&command, &log_path, &config, shutdown, Arc::clone(&pid)).unwrap();

        assert!(
            observed.load(Ordering::SeqCst) > 0,
            "current_pid should have held a real positive PID while the child ran"
        );
        assert_eq!(
            pid.load(Ordering::SeqCst),
            0,
            "current_pid should be reset to 0 once the child has exited"
        );
        std::fs::remove_dir_all(log_path.parent().unwrap()).ok();
    }

    #[test]
    fn missing_shell_command_is_a_clean_error_not_a_panic() {
        // A command sh itself can't even parse as a syntax error still
        // spawns `sh` successfully (sh reports the error and exits
        // nonzero) — this checks the *supervisor* only errors when
        // spawning genuinely fails, e.g. an unwritable log path.
        let unwritable = std::path::PathBuf::from("/nonexistent-dir-xyz/out.log");
        let config = fast_config(RestartPolicy::OnFailure, Some(0));
        let (shutdown, pid) = fresh_flags();
        let result = run("exit 0", &unwritable, &config, shutdown, pid);
        assert!(result.is_err());
    }
}
