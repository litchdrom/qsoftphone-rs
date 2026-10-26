//! Tracing setup: a daily-rotating log file plus stderr.
//!
//! The C++ app writes a plain text log through its own `Logger`. This
//! is the Rust equivalent, built on `tracing` so that every crate in
//! the workspace can emit structured events into the same sink.

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::error::{Error, Result};
use crate::paths::AppPaths;

/// Keeps the non-blocking writer's worker thread alive.
///
/// Dropping this flushes and stops the writer, so it has to live for
/// as long as the process wants logs. Hold it in `main`.
pub struct LogGuard {
    _worker: WorkerGuard,
}

/// Install the global subscriber.
///
/// `default_level` is used when `RUST_LOG` is unset. The file sink
/// always records everything the filter allows; stderr honours the
/// same filter, so `RUST_LOG=debug` turns both up.
pub fn init(paths: &AppPaths, default_level: &str) -> Result<LogGuard> {
    std::fs::create_dir_all(paths.log_dir())?;

    // `daily` produces `qsoftphone.log.2026-10-26`, so one file per
    // day and no unbounded growth inside a single file.
    let file_appender = tracing_appender::rolling::daily(paths.log_dir(), "qsoftphone.log");
    let (file_writer, worker) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::try_new(default_level).unwrap_or_else(|_| EnvFilter::new("info"))
    });

    let file_layer = fmt::layer()
        .with_writer(file_writer)
        .with_ansi(false)
        .with_target(true);

    let stderr_layer = fmt::layer().with_writer(std::io::stderr);

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init()
        .map_err(|e| Error::Message(format!("tracing subscriber init failed: {e}")))?;

    Ok(LogGuard { _worker: worker })
}
