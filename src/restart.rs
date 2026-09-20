//! Decides what to do after a supervised child exits: stop, give up, or
//! restart after some backoff delay. Pure decision logic — no process
//! spawning, no sleeping, no clock reads — so every branch is directly
//! unit-testable.

use std::time::Duration;

use crate::backoff::next_delay;

/// Whether a clean (exit code 0) run should end supervision, or whether
/// `keepalive` should restart the child no matter how it exited —
/// `supervisord`'s `autorestart=true` vs. its default "unexpected exit
/// only" behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    OnFailure,
    Always,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub policy: RestartPolicy,
    pub max_restarts: Option<u32>,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    pub backoff_multiplier: f64,
    /// A run that stayed up at least this long counts as "stable" and
    /// resets the backoff delay back to `backoff_base` for the next
    /// crash, so a process that's been healthy for hours doesn't inherit
    /// a huge delay from a crash loop it had days ago.
    pub reset_after: Duration,
}

#[derive(Debug, Default)]
pub struct RestartState {
    /// Resets to 0 whenever a run's uptime exceeds `reset_after`; drives
    /// the exponential backoff calculation.
    consecutive_failures: u32,
    /// Never resets — the total number of restarts performed across the
    /// whole life of this `keepalive` process, checked against
    /// `max_restarts`.
    total_restarts: u32,
}

impl RestartState {
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    pub fn total_restarts(&self) -> u32 {
        self.total_restarts
    }
}

#[derive(Debug, PartialEq)]
pub enum Decision {
    /// The child exited 0 under `RestartPolicy::OnFailure` — supervision
    /// ends successfully.
    Stop,
    /// `max_restarts` has been reached — supervision ends with failure.
    GiveUp,
    /// Restart the child after this delay.
    Restart(Duration),
}

/// Decides what happens next after a child exited, given whether it
/// exited successfully (code 0) and how long it had been running.
pub fn decide(
    state: &mut RestartState,
    exit_success: bool,
    uptime: Duration,
    config: &Config,
) -> Decision {
    if exit_success && config.policy == RestartPolicy::OnFailure {
        return Decision::Stop;
    }

    state.total_restarts += 1;
    if let Some(max) = config.max_restarts {
        if state.total_restarts > max {
            return Decision::GiveUp;
        }
    }

    if uptime >= config.reset_after {
        state.consecutive_failures = 0;
    }
    state.consecutive_failures += 1;

    let delay = next_delay(
        state.consecutive_failures,
        config.backoff_base,
        config.backoff_max,
        config.backoff_multiplier,
    );
    Decision::Restart(delay)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(policy: RestartPolicy, max_restarts: Option<u32>) -> Config {
        Config {
            policy,
            max_restarts,
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            backoff_multiplier: 2.0,
            reset_after: Duration::from_secs(60),
        }
    }

    #[test]
    fn on_failure_stops_after_clean_exit() {
        let mut state = RestartState::default();
        let c = config(RestartPolicy::OnFailure, None);
        let d = decide(&mut state, true, Duration::from_secs(5), &c);
        assert_eq!(d, Decision::Stop);
    }

    #[test]
    fn on_failure_restarts_after_nonzero_exit() {
        let mut state = RestartState::default();
        let c = config(RestartPolicy::OnFailure, None);
        let d = decide(&mut state, false, Duration::from_secs(5), &c);
        assert_eq!(d, Decision::Restart(Duration::from_secs(1)));
    }

    #[test]
    fn always_restarts_even_after_clean_exit() {
        let mut state = RestartState::default();
        let c = config(RestartPolicy::Always, None);
        let d = decide(&mut state, true, Duration::from_secs(5), &c);
        assert_eq!(d, Decision::Restart(Duration::from_secs(1)));
    }

    #[test]
    fn backoff_grows_across_consecutive_short_lived_failures() {
        let mut state = RestartState::default();
        let c = config(RestartPolicy::OnFailure, None);
        let short = Duration::from_millis(10); // well under reset_after

        let d1 = decide(&mut state, false, short, &c);
        let d2 = decide(&mut state, false, short, &c);
        let d3 = decide(&mut state, false, short, &c);

        assert_eq!(d1, Decision::Restart(Duration::from_secs(1)));
        assert_eq!(d2, Decision::Restart(Duration::from_secs(2)));
        assert_eq!(d3, Decision::Restart(Duration::from_secs(4)));
    }

    #[test]
    fn a_stable_run_resets_backoff_to_base() {
        let mut state = RestartState::default();
        let c = config(RestartPolicy::OnFailure, None);
        let short = Duration::from_millis(10);

        decide(&mut state, false, short, &c); // consecutive_failures = 1
        decide(&mut state, false, short, &c); // consecutive_failures = 2

        // This run stayed up past reset_after before eventually crashing.
        let long = Duration::from_secs(120);
        let d = decide(&mut state, false, long, &c);

        assert_eq!(d, Decision::Restart(Duration::from_secs(1)));
        assert_eq!(state.consecutive_failures(), 1);
    }

    #[test]
    fn max_restarts_gives_up_once_exceeded() {
        let mut state = RestartState::default();
        let c = config(RestartPolicy::OnFailure, Some(2));
        let short = Duration::from_millis(10);

        assert!(matches!(
            decide(&mut state, false, short, &c),
            Decision::Restart(_)
        ));
        assert!(matches!(
            decide(&mut state, false, short, &c),
            Decision::Restart(_)
        ));
        assert_eq!(decide(&mut state, false, short, &c), Decision::GiveUp);
    }

    #[test]
    fn total_restarts_counts_every_attempt_even_across_resets() {
        let mut state = RestartState::default();
        let c = config(RestartPolicy::OnFailure, None);
        decide(&mut state, false, Duration::from_secs(120), &c);
        decide(&mut state, false, Duration::from_secs(120), &c);
        assert_eq!(state.total_restarts(), 2);
        // Both runs were "stable" (past reset_after), so backoff stayed at base each time.
        assert_eq!(state.consecutive_failures(), 1);
    }

    #[test]
    fn max_restarts_zero_gives_up_on_first_failure() {
        let mut state = RestartState::default();
        let c = config(RestartPolicy::OnFailure, Some(0));
        let d = decide(&mut state, false, Duration::from_millis(10), &c);
        assert_eq!(d, Decision::GiveUp);
    }
}
