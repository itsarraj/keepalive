//! Pure exponential-backoff math: no I/O, no clock reads — just
//! `attempt` in, `Duration` out, so it's trivially unit-testable.

use std::time::Duration;

/// Computes the delay before restart attempt number `attempt` (1-indexed:
/// the *first* restart after a crash is attempt `1`), as
/// `base * multiplier^(attempt - 1)`, capped at `max`.
pub fn next_delay(attempt: u32, base: Duration, max: Duration, multiplier: f64) -> Duration {
    if attempt == 0 {
        return Duration::ZERO;
    }
    let exponent = (attempt - 1) as i32;
    let factor = multiplier.powi(exponent);
    let secs = base.as_secs_f64() * factor;
    let capped = secs.min(max.as_secs_f64());
    Duration::from_secs_f64(capped.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_attempt_is_exactly_base() {
        let d = next_delay(1, Duration::from_secs(1), Duration::from_secs(60), 2.0);
        assert_eq!(d, Duration::from_secs(1));
    }

    #[test]
    fn second_attempt_doubles_with_multiplier_two() {
        let d = next_delay(2, Duration::from_secs(1), Duration::from_secs(60), 2.0);
        assert_eq!(d, Duration::from_secs(2));
    }

    #[test]
    fn third_attempt_quadruples() {
        let d = next_delay(3, Duration::from_secs(1), Duration::from_secs(60), 2.0);
        assert_eq!(d, Duration::from_secs(4));
    }

    #[test]
    fn delay_is_capped_at_max() {
        let d = next_delay(10, Duration::from_secs(1), Duration::from_secs(30), 2.0);
        assert_eq!(d, Duration::from_secs(30));
    }

    #[test]
    fn attempt_zero_is_zero_delay() {
        assert_eq!(
            next_delay(0, Duration::from_secs(5), Duration::from_secs(60), 2.0),
            Duration::ZERO
        );
    }

    #[test]
    fn multiplier_one_never_grows() {
        let d1 = next_delay(1, Duration::from_millis(200), Duration::from_secs(60), 1.0);
        let d5 = next_delay(5, Duration::from_millis(200), Duration::from_secs(60), 1.0);
        assert_eq!(d1, d5);
    }

    #[test]
    fn fractional_base_and_multiplier_work() {
        let d = next_delay(2, Duration::from_millis(100), Duration::from_secs(10), 1.5);
        assert_eq!(d, Duration::from_millis(150));
    }
}
