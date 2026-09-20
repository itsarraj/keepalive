//! Opens the capture log file and writes the `[keepalive ...]` banner
//! lines that bracket each child run in it, alongside the child's own
//! raw stdout/stderr.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Local;

/// The exact `chrono` format string every banner timestamp uses — shared
/// so tests (and anything else that wants to parse the log back out) use
/// the identical format rather than a copy that could drift.
pub const BANNER_TIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3f%z";

/// Opens (creating if needed) `path` in append mode, so restarting
/// `keepalive` itself never clobbers a log from a previous run.
pub fn open_append(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening log file {}", path.display()))
}

/// Formats a `[keepalive <timestamp>] <message>` banner line. Split out
/// from the writing so the exact text is unit-testable without a real
/// file.
pub fn format_banner(message: &str) -> String {
    format!(
        "[keepalive {}] {message}\n",
        Local::now().format(BANNER_TIME_FORMAT)
    )
}

/// Writes and flushes one banner line to `log`.
pub fn write_banner(log: &mut File, message: &str) -> Result<()> {
    log.write_all(format_banner(message).as_bytes())
        .context("writing banner to log file")?;
    log.flush().context("flushing log file")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn scratch_path(name: &str) -> std::path::PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "keepalive-logfile-test-{}-{name}-{unique}.log",
            std::process::id()
        ))
    }

    #[test]
    fn format_banner_wraps_message_with_prefix_and_newline() {
        let line = format_banner("hello");
        assert!(line.starts_with("[keepalive "));
        assert!(line.contains("] hello"));
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn open_append_creates_a_new_file() {
        let path = scratch_path("create");
        let mut log = open_append(&path).unwrap();
        write_banner(&mut log, "created").unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("created"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn open_append_does_not_truncate_existing_content() {
        let path = scratch_path("append");
        std::fs::write(&path, "line one already here\n").unwrap();

        let mut log = open_append(&path).unwrap();
        write_banner(&mut log, "line two").unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with("line one already here\n"));
        assert!(contents.contains("line two"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn multiple_banners_all_land_in_order() {
        let path = scratch_path("order");
        let mut log = open_append(&path).unwrap();
        write_banner(&mut log, "first").unwrap();
        write_banner(&mut log, "second").unwrap();
        write_banner(&mut log, "third").unwrap();

        let mut contents = String::new();
        File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        let first_pos = contents.find("first").unwrap();
        let second_pos = contents.find("second").unwrap();
        let third_pos = contents.find("third").unwrap();
        assert!(first_pos < second_pos && second_pos < third_pos);
        std::fs::remove_file(&path).ok();
    }
}
