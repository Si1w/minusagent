use std::sync::{Mutex, OnceLock};

use log::{Level, LevelFilter, Log, Metadata, Record};

static LOG_BUFFER: OnceLock<Mutex<Vec<LogEntry>>> = OnceLock::new();
static LOGGER: TuiLogger = TuiLogger;

/// A structured log entry
pub struct LogEntry {
    pub level: Level,
    pub message: String,
}

impl std::fmt::Display for LogEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tag = match self.level {
            Level::Error => "[ERROR]",
            Level::Warn => "[WARN]",
            Level::Info => "[INFO]",
            Level::Debug => "[DEBUG]",
            Level::Trace => "[TRACE]",
        };
        write!(f, "{tag} {}", self.message)
    }
}

/// Logger that buffers messages for TUI display
pub struct TuiLogger;

impl TuiLogger {
    /// Initialize the global logger
    ///
    /// Log level is controlled by `RUST_LOG` env var (default: `info`).
    /// Examples: `RUST_LOG=debug cargo run`, `RUST_LOG=error cargo run`
    pub fn init() {
        LOG_BUFFER.get_or_init(|| Mutex::new(Vec::new()));
        let level = std::env::var("RUST_LOG")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(LevelFilter::Info);
        log::set_logger(&LOGGER).ok();
        log::set_max_level(level);
    }

    /// Drain all buffered log entries
    pub fn drain() -> Vec<LogEntry> {
        if let Some(buf) = LOG_BUFFER.get() {
            std::mem::take(&mut *buf.lock().unwrap())
        } else {
            Vec::new()
        }
    }
}

impl Log for TuiLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::Level::Debug
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            if let Some(buf) = LOG_BUFFER.get() {
                if let Ok(mut buf) = buf.lock() {
                    buf.push(LogEntry {
                        level: record.level(),
                        message: record.args().to_string(),
                    });
                }
            }
        }
    }

    fn flush(&self) {}
}
