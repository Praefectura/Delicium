//! Launcher logging: one file per day under `logs\`, mirrored to stderr.
//!
//! The launcher has no console in release builds, so the file is the only
//! record of why an update failed or why the engine was restarted. Each day
//! starts a new file and the previous one is rotated, keeping four bounded
//! files instead of one that grows without limit.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::paths;

/// Rotate a log file once it grows past this size.
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
/// How many rotated copies of one day's file are kept.
const ROTATIONS: usize = 3;

/// Installs the logger. Everything logged afterwards goes to stderr and to
/// `logs\launcher-YYYYMMDD.log`.
pub fn init(root: &Path, default_level: &str) -> Result<(), String> {
    let path = today_path(root);
    rotate(&path)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| format!("Cannot open {}: {error}", path.display()))?;
    let tee = Tee { file };
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(default_level.to_owned()),
    )
    .target(env_logger::Target::Pipe(Box::new(tee)))
    .init();
    Ok(())
}

/// `logs\launcher-YYYYMMDD.log` for today.
pub fn today_path(root: &Path) -> PathBuf {
    let (year, month, day) = civil_date(super::unix_now());
    paths::logs_dir(root).join(format!("launcher-{year:04}{month:02}{day:02}.log"))
}

/// The most recent log file, for the "Open logs" button; the directory when
/// no log exists yet.
pub fn recent_log(root: &Path) -> PathBuf {
    let path = today_path(root);
    if path.is_file() {
        path
    } else {
        paths::logs_dir(root)
    }
}

/// Renames the file and its rotations one step down when the current file is
/// too large. Runs before the file is opened for appending.
fn rotate(path: &Path) -> Result<(), String> {
    let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    if size <= MAX_LOG_BYTES {
        return Ok(());
    }
    let shifted = path.with_extension("log.1");
    for index in (1..ROTATIONS).rev() {
        let from = path.with_extension(format!("log.{index}"));
        let to = path.with_extension(format!("log.{}", index + 1));
        if from.exists() {
            let _ = std::fs::rename(&from, &to);
        }
    }
    if let Err(error) = std::fs::rename(path, &shifted) {
        // A failed rotation only means the file keeps growing; it must not
        // stop the launcher from logging at all.
        eprintln!("Cannot rotate {}: {error}", path.display());
    }
    Ok(())
}

/// Writes every line to both stderr and the file.
struct Tee {
    file: std::fs::File,
}

impl Write for Tee {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(bytes);
        self.file.write_all(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = std::io::stderr().flush();
        self.file.flush()
    }
}

/// `(year, month, day)` in UTC for the given Unix seconds.
pub fn civil_date(unix: u64) -> (i64, u32, u32) {
    let days = (unix / 86_400) as i64;
    civil_from_days(days)
}

/// Howard Hinnant's days-from-civil inverse: a Gregorian date for a number of
/// days since 1970-01-01. Valid for any date the clock can produce.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_match_known_epochs() {
        assert_eq!(civil_date(0), (1970, 1, 1));
        assert_eq!(civil_date(86_399), (1970, 1, 1));
        assert_eq!(civil_date(86_400), (1970, 1, 2));
        // 2000-02-29, a leap day in a century year.
        assert_eq!(civil_date(951_782_400), (2000, 2, 29));
        // 2026-10-18.
        assert_eq!(civil_date(1_792_281_600), (2026, 10, 18));
    }
}
