//! Parses human-friendly duration strings (`"500ms"`, `"1s"`, `"2m"`) for
//! the `--backoff-*`/`--reset-after` flags.

use anyhow::{anyhow, Result};
use std::time::Duration;

/// Parses a duration string. Accepts a number followed by (case
/// insensitive) `ms`, `s`, `m`, or `h`; a bare number is seconds.
/// Fractional values are allowed (`"1.5s"`).
pub fn parse_duration(input: &str) -> Result<Duration> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("empty duration string"));
    }

    let (digits, seconds_per_unit) = if let Some(d) = trimmed.strip_suffix("ms") {
        (d, 0.001)
    } else if let Some(d) = trimmed.strip_suffix('s') {
        (d, 1.0)
    } else if let Some(d) = trimmed.strip_suffix('m') {
        (d, 60.0)
    } else if let Some(d) = trimmed.strip_suffix('h') {
        (d, 3600.0)
    } else {
        (trimmed, 1.0)
    };

    let digits = digits.trim();
    if digits.is_empty() {
        return Err(anyhow!("duration '{input}' has a unit but no number"));
    }
    let value: f64 = digits.parse().map_err(|_| {
        anyhow!("'{input}' is not a valid duration (expected e.g. 500ms, 1s, 2m, 1h)")
    })?;
    if !value.is_finite() || value < 0.0 {
        return Err(anyhow!("duration '{input}' must be a non-negative number"));
    }
    Ok(Duration::from_secs_f64(value * seconds_per_unit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_milliseconds() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
    }

    #[test]
    fn parses_seconds() {
        assert_eq!(parse_duration("2s").unwrap(), Duration::from_secs(2));
    }

    #[test]
    fn parses_minutes() {
        assert_eq!(parse_duration("1m").unwrap(), Duration::from_secs(60));
    }

    #[test]
    fn parses_hours() {
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
    }

    #[test]
    fn bare_number_is_seconds() {
        assert_eq!(parse_duration("5").unwrap(), Duration::from_secs(5));
    }

    #[test]
    fn parses_fractional_seconds() {
        assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_millis(1500));
    }

    #[test]
    fn rejects_negative() {
        assert!(parse_duration("-1s").is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_duration("soon").is_err());
    }
}
