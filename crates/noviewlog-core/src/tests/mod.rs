use crate::core::types::{LaunchConfig, LogLevel, LogRecord};
use chrono::Utc;
use std::sync::Mutex;

/// User config.yaml is process-global; serialize tests that write it.
pub(crate) static USER_CONFIG_LOCK: Mutex<()> = Mutex::new(());

/// Redirect the user config directory to a fresh temp dir for the duration of
/// a test, so tests that opt into real persistence never touch the
/// developer's `~/.config/noviewlog` (issue #239). The override is cleared and
/// the directory removed on drop, including on a failed assertion.
pub(crate) struct ConfigDirGuard {
    dir: std::path::PathBuf,
}

impl ConfigDirGuard {
    pub(crate) fn new(name: &str) -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("noviewlog-test-cfg-{name}-{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();
        crate::core::config::set_config_dir_override(Some(dir.clone()));
        Self { dir }
    }
}

impl Drop for ConfigDirGuard {
    fn drop(&mut self) {
        crate::core::config::set_config_dir_override(None);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Generate (once, cached) a >8 MiB perf fixture log so the large-file tests
/// run on any host instead of silently skipping without `/home/tester/big.log`
/// (issue #72). ~55 B/line; every ~20th line carries the `11:01:13` needle
/// used by the match-index perf test.
pub(crate) fn big_log_fixture() -> std::path::PathBuf {
    use std::io::Write;
    const TARGET_BYTES: u64 = 12 * 1024 * 1024;
    const NEEDLE: &str = "11:01:13";

    // PID-suffixed name: parallel test binaries never race on the same file
    // (issue #113). Old files are cleaned opportunistically when oversized.
    let path = std::env::temp_dir().join(format!("noviewlog-perf-big-{}.log", std::process::id()));
    let ok = std::fs::metadata(&path)
        .map(|m| m.len() >= TARGET_BYTES)
        .unwrap_or(false);
    if ok {
        return path;
    }
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&path).expect("create fixture"));
        let mut i: u64 = 0;
        let mut bytes: u64 = 0;
        while bytes < TARGET_BYTES {
            let time = if i.is_multiple_of(20) {
                NEEDLE
            } else {
                "10:00:00"
            };
            let line = format!("2026-09-23T{time}.000Z info: perf line {i:09} payload-abcdef\n");
            bytes += line.len() as u64;
            f.write_all(line.as_bytes()).expect("write fixture");
            i += 1;
        }
        f.flush().expect("flush fixture");
    }
    path
}

/// Long-running process for PTY spawn tests (platform-specific).
pub(crate) fn long_running_launch_config() -> LaunchConfig {
    #[cfg(unix)]
    {
        LaunchConfig {
            command: Some("sleep".into()),
            args: vec!["30".into()],
            ..LaunchConfig::default()
        }
    }
    #[cfg(windows)]
    {
        LaunchConfig {
            command: Some("ping".into()),
            args: vec!["-n".into(), "31".into(), "127.0.0.1".into()],
            ..LaunchConfig::default()
        }
    }
}

pub(crate) fn sample_records() -> Vec<LogRecord> {
    vec![
        LogRecord {
            id: 1,
            lines: vec!["warn: deprecated".to_string()],
            text: "warn: deprecated".to_string(),
            received_at: Utc::now(),
            level: Some(LogLevel::Warn),
            overwrite: false,
        },
        LogRecord {
            id: 2,
            lines: vec!["Error: boom".to_string()],
            text: "Error: boom".to_string(),
            received_at: Utc::now(),
            level: Some(LogLevel::Error),
            overwrite: false,
        },
        LogRecord {
            id: 3,
            lines: vec!["info: ok".to_string()],
            text: "info: ok".to_string(),
            received_at: Utc::now(),
            level: Some(LogLevel::Info),
            overwrite: false,
        },
    ]
}

#[cfg(windows)]
mod conpty_windows;
mod parser_filters;
mod presets_defaults;
mod projects;
mod pty_flood;
mod spawn_async;
mod tabs_search;
mod terminal_caret;
mod terminals_files;
mod viewport_wrap;
mod volatile_patch;
