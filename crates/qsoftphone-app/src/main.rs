//! qsoftphone — the desktop softphone.
//!
//! Startup order matters and is deliberate:
//!
//! 1. detect the paths and create the directories
//! 2. load the settings file (before logging, so the log level is
//!    taken from the settings)
//! 3. install the tracing subscriber
//! 4. open the database
//! 5. hand over to egui
//!
//! `--smoke` runs steps 1-4 and exits, which is what CI and the
//! "does it start and exit cleanly" check use.

mod app;

use std::process::ExitCode;

use qsoftphone_core::config::AppConfig;
use qsoftphone_core::storage::{AccountStore, Db};
use qsoftphone_core::{logging, paths::AppPaths};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("qsoftphone: fatal: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Paths.
    let paths = AppPaths::detect();
    paths.ensure()?;

    // 2. Settings — read before logging so the level is applied.
    let config = AppConfig::load(&paths.config_file())?;

    // 3. Logging.
    let _log_guard = logging::init(&paths, &config.logging.level)?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        config_dir = %paths.config_dir().display(),
        data_dir = %paths.data_dir().display(),
        "qsoftphone-rs starting"
    );

    // 4. Storage.
    let db = Db::open(&paths.accounts_db())?;
    let account_count = AccountStore::new(&db).count()?;
    tracing::info!(accounts = account_count, "accounts loaded");

    // The SIP stack lands in M1. Until then the app starts, shows its
    // shell and exits cleanly on request.
    if std::env::args().any(|a| a == "--smoke") {
        tracing::info!("smoke test: startup path OK");
        println!("qsoftphone smoke test OK");
        return Ok(());
    }

    // 5. UI. The tokio runtime is the one the SIP stack will run on.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let handle = runtime.handle().clone();

    // The session registry is where the registrar publishes the live
    // endpoints, and the bus is where the call events travel. Both are
    // handles: the registrar and the call manager share them.
    let sessions = qsoftphone_sip::SessionRegistry::new();
    let (bus, call_events) = qsoftphone_sip::CallBus::new();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([config.ui.window_width, config.ui.window_height]),
        ..Default::default()
    };

    eframe::run_native(
        "qsoftphone",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(app::QSoftphoneApp::new(
                paths.clone(),
                db,
                config.clone(),
                handle.clone(),
                sessions.clone(),
                bus.clone(),
                call_events,
            )))
        }),
    )
    .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;

    tracing::info!("qsoftphone-rs exited cleanly");
    Ok(())
}
