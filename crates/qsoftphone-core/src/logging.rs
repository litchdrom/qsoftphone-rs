//! Tracing setup: a daily-rotating log file plus stderr.
//!
//! The C++ app writes a plain text log through its own `Logger`. This
//! is the Rust equivalent, built on `tracing` so that every crate in
//! the workspace can emit structured events into the same sink.

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::error::{Error, Result};
use crate::model::account::Account;
use crate::paths::AppPaths;

/// The span every account-scoped work runs in.
///
/// §6 wants the account id on every log line. The `Full` format prints
/// the span scope that is current when the event fires, together with
/// the span's fields, so anything logged inside this span carries the
/// id without every call site having to repeat it.
pub fn account_span(account: &Account) -> tracing::Span {
    tracing::info_span!(
        "account",
        account_id = %account.id,
        user = %account.user,
        registrar = %account.registrar,
    )
}

/// The span for one call.
///
/// It is meant to be nested inside [`account_span`], so a call line
/// carries the account id *and* the call id.
pub fn call_span(call_id: &str) -> tracing::Span {
    tracing::info_span!("call", call_id = %call_id)
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A writer that keeps what it is given.
    ///
    /// `Write` is implemented for `&Buffer` (not for `Buffer`) because
    /// `Arc<Buffer>` only satisfies `MakeWriter` when `&Buffer: Write`.
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for &Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("buffer lock").extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_account_span_reaches_the_log_line() {
        // §6: the account id has to be on the line, not just in the
        // event's own fields — otherwise a line that does not name
        // its account cannot be traced back.
        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(Arc::new(buffer.clone()))
            .with_ansi(false)
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            // The span has to be created *inside* `with_default`: a span
            // binds to the subscriber that is current when it is made.
            let span = tracing::info_span!("account", account_id = "acct-123", user = "104");
            let _guard = span.enter();
            tracing::info!("something happened");
        });

        let out = String::from_utf8(buffer.0.lock().expect("buffer lock").clone()).unwrap();
        assert!(
            out.contains("acct-123"),
            "the account id must be on the log line: {out}"
        );
    }

    #[test]
    fn a_call_line_carries_the_account_id_and_the_call_id() {
        // §6 wants both ids on every line. The call span is nested in
        // the account span, and the formatter prints the whole scope.
        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(Arc::new(buffer.clone()))
            .with_ansi(false)
            .finish();

        let account = Account::new("Office", "example.com", "104", "", "udp");
        tracing::subscriber::with_default(subscriber, || {
            let account_span = account_span(&account);
            let _account_guard = account_span.enter();
            let call_span = call_span("call-42");
            let _call_guard = call_span.enter();
            tracing::info!("the call was answered");
        });

        let out = String::from_utf8(buffer.0.lock().expect("buffer lock").clone()).unwrap();
        assert!(
            out.contains(&account.id),
            "the account id must be on the log line: {out}"
        );
        assert!(
            out.contains("call-42"),
            "the call id must be on the log line: {out}"
        );
    }
}
