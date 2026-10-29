//! Records how long the slow parts of applying a fix actually take.
//!
//! Applying a suggestion is a handful of calls into the app being corrected, each answered on the
//! thread that app uses for typing. Which of them costs the time depends on the app, so guessing
//! from the outside has been wrong before. Each step writes a line to a file instead, so a real
//! session in a real editor can be read back afterwards.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions, metadata};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The size at which the log starts again, so leaving it on costs a bounded amount of disk.
const MAX_LOG_BYTES: u64 = 1_000_000;

/// Where the timings are written.
pub fn log_path() -> Option<PathBuf> {
    dirs::config_dir().map(|path| path.join("harper-desktop").join("timings.log"))
}

/// Times a step for as long as the returned value is held.
///
/// `detail` carries whatever makes the number make sense — how many edits were in the batch, how
/// many characters were typed — and is left out when there is nothing to say.
#[must_use = "the step is timed until this is dropped"]
pub fn step(label: &'static str, detail: Option<String>) -> Step {
    Step {
        label,
        detail,
        start: Instant::now(),
    }
}

pub struct Step {
    label: &'static str,
    detail: Option<String>,
    start: Instant,
}

impl Drop for Step {
    fn drop(&mut self) {
        record(self.label, self.detail.as_deref(), self.start.elapsed());
    }
}

/// Appends one measurement to the log, and gives up quietly if it cannot.
///
/// A fix that works is worth more than the note about how long it took, so nothing here is allowed
/// to turn a failed write into a failed edit.
fn record(label: &str, detail: Option<&str>, elapsed: Duration) {
    let Some(path) = log_path() else {
        return;
    };

    if metadata(&path).is_ok_and(|metadata| metadata.len() > MAX_LOG_BYTES) {
        let _ = File::create(&path);
    }

    let Some(parent) = path.parent() else {
        return;
    };
    let _ = std::fs::create_dir_all(parent);

    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };

    let mut line = String::new();
    let _ = write!(
        line,
        "{} {label} {:.1}ms",
        seconds_since_epoch(),
        elapsed.as_secs_f64() * 1000.0
    );

    if let Some(detail) = detail {
        let _ = write!(line, " ({detail})");
    }

    let _ = writeln!(file, "{line}");
}

fn seconds_since_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{log_path, step};

    /// Proves the line actually reaches the file.
    ///
    /// Every write here is allowed to fail quietly so a bad log cannot break an edit, which means
    /// a broken log would otherwise look exactly like a quiet one.
    #[test]
    fn a_step_reaches_the_log() {
        let path = log_path().expect("no config directory to log into");
        let before = std::fs::read_to_string(&path).unwrap_or_default();

        drop(step("test-step", Some("from the test suite".to_string())));

        let after = std::fs::read_to_string(&path).expect("the log was not written");

        assert!(after.len() > before.len());
        assert!(after.contains("test-step"));
        assert!(after.contains("from the test suite"));
    }
}
