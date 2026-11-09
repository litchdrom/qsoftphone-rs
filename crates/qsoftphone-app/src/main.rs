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
mod history;
mod picker;

use std::process::ExitCode;

use qsoftphone_core::config::AppConfig;
use qsoftphone_core::storage::{AccountStore, Db};
use qsoftphone_core::{logging, paths::AppPaths};

fn main() -> ExitCode {
    // The clock for the startup timing: the first frame says how long
    // the app took to get there.
    app::mark_started();

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

    // The `--call` diagnostic: a call without a window. It registers,
    // calls, and says what the media did — the only way to find out
    // what a call carried when there is nobody at the machine to
    // listen to it.
    if let Some((number, seconds)) = diagnostic_call() {
        let mut config = config;
        if let Some(codec) = diagnostic_codec() {
            config.media.codecs = vec![codec.name().to_string()];
        }
        let mut app = app::QSoftphoneApp::new(
            paths.clone(),
            db,
            config.clone(),
            handle.clone(),
            sessions.clone(),
            bus.clone(),
            call_events,
        );
        diagnostic_run(&mut app, &number, seconds);
        return Ok(());
    }

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

// ---------------------------------------------------------------------------
// The `--call` diagnostic: a call without a window.
// ---------------------------------------------------------------------------

/// `--call <number> [--seconds N] [--codec NAME]`
fn diagnostic_call() -> Option<(String, u64)> {
    let mut args = std::env::args().skip(1);
    let mut number = None;
    let mut seconds = 20u64;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--call" => number = args.next(),
            "--seconds" => seconds = args.next().and_then(|s| s.parse().ok()).unwrap_or(seconds),
            _ => {}
        }
    }
    number.map(|number| (number, seconds))
}

/// The codec the diagnostic call should offer.
fn diagnostic_codec() -> Option<qsoftphone_media::Codec> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--codec" {
            let name = args.next()?;
            return qsoftphone_media::Codec::from_name(&name);
        }
    }
    None
}

/// Register, call, wait, hang up — with no window.
///
/// The log is the output: the levels of the call, once a second, and
/// the numbers of the media when it stops.
fn diagnostic_run(app: &mut app::QSoftphoneApp, number: &str, seconds: u64) {
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1100.0, 700.0),
        )),
        ..Default::default()
    };

    let frame = |app: &mut app::QSoftphoneApp| {
        let mut output = ctx.run_ui(input.clone(), |ui| app.draw(ui));
        output.textures_delta.clear();
        output
    };

    app.start_registration();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline && !app.is_registered() {
        std::thread::sleep(std::time::Duration::from_millis(50));
        frame(app);
    }
    if !app.is_registered() {
        tracing::error!("the account did not register");
        return;
    }
    tracing::info!("registered; calling {number}");

    app.call_now(number);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline && !app.call_is_up() {
        std::thread::sleep(std::time::Duration::from_millis(50));
        frame(app);
    }
    if !app.call_is_up() {
        tracing::error!("the call did not connect");
        app.hangup_now();
        return;
    }
    tracing::info!("the call is up; letting it run for {seconds}s");
    if let Some(media) = app.media_of_active() {
        tracing::info!("the media of the call: {media}");
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let mut last_report = std::time::Instant::now();
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
        frame(app);
        if last_report.elapsed() >= std::time::Duration::from_secs(1) {
            last_report = std::time::Instant::now();
            if let Some(levels) = app.levels_of_active() {
                tracing::info!("the levels of the call: {levels}");
            }
        }
    }

    app.hangup_now();
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        frame(app);
    }
    tracing::info!("the diagnostic call is over");
}
