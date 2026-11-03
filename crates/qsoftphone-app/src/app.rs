//! The egui shell.
//!
//! M0 scope: the window, the account list and the account editor.
//! M1 adds the registration state. M2 adds the dialer, the call panel
//! and the incoming-call modal.
//!
//! Note the egui 0.37 API: `eframe::App` is driven by `App::ui`,
//! which hands over a root `Ui` rather than a `Context`, and the
//! panel types are unified into `egui::Panel`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use eframe::egui;
use qsoftphone_core::config::AppConfig;
use qsoftphone_core::model::account::{Account, now_ms};
use qsoftphone_core::model::call_event::{CallEventType, Direction};
use qsoftphone_core::model::history::{
    CallRecord, CallResult, Direction as HistoryDirection, LocalTime, classify_result,
    format_duration, history_direction,
};
use qsoftphone_core::paths::AppPaths;
use qsoftphone_core::sip::uri::sip_uri_user_part;
use qsoftphone_core::storage::{AccountStore, Db};
use qsoftphone_media::{Codec, DeviceInfo};
use qsoftphone_sip::{
    CallBus, CallManager, RegistrationManager, RegistrationState, SessionRegistry,
};

use crate::history::{HistoryService, HistoryUpdate};

/// The application state.
pub struct QSoftphoneApp {
    paths: AppPaths,
    /// The database, shared with the history worker (§6: the history
    /// is read and written on the runtime, never on the UI thread).
    db: Arc<Db>,
    config: AppConfig,
    /// Handle to the tokio runtime the SIP stack runs on. Kept so
    /// the runtime is not dropped while the window is open, and used
    /// for the work that must not happen on the interface thread
    /// (§3): reading the sound card's devices and playing the test
    /// sound.
    runtime: tokio::runtime::Handle,
    /// The registrations, and the state each one is in.
    registrations: RegistrationManager,
    /// The calls, and the state each one is in.
    calls: CallManager,
    /// The history, and the rows it last answered with.
    history: HistoryService,
    /// The history rows the UI shows.
    history_rows: Vec<CallRecord>,
    /// What the history search box holds.
    history_query: String,
    /// The direction the history is filtered by; `None` is all of them.
    history_direction: Option<HistoryDirection>,
    /// The history row that is expanded.
    history_expanded: Option<i64>,
    /// What the UI shows about each call, keyed by SIP Call-ID.
    call_views: HashMap<String, CallView>,
    /// The calls in the order they arrived.
    ///
    /// The map above is a lookup; this is the list the UI shows.
    call_order: Vec<String>,
    /// The call the call panel is showing.
    active_call_id: Option<String>,
    /// Which page the middle panel shows.
    view: View,
    /// The number the dialer has.
    dial_number: String,
    states: HashMap<String, RegistrationState>,
    accounts: Vec<Account>,
    /// Index into `accounts` of the selected row.
    selected: Option<usize>,
    /// The form for creating a *new* account.
    ///
    /// It is a separate draft from the editor on purpose: with one
    /// shared draft, typing a new account silently changed what
    /// "save" would write to the selected one.
    new_account: Draft,
    /// The form for the selected account.
    edit_account: Draft,
    /// The id the editor was loaded from.
    ///
    /// The editor is reloaded whenever the selection changes, so a
    /// form left half-typed is never written to a different account.
    editing_id: Option<String>,
    status: String,
    /// The theme is applied once, on the first frame.
    theme_applied: bool,
    /// Whether the first frame has been drawn — the startup timing.
    first_frame_drawn: bool,
    /// The audio devices of this machine.
    ///
    /// They are read on the runtime (§3): opening a device is a slow
    /// operation, and the interface thread must not wait for it.
    devices: Vec<DeviceInfo>,
    /// Whether the devices have been read yet.
    devices_loaded: bool,
    /// Whether this is a remote session.
    ///
    /// It cannot change while the app runs, so it is read once at
    /// startup. A remote session redirects the playback but not the
    /// microphone, and the switch for the microphone is on the
    /// **client** — so the user has to be told where to look.
    remote_session: bool,
    /// What the test sound is doing.
    sound_test: SoundTest,
    /// What the echo test is doing.
    echo_test: EchoTest,
    /// When the echo test started.
    ///
    /// The test opens the sound card on another thread. If the card
    /// never answers, the button would sit at "Listening…" for ever,
    /// which looks like a frozen app — so the test is given up on.
    echo_started: Option<std::time::Instant>,
    /// The live level of the microphone, from 0 to 1.
    ///
    /// It is the loudest sample of the last capture callback, and it is
    /// what the level meter shows. A microphone that never moves it is
    /// a microphone that is not working.
    echo_level: f32,
    /// The loudest the microphone has been during this test.
    echo_peak: f32,
    /// Where the live level arrives.
    echo_levels: Option<std::sync::mpsc::Receiver<f32>>,
    /// The results of the background media work.
    media_updates: Receiver<MediaUpdate>,
    /// The way back to the runtime that does the work.
    media_updates_tx: Sender<MediaUpdate>,
}

/// What the background media work has to say.
enum MediaUpdate {
    /// The sound card's devices.
    Devices(Vec<DeviceInfo>),
    /// The test sound has finished — or could not be played.
    SoundTest(Result<(), String>),
    /// The echo test has finished — or could not be run.
    EchoTest(Result<qsoftphone_media::EchoReport, String>),
}

/// What the test sound is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SoundTest {
    /// Nothing has been asked for.
    Idle,
    /// The tone is playing.
    Playing,
    /// The tone has been played.
    Done,
    /// The tone could not be played.
    Failed(String),
}

/// What the echo test is doing.
///
/// The echo test is the microphone, played back through the speaker:
/// speak, and hear yourself. It is the only test of the sound card
/// that a user can do alone.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EchoTest {
    /// Nothing has been asked for.
    Idle,
    /// The microphone is being played back.
    Running,
    /// The test has finished, with what it heard.
    Done {
        /// What it heard, in words.
        text: String,
        /// The speaker it played through.
        ///
        /// A headphone jack is the commonest reason a test is silent,
        /// and the name is what says so.
        speaker: String,
    },
    /// The test could not be run.
    Failed(String),
}

/// Which page the middle panel shows.
///
/// The dialer is what the window opens on: a softphone is for calling.
/// The history is a page away, and so are the settings — the codecs,
/// the sound card and the test sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    /// The dialer and the calls.
    Call,
    /// The call history.
    History,
    /// The account settings.
    Account,
    /// The sound and the codecs.
    Settings,
}

/// What the UI shows about one call.
#[derive(Debug, Clone, Default)]
struct CallView {
    /// The account the call belongs to.
    account_id: String,
    /// The SIP Call-ID.
    call_id: String,
    /// Which way the call was going.
    direction: Option<Direction>,
    /// The peer: the caller for an incoming call, the callee for an
    /// outgoing one — the user-part, the way the list shows it.
    peer: String,
    /// The peer's full URI, as the stack saw it.
    ///
    /// The history row keeps the URI and derives the number from it
    /// (§5.3), so the URI has to survive the call.
    peer_uri: String,
    /// The resolved display name of the peer.
    caller_id: String,
    /// The number the remote party actually dialled.
    dialed_number: String,
    /// `initiating`, `ringing`, `connected`, `ended`.
    state: String,
    /// The last DTMF digit that arrived.
    digit: String,
    /// Whether the call is over.
    ended: bool,
    /// When the call started, epoch milliseconds.
    started_at: i64,
    /// When it ended, epoch milliseconds.
    ended_at: i64,
    /// Whether the call was answered.
    answered: bool,
    /// The final SIP status code.
    sip_code: i32,
    /// The final SIP reason phrase.
    hangup_cause: String,
}

/// What the history page asked for when a button was pressed.
#[derive(Debug, Clone)]
enum HistoryAction {
    /// Expand or collapse a row.
    Toggle(i64),
    /// Redial a call.
    Redial(CallRecord),
}

/// The account form.
///
/// The same shape backs the "add account" fields and the editor for
/// the selected account, so a field that exists in one exists in the
/// other.
#[derive(Debug, Clone, Default)]
struct Draft {
    label: String,
    registrar: String,
    user: String,
    password: String,
    transport: String,
    domain: String,
    outbound_proxy: String,
    caller_id_header: String,
    dialed_number_header: String,
}

impl Draft {
    /// A new account from the form.
    ///
    /// The id and the timestamps are left empty — the store fills them
    /// in.
    fn to_account(&self) -> Account {
        let mut account = Account::new(
            self.label.trim(),
            self.registrar.trim(),
            self.user.trim(),
            self.password.clone(),
            self.transport.trim(),
        );
        account.domain = self.domain.trim().to_string();
        account.outbound_proxy = self.outbound_proxy.trim().to_string();
        account.caller_id_header = self.caller_id_header.trim().to_string();
        account.dialed_number_header = self.dialed_number_header.trim().to_string();
        account
    }

    /// The form filled in from an account.
    fn from_account(account: &Account) -> Self {
        Self {
            label: account.label.clone(),
            registrar: account.registrar.clone(),
            user: account.user.clone(),
            password: account.password.clone(),
            transport: account.transport.clone(),
            domain: account.domain.clone(),
            outbound_proxy: account.outbound_proxy.clone(),
            caller_id_header: account.caller_id_header.clone(),
            dialed_number_header: account.dialed_number_header.clone(),
        }
    }
}

impl QSoftphoneApp {
    pub fn new(
        paths: AppPaths,
        db: Db,
        config: AppConfig,
        runtime: tokio::runtime::Handle,
        sessions: SessionRegistry,
        bus: CallBus,
        call_events: tokio::sync::mpsc::UnboundedReceiver<qsoftphone_sip::CallUpdate>,
    ) -> Self {
        let accounts = AccountStore::new(&db).list().unwrap_or_default();
        // Prefer the account the settings file points at; fall back
        // to the first one.
        let selected = config
            .active_account_id
            .as_ref()
            .and_then(|id| accounts.iter().position(|a| &a.id == id))
            .or_else(|| (!accounts.is_empty()).then_some(0));
        // The history reads and writes the same database, on the
        // runtime (§6).
        let db = Arc::new(db);
        let history = HistoryService::new(runtime.clone(), db.clone());
        let mut calls =
            CallManager::new(runtime.clone(), sessions.clone(), bus.clone(), call_events);
        // The media settings are shared with the registrar: the codec
        // the user picks has to reach the offer that is built.
        calls.set_media_config(media_config_of(&config));
        let (media_updates_tx, media_updates) = std::sync::mpsc::channel();
        let mut app = Self {
            paths,
            db,
            config,
            runtime: runtime.clone(),
            registrations: RegistrationManager::new(runtime.clone(), sessions.clone(), bus.clone()),
            calls,
            history,
            history_rows: Vec::new(),
            history_query: String::new(),
            history_direction: None,
            history_expanded: None,
            call_views: HashMap::new(),
            call_order: Vec::new(),
            active_call_id: None,
            view: View::Call,
            dial_number: String::new(),
            states: HashMap::new(),
            accounts,
            selected,
            new_account: Draft {
                transport: "udp".to_string(),
                ..Default::default()
            },
            edit_account: Draft::default(),
            editing_id: None,
            status: "Ready.".to_string(),
            theme_applied: false,
            first_frame_drawn: false,
            devices: Vec::new(),
            devices_loaded: false,
            remote_session: qsoftphone_media::is_remote_session(),
            sound_test: SoundTest::Idle,
            echo_test: EchoTest::Idle,
            echo_started: None,
            echo_level: 0.0,
            echo_peak: 0.0,
            echo_levels: None,
            media_updates,
            media_updates_tx,
        };
        app.load_selected();
        // The history is read on the runtime; the rows arrive on the
        // channel and are drained at the top of the frame (§3).
        app.history.query("", None);
        app
    }

    /// Drain the registration state changes that arrived since the
    /// last frame (§3: events come on a channel and are drained here).
    fn drain_registrations(&mut self) {
        for event in self.registrations.drain() {
            let state = event.state.clone();
            self.status = format!("{} — {}", event.user, state.summary());
            self.states.insert(event.account_id, state);
        }
    }

    /// Drain the call events that arrived since the last frame.
    fn drain_calls(&mut self) {
        for update in self.calls.drain() {
            let event = update.event;
            if event.call_id.is_empty() {
                continue;
            }
            let call_id = event.call_id.clone();

            let is_new = !self.call_views.contains_key(&call_id);
            let view = self
                .call_views
                .entry(call_id.clone())
                .or_insert_with(|| CallView {
                    call_id: call_id.clone(),
                    started_at: now_ms(),
                    ..Default::default()
                });
            if is_new {
                self.call_order.push(call_id.clone());
            }

            view.account_id = update.account_id.clone();
            if let Some(direction) = event.call_direction {
                view.direction = Some(direction);
            }
            if !event.caller_id.is_empty() {
                view.caller_id = event.caller_id.clone();
            }
            if !event.dialed_number.is_empty() {
                view.dialed_number = event.dialed_number.clone();
            }
            if !event.call_state.is_empty() {
                view.state = event.call_state.clone();
            }
            if !event.digit.is_empty() {
                view.digit = event.digit.clone();
            }
            if event.sip_code != 0 {
                view.sip_code = event.sip_code;
            }
            if !event.hangup_cause.is_empty() {
                view.hangup_cause = event.hangup_cause.clone();
            }
            match event.event_type {
                CallEventType::IncomingCall => {
                    // The event carries the peer's URI; the list shows
                    // the user-part of it.
                    view.peer_uri = event.caller.clone();
                    view.peer = sip_uri_user_part(&event.caller);
                    // A call is coming in: show the call page, not the
                    // account settings.
                    self.view = View::Call;
                }
                CallEventType::OutgoingCallInitiated => {
                    view.peer_uri = event.callee.clone();
                    view.peer = sip_uri_user_part(&event.callee);
                }
                CallEventType::CallAnswered => view.answered = true,
                CallEventType::CallEnded => {
                    view.ended = true;
                    view.ended_at = now_ms();
                    // The call is over: it belongs in the history
                    // (§5.3).
                    if let Some(record) = Self::history_record(view) {
                        self.history.record(record);
                    }
                    // The status line goes back to what it says when
                    // there is nothing to say. It used to stay on
                    // "Hanging up…" for ever, because nothing ever
                    // said the call was over — and a status line that
                    // lies about the state of the app is worse than no
                    // status line at all.
                    self.status = "Ready.".to_string();
                }
                _ => {}
            }

            // The call panel follows the newest call that is not over.
            if !view.ended && self.active_call_id.is_none() {
                self.active_call_id = Some(call_id);
            }
        }
    }

    /// The history row for a finished call — §5.3.
    ///
    /// The remote party is the **caller** for an incoming call and the
    /// **callee** for an outgoing one; never our own extension. The
    /// dialled number is kept separately: it is what a redial uses
    /// (§5.1), and for a DID-forwarded call it is not the remote
    /// party.
    fn history_record(view: &CallView) -> Option<CallRecord> {
        let direction = match view.direction? {
            Direction::Incoming => HistoryDirection::Incoming,
            Direction::Outgoing => HistoryDirection::Outgoing,
        };
        let mut record = match direction {
            HistoryDirection::Incoming => CallRecord::from_incoming(
                view.account_id.clone(),
                view.call_id.clone(),
                view.peer_uri.clone(),
                view.dialed_number.clone(),
                view.caller_id.clone(),
                view.started_at,
                view.ended_at,
                view.sip_code,
                view.hangup_cause.clone(),
                view.answered,
            ),
            _ => CallRecord::from_outgoing(
                view.account_id.clone(),
                view.call_id.clone(),
                view.peer_uri.clone(),
                view.caller_id.clone(),
                view.started_at,
                view.ended_at,
                view.sip_code,
                view.hangup_cause.clone(),
                view.answered,
            ),
        };
        // An incoming call that was never answered is a missed call.
        record.direction = history_direction(record.direction, record.answered, record.sip_code);
        Some(record)
    }

    /// Drain the history answers that arrived since the last frame.
    fn drain_history(&mut self) {
        for update in self.history.drain() {
            match update {
                HistoryUpdate::Rows(rows) => self.history_rows = rows,
                HistoryUpdate::Failed(error) => {
                    self.status = format!("The history could not be read: {error}");
                }
            }
        }
    }

    /// Redial a call from the history — §5.1.
    ///
    /// The target is the **peer**: the caller for an incoming call, the
    /// number that was dialled for an outgoing one. The call goes out on
    /// the account the call was on, not on whichever account happens to
    /// be selected now.
    fn redial(&mut self, record: &CallRecord) {
        let account = self
            .accounts
            .iter()
            .find(|account| account.id == record.account_id)
            .or_else(|| self.selected_account())
            .cloned();
        let Some(account) = account else {
            self.status = "There is no account to redial on.".to_string();
            return;
        };
        let target = record.redial_target(&account.user).to_string();
        if target.is_empty() {
            self.status = "That call has no number to redial.".to_string();
            return;
        }
        self.place_call_on(&account, &target);
    }

    /// Place a call to a target.
    fn place_call_to(&mut self, target: &str) {
        let Some(account) = self.selected_account().cloned() else {
            return;
        };
        self.place_call_on(&account, target);
    }

    /// Place a call to a target, on a given account.
    fn place_call_on(&mut self, account: &Account, target: &str) {
        let target = target.trim().to_string();
        if target.is_empty() {
            return;
        }
        // The echo test is holding the sound card. A call that starts
        // now would open the microphone a second time, and two clients
        // on one endpoint is what makes the audio break up.
        if self.echo_test == EchoTest::Running {
            self.status = "The echo test is running — wait for it to finish.".to_string();
            return;
        }
        // The dialer shows what is being called, so a redial from the
        // history is visible too.
        self.dial_number = target.clone();
        match self.calls.place_call(&account.id, &target) {
            Ok(call_id) => {
                self.active_call_id = Some(call_id);
                self.view = View::Call;
                self.status = format!("Calling {target}…");
            }
            Err(e) => self.status = format!("Could not place the call: {e}"),
        }
    }

    /// The calls, in the order they arrived.
    fn calls(&self) -> Vec<CallView> {
        self.call_order
            .iter()
            .filter_map(|id| self.call_views.get(id).cloned())
            .collect()
    }

    /// The state of an account's registration.
    fn registration_state(&self, account_id: &str) -> &RegistrationState {
        self.states
            .get(account_id)
            .unwrap_or(&RegistrationState::Idle)
    }

    /// The selected account, if there is one.
    fn selected_account(&self) -> Option<&Account> {
        self.selected.and_then(|i| self.accounts.get(i))
    }

    /// The call the panel is showing.
    fn active_call(&self) -> Option<&CallView> {
        let id = self.active_call_id.as_ref()?;
        self.call_views.get(id)
    }

    /// The call the incoming-call modal is for.
    ///
    /// §5.7: the modal is for a call that is ringing and has not been
    /// answered yet.
    fn incoming_call(&self) -> Option<&CallView> {
        self.call_views.values().find(|call| {
            call.direction == Some(Direction::Incoming)
                && !call.ended
                && (call.state == "ringing" || call.state.is_empty())
        })
    }

    /// Load the selected account into the editor.
    fn load_selected(&mut self) {
        match self.selected_account().cloned() {
            Some(account) => {
                self.edit_account = Draft::from_account(&account);
                self.editing_id = Some(account.id.clone());
            }
            None => {
                self.edit_account = Draft::default();
                self.editing_id = None;
            }
        }
    }

    /// Write the editor back to the selected account.
    ///
    /// The id and the creation time are kept: the account is the same
    /// one, with different settings.
    fn save_selected(&mut self) {
        let Some(index) = self.selected else {
            return;
        };
        let Some(stored) = self.accounts.get(index).cloned() else {
            return;
        };
        if self.editing_id.as_deref() != Some(stored.id.as_str()) {
            self.status = "The selection changed — pick the account again.".to_string();
            self.load_selected();
            return;
        }

        let mut account = self.edit_account.to_account();
        account.id = stored.id.clone();
        account.created_at = stored.created_at;
        account.capture_headers = stored.capture_headers.clone();
        account.crm_preset_id = stored.crm_preset_id.clone();

        match AccountStore::new(&self.db).upsert(&account) {
            Ok(saved) => {
                self.status = format!("Saved account {}.", saved.label);
                self.reload();
                self.load_selected();
            }
            Err(e) => self.status = format!("Could not save the account: {e}"),
        }
    }

    fn register_selected(&mut self) {
        let Some(account) = self.selected_account().cloned() else {
            return;
        };
        if account.registrar.trim().is_empty() {
            self.status = "That account has no registrar.".to_string();
            return;
        }
        self.registrations.start(&account, self.config.network.port);
        self.status = format!("Registering {}…", account.label);
    }

    fn unregister_selected(&mut self) {
        let Some(account) = self.selected_account().cloned() else {
            return;
        };
        self.registrations.stop(&account.id);
        self.states.insert(account.id, RegistrationState::Idle);
        self.status = format!("Unregistered {}.", account.label);
    }

    /// Place a call to the number in the dialer.
    fn place_call(&mut self) {
        if self.dial_number.trim().is_empty() {
            self.status = "There is no number to call.".to_string();
            return;
        }
        let target = self.dial_number.trim().to_string();
        self.place_call_to(&target);
    }

    /// Answer the incoming call.
    fn answer_call(&mut self) {
        let Some(call) = self.incoming_call().cloned() else {
            return;
        };
        match self.calls.answer(&call.account_id, &call.call_id) {
            Ok(()) => self.status = "Answered.".to_string(),
            Err(e) => self.status = format!("Could not answer: {e}"),
        }
    }

    /// Decline the incoming call.
    fn decline_call(&mut self) {
        let Some(call) = self.incoming_call().cloned() else {
            return;
        };
        match self.calls.decline(&call.account_id, &call.call_id) {
            Ok(()) => self.status = "Declined.".to_string(),
            Err(e) => self.status = format!("Could not decline: {e}"),
        }
    }

    /// Hang the active call up.
    fn hangup_call(&mut self) {
        let Some(call) = self.active_call().cloned() else {
            return;
        };
        match self.calls.hangup(&call.account_id, &call.call_id) {
            Ok(()) => self.status = "Hanging up…".to_string(),
            Err(e) => self.status = format!("Could not hang up: {e}"),
        }
    }

    /// Send a DTMF digit on the active call.
    fn send_dtmf(&mut self, digit: &str) {
        let Some(call) = self.active_call().cloned() else {
            return;
        };
        if let Err(e) = self.calls.send_dtmf(&call.account_id, &call.call_id, digit) {
            self.status = format!("Could not send the digit: {e}");
        }
    }

    /// Mute or unmute the microphone of a call.
    fn set_mic_muted(&mut self, call: &CallView, muted: bool) {
        if let Err(e) = self
            .calls
            .set_mic_muted(&call.account_id, &call.call_id, muted)
        {
            self.status = format!("Could not change the microphone: {e}");
        }
    }

    /// Mute or unmute the speaker of a call.
    fn set_speaker_muted(&mut self, call: &CallView, muted: bool) {
        if let Err(e) = self
            .calls
            .set_speaker_muted(&call.account_id, &call.call_id, muted)
        {
            self.status = format!("Could not change the speaker: {e}");
        }
    }

    /// The codecs the offer lists, in the order they are preferred.
    ///
    /// The C++ let the user pick a codec per call, and the pick was
    /// kept in the settings file. It is kept here too: the codec is a
    /// property of the softphone, not of one account, and a call page
    /// is no place for a list of them.
    fn set_codecs(&mut self, codecs: Vec<String>) {
        if codecs.is_empty() {
            // A call with no codec has no media at all.
            self.status = "At least one codec has to stay on.".to_string();
            return;
        }
        self.config.media.codecs = codecs;

        // The registrar and the call layer share the settings, so the
        // pick has to be pushed into both.
        let media = media_config_of(&self.config);
        self.calls.set_media_config(media);

        if let Err(e) = self.config.save(&self.paths.config_file()) {
            tracing::warn!(error = %e, "could not persist the codec choice");
        }
    }

    /// The codecs the offer lists, in order.
    fn enabled_codecs(&self) -> Vec<Codec> {
        self.config
            .media
            .codecs
            .iter()
            .filter_map(|name| Codec::from_name(name))
            .collect()
    }

    /// Take a codec out of the offer.
    fn remove_codec(&mut self, codec: Codec) {
        let mut codecs = self.enabled_codecs();
        codecs.retain(|c| *c != codec);
        let names: Vec<String> = codecs.iter().map(|c| c.name().to_string()).collect();
        self.set_codecs(names);
    }

    /// Put a codec back into the offer, at the end.
    fn add_codec(&mut self, codec: Codec) {
        let mut codecs = self.enabled_codecs();
        if codecs.contains(&codec) {
            return;
        }
        codecs.push(codec);
        let names: Vec<String> = codecs.iter().map(|c| c.name().to_string()).collect();
        self.set_codecs(names);
    }

    /// Move a codec up or down in the offer.
    fn move_codec(&mut self, codec: Codec, up: bool) {
        let mut codecs = self.enabled_codecs();
        let Some(index) = codecs.iter().position(|c| *c == codec) else {
            return;
        };
        let other = if up {
            index.checked_sub(1)
        } else {
            index.checked_add(1)
        };
        let Some(other) = other else {
            return;
        };
        if other >= codecs.len() {
            return;
        }
        codecs.swap(index, other);
        let names: Vec<String> = codecs.iter().map(|c| c.name().to_string()).collect();
        self.set_codecs(names);
    }

    /// The codecs that are not in the offer.
    fn disabled_codecs(&self) -> Vec<Codec> {
        let enabled = self.enabled_codecs();
        Codec::ALL
            .iter()
            .copied()
            .filter(|codec| !enabled.contains(codec))
            .collect()
    }

    /// Set the microphone — and persist it.
    fn set_microphone(&mut self, name: String) {
        self.config.media.microphone = name;
        self.push_media_config();
    }

    /// Set the speaker — and persist it.
    fn set_speaker(&mut self, name: String) {
        self.config.media.speaker = name;
        self.push_media_config();
    }

    /// Push the media settings into the call layer and into the
    /// settings file.
    fn push_media_config(&mut self) {
        let media = media_config_of(&self.config);
        self.calls.set_media_config(media);
        if let Err(e) = self.config.save(&self.paths.config_file()) {
            tracing::warn!(error = %e, "could not persist the sound settings");
        }
    }

    /// Read the sound card's devices.
    ///
    /// This opens the host and asks it what it has, which takes a
    /// moment — so it happens on the runtime and not on the interface
    /// thread (§3). The devices arrive on a channel and are drained
    /// at the top of the frame.
    fn load_devices(&mut self) {
        if self.devices_loaded {
            return;
        }
        self.devices_loaded = true;

        let sender = self.media_updates_tx.clone();
        self.runtime.spawn_blocking(move || {
            let devices = qsoftphone_media::devices();
            let _ = sender.send(MediaUpdate::Devices(devices));
        });
    }

    /// Play the test sound through the speaker.
    ///
    /// The tone is a third of a second long, and playing it means
    /// opening the device — so it happens on the runtime (§3).
    fn play_test_sound(&mut self) {
        if self.sound_test == SoundTest::Playing {
            return;
        }
        let speaker = self.config.media.speaker.clone();
        let speaker = if speaker.trim().is_empty() {
            None
        } else {
            Some(speaker)
        };

        self.sound_test = SoundTest::Playing;
        let sender = self.media_updates_tx.clone();
        self.runtime.spawn_blocking(move || {
            let outcome =
                qsoftphone_media::play_test_sound(speaker.as_deref()).map_err(|e| e.to_string());
            let _ = sender.send(MediaUpdate::SoundTest(outcome));
        });
    }

    /// Play the microphone back through the speaker.
    ///
    /// The echo test: speak, and hear yourself. It checks the
    /// microphone, the speaker and the two together, and it is the
    /// only test of the sound card that a user can do alone.
    ///
    /// It opens the sound card, so it happens on the runtime (§3).
    fn play_echo_test(&mut self) {
        if self.echo_test == EchoTest::Running {
            return;
        }
        // The sound card is a shared resource: a call is holding the
        // microphone and the speaker, and a second client on the same
        // endpoint is what makes cpal report an overrun.
        if self.active_call().is_some() {
            self.echo_test =
                EchoTest::Failed("a call is up — the sound card is in use.".to_string());
            return;
        }

        let microphone = self.config.media.microphone.clone();
        let microphone = if microphone.trim().is_empty() {
            None
        } else {
            Some(microphone)
        };
        let speaker = self.config.media.speaker.clone();
        let speaker = if speaker.trim().is_empty() {
            None
        } else {
            Some(speaker)
        };

        self.echo_test = EchoTest::Running;
        self.echo_started = Some(std::time::Instant::now());
        self.echo_level = 0.0;
        self.echo_peak = 0.0;
        self.status = "The echo test is running — speak now.".to_string();

        // The live level comes on its own channel: it arrives from the
        // capture thread, twenty times a second, and it must not wait
        // for the next frame to be drawn.
        let (level_tx, level_rx) = std::sync::mpsc::channel::<f32>();
        self.echo_levels = Some(level_rx);

        let sender = self.media_updates_tx.clone();
        self.runtime.spawn_blocking(move || {
            let outcome = qsoftphone_media::play_echo_test(
                microphone.as_deref(),
                speaker.as_deref(),
                ECHO_SECONDS,
                Some(Box::new(move |level: f32| {
                    // A closed receiver means the app has gone; the
                    // send fails and nothing else happens.
                    let _ = level_tx.send(level);
                })),
            )
            .map_err(|e| e.to_string());
            let _ = sender.send(MediaUpdate::EchoTest(outcome));
        });
    }

    /// Take the live level of the microphone, if the echo test is
    /// running.
    ///
    /// The level is what tells a live microphone from a dead one: if
    /// the user speaks and the bar does not move, the microphone is not
    /// working, and no amount of waiting will change that.
    fn poll_echo_level(&mut self) {
        let Some(receiver) = &self.echo_levels else {
            return;
        };
        while let Ok(level) = receiver.try_recv() {
            self.echo_level = level;
            if level > self.echo_peak {
                self.echo_peak = level;
            }
        }
    }

    /// Give up on the echo test if the sound card does not come back.
    ///
    /// The test runs on its own thread, so a sound card that never
    /// answers cannot freeze the window — but the button would sit at
    /// "Listening…" for ever, which looks like a frozen app. This says
    /// what happened instead.
    fn check_echo_test_deadline(&mut self) {
        let Some(started) = self.echo_started else {
            return;
        };
        if self.echo_test != EchoTest::Running {
            return;
        }
        if started.elapsed() < ECHO_DEADLINE {
            return;
        }
        self.echo_test = EchoTest::Failed(
            "the sound card did not answer — the test was given up on.".to_string(),
        );
        self.status = "The echo test did not finish.".to_string();
    }

    /// Drain what the background media work has to say.
    fn drain_media(&mut self) {
        while let Ok(update) = self.media_updates.try_recv() {
            match update {
                MediaUpdate::Devices(devices) => {
                    self.devices = devices;
                    self.devices_loaded = true;
                }
                MediaUpdate::SoundTest(Ok(())) => {
                    self.sound_test = SoundTest::Done;
                    self.status = "The test sound has been played.".to_string();
                }
                MediaUpdate::SoundTest(Err(error)) => {
                    self.sound_test = SoundTest::Failed(error.clone());
                    self.status = format!("The test sound could not be played: {error}");
                }
                MediaUpdate::EchoTest(Ok(report)) => {
                    let text = echo_report_text(&report);
                    self.echo_test = EchoTest::Done {
                        text: text.clone(),
                        speaker: report.speaker.clone(),
                    };
                    self.echo_started = None;
                    self.echo_levels = None;
                    self.status = text;
                }
                MediaUpdate::EchoTest(Err(error)) => {
                    self.echo_test = EchoTest::Failed(error.clone());
                    self.echo_started = None;
                    self.echo_levels = None;
                    self.status = format!("The echo test could not be run: {error}");
                }
            }
        }
    }

    /// Remember the selected account in the settings file.
    fn persist_active_account(&mut self) {
        let id = self
            .selected
            .and_then(|i| self.accounts.get(i))
            .map(|a| a.id.clone());
        if id == self.config.active_account_id {
            return;
        }
        self.config.active_account_id = id.clone();
        if let Err(e) = self.config.save(&self.paths.config_file()) {
            tracing::warn!(
                account_id = id.as_deref().unwrap_or_default(),
                error = %e,
                "could not persist the active account"
            );
        }
    }

    fn reload(&mut self) {
        match AccountStore::new(&self.db).list() {
            Ok(accounts) => {
                self.accounts = accounts;
                if self.selected.is_some_and(|i| i >= self.accounts.len()) {
                    self.selected = (!self.accounts.is_empty()).then_some(0);
                }
            }
            Err(e) => self.status = format!("Could not load accounts: {e}"),
        }
    }

    fn add_account(&mut self) {
        if self.new_account.label.trim().is_empty()
            && self.new_account.user.trim().is_empty()
            && self.new_account.registrar.trim().is_empty()
        {
            self.status = "An account needs at least a label, a user or a registrar.".to_string();
            return;
        }
        let account = self.new_account.to_account();
        match AccountStore::new(&self.db).upsert(&account) {
            Ok(stored) => {
                self.status = format!("Saved account {}.", stored.label);
                self.new_account = Draft {
                    transport: "udp".to_string(),
                    ..Default::default()
                };
                self.reload();
                self.selected = self.accounts.iter().position(|a| a.id == stored.id);
                self.load_selected();
                self.persist_active_account();
            }
            Err(e) => self.status = format!("Could not save the account: {e}"),
        }
    }

    fn delete_selected(&mut self) {
        let Some(index) = self.selected else {
            return;
        };
        let Some(account) = self.accounts.get(index).cloned() else {
            return;
        };
        match AccountStore::new(&self.db).delete(&account.id) {
            Ok(true) => {
                self.status = format!("Removed account {}.", account.label);
                self.registrations.stop(&account.id);
                self.states.remove(&account.id);
                self.reload();
                self.load_selected();
            }
            Ok(false) => self.status = "That account was already gone.".to_string(),
            Err(e) => self.status = format!("Could not remove the account: {e}"),
        }
    }
}

impl QSoftphoneApp {
    /// Draw one frame.
    ///
    /// This is the whole UI. It is a separate method from
    /// [`eframe::App::ui`] so that a test can drive it headlessly —
    /// egui does not need a window to lay the widgets out.
    pub fn draw(&mut self, ui: &mut egui::Ui) {
        // How long the app took to get to its first frame.
        //
        // "It starts slowly" is a complaint that needs a number
        // behind it: the window is not up until the first frame is
        // drawn, and everything before it — the database, the settings,
        // the font, the graphics device — is part of the wait.
        if !self.first_frame_drawn {
            self.first_frame_drawn = true;
            if let Some(started) = STARTED.get() {
                tracing::info!(
                    millis = started.elapsed().as_millis(),
                    "the first frame was drawn"
                );
            }
        }

        // Ask to be drawn again.
        //
        // egui only draws when something asks it to, and the things
        // that change in this app all arrive from **outside** egui:
        // the registration, the calls, the media, the history. Without
        // this the window sits on the last picture until the user
        // happens to move the mouse — an incoming call would not show
        // up until then, and the status line would lag behind the app.
        //
        // A tenth of a second is quick enough that nothing is noticed
        // and slow enough that an idle window costs nothing.
        // The echo test is on its own thread, but it still has to be
        // given up on if the sound card never answers, and its level
        // has to be picked up while it runs.
        self.check_echo_test_deadline();
        self.poll_echo_level();

        // The level meter moves, so while the echo test runs the window
        // is drawn faster. A tenth of a second is too slow for a bar.
        let poll = if self.echo_test == EchoTest::Running {
            LEVEL_POLL
        } else {
            POLL
        };
        ui.ctx().request_repaint_after(poll);

        if !self.theme_applied {
            self.theme_applied = true;
            let visuals = if self.config.ui.theme.eq_ignore_ascii_case("light") {
                egui::Visuals::light()
            } else {
                egui::Visuals::dark()
            };
            ui.ctx().set_visuals(visuals);
        }

        // §3: the UI reads cached state; the network events arrive on
        // a channel and are drained here.
        self.drain_registrations();
        self.drain_calls();
        self.drain_history();
        self.drain_media();

        // The header. One line: who we are and whether we are
        // registered. Everything else is a page.
        egui::Panel::top("header").exact_size(32.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                let account = self.selected_account().cloned();
                let title = match &account {
                    Some(account) => {
                        let name = if account.label.is_empty() {
                            account.user.clone()
                        } else {
                            account.label.clone()
                        };
                        format!("{name} · {}@{}", account.user, account.registrar)
                    }
                    None => "No account".to_string(),
                };
                ui.label(egui::RichText::new(title).strong().size(15.0));

                ui.separator();
                let state = account
                    .as_ref()
                    .map(|account| self.registration_state(&account.id).summary())
                    .unwrap_or_default();
                ui.label(state);

                // The registration button lives next to the state it
                // changes. A softphone that needs a trip through a
                // settings form before it can register is a softphone
                // nobody uses.
                if let Some(account) = account {
                    if self.registration_state(&account.id).is_registered() {
                        if ui.button("Unregister").clicked() {
                            self.unregister_selected();
                        }
                    } else if ui.button("Register").clicked() {
                        self.register_selected();
                    }
                }
            });
        });

        // The status line. It lives at the top of the page rather than
        // in a panel of its own: the panels are for the page, and a
        // message has to be where the eye already is.
        egui::Panel::left("accounts")
            .resizable(true)
            .default_size(230.0)
            .show(ui, |ui| {
                ui.label(egui::RichText::new("ACCOUNTS").strong().size(11.0));
                ui.separator();

                // Set when the user picks a different account; applied
                // once the account list's borrow has ended.
                let mut pending_select: Option<usize> = None;

                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.accounts.is_empty() {
                        ui.weak("No accounts yet.");
                    }
                    for (index, account) in self.accounts.iter().enumerate() {
                        let label = if account.label.is_empty() {
                            account.user.clone()
                        } else {
                            account.label.clone()
                        };
                        let text = if label.is_empty() {
                            format!("(unnamed #{index})")
                        } else {
                            label
                        };
                        let badge = self.registration_state(&account.id).badge();
                        let text = if badge.is_empty() {
                            text
                        } else {
                            format!("{text} {badge}")
                        };
                        if ui
                            .selectable_value(&mut self.selected, Some(index), text)
                            .clicked()
                        {
                            pending_select = Some(index);
                        }
                    }
                });

                if let Some(index) = pending_select {
                    self.selected = Some(index);
                    // The editor follows the selection, so the form is
                    // never stale.
                    self.load_selected();
                    self.persist_active_account();
                }

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("+ Add").clicked() {
                        self.view = View::Account;
                    }
                    if ui.button("Remove").clicked() {
                        self.delete_selected();
                    }
                });
            });

        egui::CentralPanel::default().show(ui, |ui| {
            // Three pages, one line to switch between them.
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(self.view == View::Call, "Call")
                    .clicked()
                {
                    self.view = View::Call;
                }
                if ui
                    .selectable_label(self.view == View::History, "History")
                    .clicked()
                {
                    self.view = View::History;
                    self.history
                        .query(&self.history_query, self.history_direction);
                }
                if ui
                    .selectable_label(self.view == View::Account, "Account")
                    .clicked()
                {
                    self.view = View::Account;
                }
                if ui
                    .selectable_label(self.view == View::Settings, "Settings")
                    .clicked()
                {
                    self.view = View::Settings;
                }
                ui.separator();
                ui.label(egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).small());
            });
            ui.separator();

            // The status line: one line, always visible, so a message
            // never gets pushed off the bottom of the window.
            ui.label(
                egui::RichText::new(&self.status)
                    .small()
                    .color(egui::Color32::GRAY),
            );
            ui.add_space(4.0);

            match self.view {
                View::Call => self.draw_calls(ui),
                View::History => self.draw_history(ui),
                View::Account => self.draw_account(ui),
                View::Settings => self.draw_settings(ui),
            }
        });

        // The incoming-call modal.
        //
        // §5.7: it must not be closable by the user — no close button,
        // no click-outside-to-dismiss, no Esc. Only Answer and Decline
        // end it. `egui::Modal` has no window chrome, and the
        // backdrop's response is deliberately not used.
        if let Some(call) = self.incoming_call().cloned() {
            let who = if call.caller_id.is_empty() {
                call.peer.clone()
            } else {
                call.caller_id.clone()
            };
            egui::Modal::new(egui::Id::unique("incoming_call")).show(ui.ctx(), |ui| {
                ui.set_width(260.0);
                ui.heading("Incoming call");
                ui.label(who);
                if !call.dialed_number.is_empty() {
                    ui.weak(format!("Dialled: {}", call.dialed_number));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Answer").clicked() {
                        self.answer_call();
                    }
                    if ui.button("Decline").clicked() {
                        self.decline_call();
                    }
                });
            });
        }
    }

    /// The call page: the dialer, and the calls.
    fn draw_calls(&mut self, ui: &mut egui::Ui) {
        // The dialer. It is the first thing on the page, because it is
        // what the window is mostly for.
        let registered = self
            .selected_account()
            .is_some_and(|account| self.registration_state(&account.id).is_registered());

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Number").strong());
            let number = &mut self.dial_number;
            ui.add(
                egui::TextEdit::singleline(number)
                    .hint_text("6666")
                    .desired_width(200.0),
            );

            let disabled = !registered || number.trim().is_empty();
            if ui
                .add_enabled(
                    !disabled,
                    egui::Button::new("Call").min_size(egui::vec2(70.0, 0.0)),
                )
                .clicked()
            {
                self.place_call();
            }
        });
        if !registered {
            ui.weak("Register the account before it can call.");
        }

        // The keypad, in the middle of the page.
        //
        // It is between the number card and the calls, as the C++ has
        // it (`src/ui/Dialer.qml`): the number is above it, the calls
        // are below, and the keypad is where the eye already is. In
        // the corner of the window it was in a corner, hard to find
        // and half off the screen.
        ui.add_space(10.0);
        let gap = ui.spacing().item_spacing.x;
        let width = 3.0 * KEYPAD_BUTTON.x + 2.0 * gap;
        let spacer = ((ui.available_width() - width) / 2.0).max(0.0);
        ui.horizontal(|ui| {
            ui.add_space(spacer);
            self.draw_keypad(ui);
        });
        ui.add_space(6.0);

        ui.label(egui::RichText::new("CALLS").strong().size(11.0));
        ui.separator();

        // **Only the calls that are up.**
        //
        // A finished call belongs to the history, and the history has
        // its own page: listing it here as well meant the section was
        // full of rows nobody could do anything with, and the space
        // went to what had already happened instead of to what is
        // happening. A call leaves this list when it is hung up.
        let calls: Vec<CallView> = self
            .calls()
            .into_iter()
            .filter(|call| !call.ended)
            .collect();
        if calls.is_empty() {
            ui.weak("No calls up.");
            return;
        }
        for call in calls {
            self.draw_call(ui, call);
            ui.separator();
        }
    }

    /// The keypad.
    ///
    /// The C++ has one keypad, always on the dialer page, and it does
    /// both jobs: it dials the number when the app is idle and it
    /// sends the DTMF tones during a call. This is the same keypad in
    /// the same place — the middle of the page, between the number
    /// card and the calls, where the eye is.
    ///
    /// The digits are the C++'s: `*` and `#` are there for the
    /// feature codes a PBX puts in front of a call.
    fn draw_keypad(&mut self, ui: &mut egui::Ui) {
        // While a call is up, the keypad sends the tones; otherwise it
        // dials the number.
        let in_call = self
            .active_call()
            .is_some_and(|call| call.state == "connected");

        ui.vertical(|ui| {
            for row in [
                ["1", "2", "3"],
                ["4", "5", "6"],
                ["7", "8", "9"],
                ["*", "0", "#"],
            ] {
                ui.horizontal(|ui| {
                    for digit in row {
                        if ui
                            .add(egui::Button::new(digit).min_size(KEYPAD_BUTTON))
                            .clicked()
                        {
                            if in_call {
                                self.send_dtmf(digit);
                            } else {
                                self.dial_number.push_str(digit);
                            }
                        }
                    }
                });
            }
        });
    }

    /// One call, in the call list.
    fn draw_call(&mut self, ui: &mut egui::Ui, call: CallView) {
        let who = if call.caller_id.is_empty() {
            call.peer.clone()
        } else {
            call.caller_id.clone()
        };
        let arrow = match call.direction {
            Some(Direction::Incoming) => "←",
            Some(Direction::Outgoing) => "→",
            None => "·",
        };
        let active = self.active_call_id.as_deref() == Some(call.call_id.as_str());

        ui.horizontal(|ui| {
            if ui
                .selectable_label(active, format!("{arrow}  {who}"))
                .clicked()
            {
                self.active_call_id = Some(call.call_id.clone());
            }
            ui.label(&call.state);
            if !call.dialed_number.is_empty() {
                ui.weak(format!("dialled {}", call.dialed_number));
            }
            if !call.digit.is_empty() {
                ui.weak(format!("digit {}", call.digit));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !call.ended && ui.button("Hang up").clicked() {
                    self.active_call_id = Some(call.call_id.clone());
                    self.hangup_call();
                }
            });
        });

        // The keypad belongs to the call the user is looking at, and
        // only while it is up.
        if active && call.state == "connected" {
            // What the media is doing. The call is connected in SIP;
            // whether it carries a voice is a separate question, and
            // the answer belongs on the screen.
            let status = self.calls.media_status(&call.account_id, &call.call_id);
            let (summary, mic_muted, speaker_muted) = match &status {
                Some(status) => (status.summary(), status.mic_muted, status.speaker_muted),
                None => ("Media: not negotiated.".to_string(), false, false),
            };
            ui.label(
                egui::RichText::new(summary)
                    .small()
                    .color(egui::Color32::GRAY),
            );

            // The levels.
            //
            // "There is no sound" is not a diagnosis. A call has two
            // directions and three places a thing can go wrong between
            // the microphone and the ear: the peer may be sending
            // silence, our microphone may be picking up nothing, or the
            // audio may be arriving and never reaching the speaker.
            // The numbers say which.
            if let Some(status) = &status {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Peer")
                            .small()
                            .color(egui::Color32::GRAY),
                    );
                    // The bar is the level as it is now; the numbers
                    // beside it are where it got to.
                    draw_level_meter(ui, status.incoming_level, status.incoming_peak, 140.0, true);
                    ui.label(
                        egui::RichText::new(format!(
                            "peaks: peer {} · you {} · speaker {}",
                            level_text(status.incoming_peak),
                            level_text(status.outgoing_peak),
                            level_text(status.played_peak)
                        ))
                        .small()
                        .color(egui::Color32::GRAY),
                    );
                });
                if let Some(diagnosis) = silence_diagnosis(status) {
                    ui.label(
                        egui::RichText::new(diagnosis)
                            .small()
                            .color(egui::Color32::from_rgb(231, 176, 60)),
                    );
                }
            }

            // The mute buttons. A muted microphone still sends RTP —
            // it sends silence — because the peer has to see the
            // stream keep going or it tears the call down.
            ui.horizontal(|ui| {
                let mic = if mic_muted { "Unmute mic" } else { "Mute mic" };
                if ui.button(mic).clicked() {
                    self.set_mic_muted(&call, !mic_muted);
                }
                let speaker = if speaker_muted {
                    "Unmute speaker"
                } else {
                    "Mute speaker"
                };
                if ui.button(speaker).clicked() {
                    self.set_speaker_muted(&call, !speaker_muted);
                }
            });

            // The DTMF keypad is in the header, where it is always
            // visible and does double duty: it dials the number when
            // the app is idle and it sends the tones during a call.
        }
    }

    /// The history page: the searchable list of past calls.
    ///
    /// Each row shows the number, how long the call lasted, when it
    /// happened and the SIP status code, and offers a redial (§5.1)
    /// **in the row itself** — the redial is the common case, and
    /// making the user expand a row to find it is a step too many.
    /// Expanding a row shows the full URI, the cause and the exact
    /// timestamps.
    ///
    /// The legend above the list says what the marks mean: the rows
    /// use an arrow, a colour and a bare number, and none of those
    /// explain themselves.
    fn draw_history(&mut self, ui: &mut egui::Ui) {
        // The search box and the direction chips.
        ui.horizontal(|ui| {
            let mut query = self.history_query.clone();
            let response = ui.add(
                egui::TextEdit::singleline(&mut query)
                    .hint_text("Search the history…")
                    .desired_width(220.0),
            );
            if response.changed() && query != self.history_query {
                self.history_query = query;
                self.history
                    .query(&self.history_query, self.history_direction);
            }
            ui.separator();
            for (label, direction) in [
                ("All", None),
                ("Incoming", Some(HistoryDirection::Incoming)),
                ("Outgoing", Some(HistoryDirection::Outgoing)),
                ("Missed", Some(HistoryDirection::Missed)),
            ] {
                if ui
                    .selectable_label(self.history_direction == direction, label)
                    .clicked()
                {
                    self.history_direction = direction;
                    self.history
                        .query(&self.history_query, self.history_direction);
                }
            }
        });
        ui.separator();

        // The legend.
        //
        // The rows use an arrow, a colour and a bare number, and a bare
        // "486" in the middle of a line means nothing to anyone who has
        // not read the code. The key is on the page.
        ui.horizontal_wrapped(|ui| {
            let dim = egui::Color32::GRAY;
            for (direction, name) in [
                (HistoryDirection::Outgoing, "outgoing"),
                (HistoryDirection::Incoming, "incoming"),
                (HistoryDirection::Missed, "missed"),
            ] {
                direction_mark(ui, direction);
                ui.label(egui::RichText::new(name).small().color(dim));
            }
            ui.separator();
            for result in [
                CallResult::Completed,
                CallResult::Missed,
                CallResult::Busy,
                CallResult::NoAnswer,
                CallResult::Rejected,
                CallResult::Failed,
                CallResult::Cancelled,
            ] {
                ui.label(
                    egui::RichText::new(result.label())
                        .small()
                        .color(result_color(result)),
                );
            }
            ui.separator();
            ui.label(
                egui::RichText::new("a bare number is the SIP status code")
                    .small()
                    .color(dim),
            );
        });
        ui.separator();

        let clock = LocalTime::default();
        // The rows are moved out for the drawing, so that the drawing
        // does not hold a borrow of the app while the buttons are
        // acted on.
        let rows = std::mem::take(&mut self.history_rows);
        let mut action: Option<HistoryAction> = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            // The table.
            //
            // A table, with a header: the rows are read down the
            // columns, and a row of marks with nothing to say what
            // the columns are is not a table. The header is drawn even
            // when there is nothing to put under it: the page is a
            // table, and a table that only appears once it has rows is
            // a list that occasionally grows a header.
            //
            // The cells are placed at rects this code computes, and
            // that is the whole point of drawing the table by hand
            // instead of with a grid: the details of a row are long —
            // the full SIP URI, the exact timestamps — and a grid sizes
            // its columns by what is in them, so the details would
            // widen the table and then have nowhere to go but below the
            // whole thing, where a long history puts them out of sight.
            // With fixed widths the details go **under the row they
            // belong to**, which is what the C++ does
            // (`src/ui/HistoryPanel.qml`: an "Expanded details panel"
            // inside the row, filling its width).
            //
            // The columns, and whether they fit.
            //
            // The widths are fixed, but the table still has to fit the
            // page: a table whose last column hangs off the edge
            // cannot be used, and the button that opens the details
            // lives in that column. When the page is narrow the
            // columns shrink a little, each by the same share, so they
            // keep their proportions and still line up.
            //
            // The page is measured against the **clip** rectangle and
            // not against the available width: inside the scroll area
            // the available width runs past the edge of the window —
            // the panel's own margin is not clipped away — so a table
            // sized against it ends its last column outside the window,
            // and the button that opens the details is one of the
            // buttons that cannot be reached.
            let origin = ui.next_widget_position();
            let page = (ui.clip_rect().max.x - origin.x).max(320.0);

            let wanted = [
                COL_DIR,
                COL_NUMBER,
                COL_DURATION,
                COL_TIME,
                COL_CODE,
                COL_STATUS,
            ];
            let overhead = COL_GAP * wanted.len() as f32 + COL_ACTIONS;
            let room = (page - overhead).max(160.0);
            let wanted_sum: f32 = wanted.iter().sum();
            let scale = if wanted_sum > room {
                room / wanted_sum
            } else {
                1.0
            };
            let widths = [
                wanted[0] * scale,
                wanted[1] * scale,
                wanted[2] * scale,
                wanted[3] * scale,
                wanted[4] * scale,
                wanted[5] * scale,
            ];

            let gap = COL_GAP;
            let mut x = origin.x;
            for (title, width) in [
                ("dir", widths[0]),
                ("Number", widths[1]),
                ("Duration", widths[2]),
                ("Time", widths[3]),
                ("code", widths[4]),
                ("status", widths[5]),
            ] {
                let rect = egui::Rect::from_min_size(
                    egui::pos2(x, origin.y),
                    egui::vec2(width, ROW_HEIGHT),
                );
                x += width + gap;
                table_cell(ui, rect, |ui| {
                    ui.label(egui::RichText::new(title).strong().size(11.0));
                });
            }
            // The actions have no title: they are not a column of
            // information, they are what you do with the row.
            ui.allocate_space(egui::vec2(0.0, ROW_HEIGHT));

            for record in &rows {
                let expanded = self.history_expanded == Some(record.id);
                let result = classify_result(
                    record.direction,
                    record.sip_code,
                    record.duration_sec,
                    record.answered,
                    &record.hangup_cause,
                );

                // The number — never the full URI; that is what the
                // details are for. The name, when there is one, goes
                // under it, and a row with a name is two lines high.
                let number = if record.remote_number.is_empty() {
                    record.remote_uri.clone()
                } else {
                    record.remote_number.clone()
                };
                let height = if record.remote_name.is_empty() {
                    ROW_HEIGHT
                } else {
                    ROW_TWO_LINES
                };

                let origin = ui.next_widget_position();
                let mut x = origin.x;

                // Which way the call was going.
                let rect = egui::Rect::from_min_size(
                    egui::pos2(x, origin.y),
                    egui::vec2(widths[0], height),
                );
                x += widths[0] + gap;
                table_cell(ui, rect, |ui| {
                    direction_mark(ui, record.direction);
                });

                // The number.
                let rect = egui::Rect::from_min_size(
                    egui::pos2(x, origin.y),
                    egui::vec2(widths[1], height),
                );
                x += widths[1] + gap;
                table_cell(ui, rect, |ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.add(egui::Label::new(egui::RichText::new(&number).strong()).truncate());
                        if !record.remote_name.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&record.remote_name)
                                        .small()
                                        .color(egui::Color32::GRAY),
                                )
                                .truncate(),
                            );
                        }
                    });
                });

                // How long it lasted.
                let rect = egui::Rect::from_min_size(
                    egui::pos2(x, origin.y),
                    egui::vec2(widths[2], height),
                );
                x += widths[2] + gap;
                table_cell(ui, rect, |ui| {
                    ui.label(
                        egui::RichText::new(format_duration(record.duration_sec))
                            .small()
                            .color(egui::Color32::GRAY),
                    );
                });

                // When it happened.
                let rect = egui::Rect::from_min_size(
                    egui::pos2(x, origin.y),
                    egui::vec2(widths[3], height),
                );
                x += widths[3] + gap;
                table_cell(ui, rect, |ui| {
                    ui.label(
                        egui::RichText::new(clock.time(record.started_at))
                            .small()
                            .color(egui::Color32::GRAY),
                    );
                });

                // The SIP status code.
                let rect = egui::Rect::from_min_size(
                    egui::pos2(x, origin.y),
                    egui::vec2(widths[4], height),
                );
                x += widths[4] + gap;
                table_cell(ui, rect, |ui| {
                    if record.sip_code != 0 {
                        ui.label(
                            egui::RichText::new(record.sip_code.to_string())
                                .small()
                                .strong(),
                        );
                    } else {
                        ui.label("");
                    }
                });

                // And how it ended.
                let rect = egui::Rect::from_min_size(
                    egui::pos2(x, origin.y),
                    egui::vec2(widths[5], height),
                );
                x += widths[5] + gap;
                table_cell(ui, rect, |ui| {
                    ui.label(
                        egui::RichText::new(result.label())
                            .small()
                            .color(result_color(result)),
                    );
                });

                // The redial, in the row: §5.1. It is the thing
                // people come to the history for.
                //
                // The column is at least as wide as the two buttons
                // need: a button that hangs off the edge of the page
                // cannot be clicked, and that button is the one that
                // opens the details.
                let width = (origin.x + page - x).max(COL_ACTIONS);
                let rect =
                    egui::Rect::from_min_size(egui::pos2(x, origin.y), egui::vec2(width, height));
                table_cell(ui, rect, |ui| {
                    ui.horizontal(|ui| {
                        if ui.small_button("Redial").clicked() {
                            action = Some(HistoryAction::Redial(record.clone()));
                        }
                        if ui
                            .small_button(if expanded { "Hide" } else { "Details" })
                            .clicked()
                        {
                            action = Some(HistoryAction::Toggle(record.id));
                        }
                    });
                });
                // The cells were placed by hand, so the cursor has to
                // be moved past the row by hand too.
                ui.allocate_space(egui::vec2(0.0, height));

                // The details, **under the row they belong to**.
                //
                // They were under the whole table, which is the same
                // thing as nowhere: a history with twenty calls put the
                // details of the first row below the twentieth, and the
                // user was told to click a button that did nothing
                // they could see. The C++ expands the details inside
                // the row (`src/ui/HistoryPanel.qml`: an "Expanded
                // details panel" that fills the row's width).
                if expanded {
                    ui.add_space(2.0);
                    ui.indent("details", |ui| {
                        let number = if record.remote_number.is_empty() {
                            record.remote_uri.clone()
                        } else {
                            record.remote_number.clone()
                        };
                        ui.label(
                            egui::RichText::new(format!(
                                "{} — {}",
                                number,
                                clock.stamp(record.started_at)
                            ))
                            .strong(),
                        );
                        if !record.remote_uri.is_empty() {
                            ui.label(
                                egui::RichText::new(format!("URI: {}", record.remote_uri)).small(),
                            );
                        }
                        if !record.dialed_number.is_empty() {
                            ui.label(
                                egui::RichText::new(format!("Dialled: {}", record.dialed_number))
                                    .small(),
                            );
                        }
                        ui.label(
                            egui::RichText::new(format!(
                                "Started: {}",
                                clock.stamp(record.started_at)
                            ))
                            .small(),
                        );
                        if record.ended_at > 0 {
                            ui.label(
                                egui::RichText::new(format!(
                                    "Ended: {}",
                                    clock.stamp(record.ended_at)
                                ))
                                .small(),
                            );
                        }
                        if !record.hangup_cause.is_empty() {
                            ui.label(
                                egui::RichText::new(format!("Cause: {}", record.hangup_cause))
                                    .small(),
                            );
                        }
                    });
                    ui.add_space(2.0);
                }
            }

            // Nothing to put under the header. It goes under the
            // table, not over it: the header is the table's, and it
            // stays there.
            if rows.is_empty() {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(if self.history_query.trim().is_empty() {
                        "No calls yet."
                    } else {
                        "No calls match the search."
                    })
                    .color(egui::Color32::GRAY),
                );
            }
        });

        self.history_rows = rows;

        match action {
            Some(HistoryAction::Toggle(id)) => {
                self.history_expanded = if self.history_expanded == Some(id) {
                    None
                } else {
                    Some(id)
                };
            }
            Some(HistoryAction::Redial(record)) => self.redial(&record),
            None => {}
        }
    }

    /// The account page: the settings and the registration.
    fn draw_account(&mut self, ui: &mut egui::Ui) {
        let Some(account) = self.selected_account().cloned() else {
            ui.weak("Select an account, or add one.");
            return;
        };

        ui.horizontal(|ui| {
            let title = if account.label.is_empty() {
                "(unnamed account)".to_string()
            } else {
                account.label.clone()
            };
            ui.label(egui::RichText::new(title).strong().size(15.0));
            ui.weak(format!("id {}", account.id));
        });

        // The editor. The borrow of the draft ends with this block, so
        // the buttons below can take `&mut self`.
        {
            let draft = &mut self.edit_account;
            egui::Grid::new("account_editor")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    ui.label("Label");
                    ui.text_edit_singleline(&mut draft.label);
                    ui.end_row();

                    ui.label("Registrar");
                    ui.text_edit_singleline(&mut draft.registrar);
                    ui.end_row();

                    ui.label("User");
                    ui.text_edit_singleline(&mut draft.user);
                    ui.end_row();

                    ui.label("Password");
                    ui.add(egui::TextEdit::singleline(&mut draft.password).password(true));
                    ui.end_row();

                    ui.label("Transport");
                    egui::ComboBox::from_id_salt("edit_transport")
                        .selected_text(&draft.transport)
                        .show_ui(ui, |ui| {
                            for t in ["udp", "tcp", "tls"] {
                                ui.selectable_value(&mut draft.transport, t.to_string(), t);
                            }
                        });
                    ui.end_row();

                    ui.label("Domain");
                    ui.text_edit_singleline(&mut draft.domain);
                    ui.end_row();

                    ui.label("Outbound proxy");
                    ui.text_edit_singleline(&mut draft.outbound_proxy);
                    ui.end_row();

                    ui.label("Caller name header");
                    ui.text_edit_singleline(&mut draft.caller_id_header);
                    ui.end_row();

                    ui.label("Dialled number header");
                    ui.text_edit_singleline(&mut draft.dialed_number_header);
                    ui.end_row();
                });
        }

        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                self.save_selected();
            }
            if ui.button("Revert").clicked() {
                self.load_selected();
                self.status = "Reverted.".to_string();
            }
        });

        ui.separator();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Registration").strong());
            ui.label(self.registration_state(&account.id).summary());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Unregister").clicked() {
                    self.unregister_selected();
                }
                if ui.button("Register").clicked() {
                    self.register_selected();
                }
            });
        });

        ui.separator();
        egui::CollapsingHeader::new("Add another account").show(ui, |ui| {
            let draft = &mut self.new_account;
            ui.label("Label");
            ui.text_edit_singleline(&mut draft.label);
            ui.label("Registrar");
            ui.text_edit_singleline(&mut draft.registrar);
            ui.label("User");
            ui.text_edit_singleline(&mut draft.user);
            ui.label("Password");
            ui.add(egui::TextEdit::singleline(&mut draft.password).password(true));
            ui.horizontal(|ui| {
                ui.label("Transport");
                egui::ComboBox::from_id_salt("new_transport")
                    .selected_text(&draft.transport)
                    .show_ui(ui, |ui| {
                        for t in ["udp", "tcp", "tls"] {
                            ui.selectable_value(&mut draft.transport, t.to_string(), t);
                        }
                    });
            });
            if ui.button("Add").clicked() {
                self.add_account();
            }
        });

        ui.separator();
        egui::CollapsingHeader::new("Paths").show(ui, |ui| {
            ui.monospace(format!("config  {}", self.paths.config_file().display()));
            ui.monospace(format!("data    {}", self.paths.data_dir().display()));
            ui.monospace(format!("logs    {}", self.paths.log_dir().display()));
        });
    }

    /// The settings page: the codecs, the sound card and the test
    /// sound.
    ///
    /// These are the things that are not about one call. The call page
    /// had a codec picker, and it was in the wrong place: a codec is
    /// what the softphone speaks, not what one call is — and a list of
    /// three of them in the middle of the dialer is noise.
    fn draw_settings(&mut self, ui: &mut egui::Ui) {
        // The devices are read on the runtime (§3); ask for them the
        // first time this page is shown.
        self.load_devices();

        ui.label(egui::RichText::new("CODECS").strong().size(11.0));
        ui.separator();
        ui.weak("The offer names these, in this order. The peer picks one of them.");
        ui.add_space(4.0);

        let enabled = self.enabled_codecs();
        for codec in enabled.iter() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(codec.name()).strong());
                ui.weak(codec_note(*codec));
                if ui.small_button("▲").clicked() {
                    self.move_codec(*codec, true);
                }
                if ui.small_button("▼").clicked() {
                    self.move_codec(*codec, false);
                }
                if ui.small_button("×").clicked() {
                    self.remove_codec(*codec);
                }
            });
        }

        let disabled = self.disabled_codecs();
        if !disabled.is_empty() {
            ui.horizontal(|ui| {
                ui.label("Add a codec:");
                let mut choice = disabled[0];
                egui::ComboBox::from_id_salt("add_codec")
                    .selected_text(choice.name())
                    .width(100.0)
                    .show_ui(ui, |ui| {
                        for codec in disabled.iter() {
                            if ui
                                .selectable_label(choice == *codec, codec.name())
                                .clicked()
                            {
                                choice = *codec;
                            }
                        }
                    });
                if ui.button("Add").clicked() {
                    self.add_codec(choice);
                }
            });
        }

        ui.add_space(4.0);
        ui.weak("A PBX has to be told to allow G.722: allow=g722 on the PJSIP endpoint.");
        ui.add_space(10.0);

        ui.label(egui::RichText::new("SOUND CARD").strong().size(11.0));
        ui.separator();

        let inputs: Vec<String> = self
            .devices
            .iter()
            .filter(|device| device.is_input)
            .map(|device| device.name.clone())
            .collect();
        let outputs: Vec<String> = self
            .devices
            .iter()
            .filter(|device| device.is_output)
            .map(|device| device.name.clone())
            .collect();

        ui.horizontal(|ui| {
            ui.label("Microphone");
            let current = self.config.media.microphone.clone();
            egui::ComboBox::from_id_salt("microphone")
                .selected_text(device_label(&current))
                .width(260.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(current.is_empty(), "System default")
                        .clicked()
                    {
                        self.set_microphone(String::new());
                    }
                    for name in inputs.iter() {
                        if ui.selectable_label(current == *name, name).clicked() {
                            self.set_microphone(name.clone());
                        }
                    }
                });
        });

        ui.horizontal(|ui| {
            ui.label("Speaker");
            let current = self.config.media.speaker.clone();
            egui::ComboBox::from_id_salt("speaker")
                .selected_text(device_label(&current))
                .width(260.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(current.is_empty(), "System default")
                        .clicked()
                    {
                        self.set_speaker(String::new());
                    }
                    for name in outputs.iter() {
                        if ui.selectable_label(current == *name, name).clicked() {
                            self.set_speaker(name.clone());
                        }
                    }
                });
        });

        if self.devices.is_empty() && self.devices_loaded {
            ui.weak("This machine has no sound card that could be found.");
        }

        // A remote session is worth saying out loud: it is the one
        // case where the sound card is there and the microphone is not,
        // and the reason is a setting on the **client**.
        if self.remote_session {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("This is a remote session (RDP).")
                    .strong()
                    .color(egui::Color32::from_rgb(230, 162, 60)),
            );
            ui.weak(qsoftphone_media::REMOTE_SESSION_HINT);
            ui.weak(
                "The call still works: the peer hears silence, and this end still hears the peer.",
            );
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let label = match &self.sound_test {
                SoundTest::Playing => "Playing…",
                SoundTest::Failed(_) => "Test sound again",
                _ => "Test sound",
            };
            let playing = self.sound_test == SoundTest::Playing;
            if ui.add_enabled(!playing, egui::Button::new(label)).clicked() {
                self.play_test_sound();
            }
            if let SoundTest::Failed(why) = &self.sound_test {
                ui.weak(why.clone());
            }
        });
        ui.weak("A short tone through the speaker, to see that it works.");

        // The echo test: the microphone, through the speaker.
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let label = match &self.echo_test {
                EchoTest::Running => "Listening…",
                EchoTest::Done { .. } | EchoTest::Failed(_) => "Echo test again",
                _ => "Echo test",
            };
            let running = self.echo_test == EchoTest::Running;
            if ui.add_enabled(!running, egui::Button::new(label)).clicked() {
                self.play_echo_test();
            }
            match &self.echo_test {
                EchoTest::Failed(why) => {
                    ui.weak(why.clone());
                }
                EchoTest::Done { text, .. } => {
                    ui.weak(text.clone());
                }
                _ => {}
            }
        });
        ui.weak(
            "Your own voice, from the microphone, through the speaker. Wear headphones, \
             or the microphone hears the speaker and the test howls.",
        );
        ui.weak(
            "Speak during the test and watch the meter: if the bar does not move when \
             you talk, the microphone is not working.",
        );

        // The level meter, while the test runs.
        //
        // This is the answer to "the microphone does not work": if the
        // user speaks and the bar does not move, the microphone is not
        // working, and no amount of waiting will change that.
        if self.echo_test == EchoTest::Running {
            ui.add_space(4.0);
            draw_level_meter(ui, self.echo_level, self.echo_peak, 260.0, true);
            ui.weak(
                "Speak now. If the bar does not move, the microphone is muted or the \
                 wrong input is picked.",
            );
        }

        // The speaker the test used, and whether it is a headphone
        // jack — which is the commonest reason a test is silent.
        if let EchoTest::Done { speaker, .. } = &self.echo_test {
            if is_headphone(speaker) {
                ui.add_space(2.0);
                ui.weak(format!(
                    "The speaker is '{speaker}'. If nothing is plugged into it, there is no sound."
                ));
            }
        }
    }
}

/// The parts of the app the `--call` diagnostic drives.
///
/// The diagnostic is a call without a window: it registers, calls, and
/// says what the media did — which is the only way to find out what a
/// call carried when there is nobody at the machine to listen to it.
impl QSoftphoneApp {
    /// Start registering the selected account.
    pub fn start_registration(&mut self) {
        self.register_selected();
    }

    /// Whether the selected account is registered.
    pub fn is_registered(&self) -> bool {
        self.selected_account()
            .is_some_and(|account| self.registration_state(&account.id).is_registered())
    }

    /// Place a call to this number.
    pub fn call_now(&mut self, number: &str) {
        self.place_call_to(number);
    }

    /// Whether a call is up.
    pub fn call_is_up(&self) -> bool {
        self.calls().iter().any(|call| call.state == "connected")
    }

    /// The levels of the active call.
    ///
    /// The peer, what we send, and what the speaker was given: the
    /// three numbers that say where a silence is.
    pub fn levels_of_active(&self) -> Option<String> {
        let call = self.active_call()?;
        let status = self.calls.media_status(&call.account_id, &call.call_id)?;
        Some(format!(
            "peer={:.5} peer_peak={:.5} you={:.5} speaker={:.5}",
            status.incoming_level, status.incoming_peak, status.outgoing_peak, status.played_peak
        ))
    }

    /// What the media of the active call is doing.
    pub fn media_of_active(&self) -> Option<String> {
        let call = self.active_call()?;
        let status = self.calls.media_status(&call.account_id, &call.call_id)?;
        Some(format!(
            "codec={} pt={} port={} remote={:?} mic={:?} speaker={:?} problem={:?}",
            status.codec.name(),
            status.payload_type,
            status.port,
            status.remote,
            status.microphone,
            status.speaker,
            status.problem
        ))
    }

    /// Hang the active call up.
    pub fn hangup_now(&mut self) {
        self.hangup_call();
    }
}

/// The level of a signal, in decibels against full scale.
///
/// Below the floor it is not a number at all, and saying "-inf dB"
/// would be a way of dressing up "nothing" as a measurement.
fn decibels(level: f32) -> Option<f32> {
    if level <= 1e-6 {
        None
    } else {
        Some(20.0 * level.log10())
    }
}

/// A level, as the user reads it.
fn level_text(level: f32) -> String {
    match decibels(level) {
        Some(db) => format!("{db:.0} dB"),
        None => "silent".to_string(),
    }
}

/// The level below which there is nothing to hear: -40 dB.
///
/// A voice is well above this. A line that carries less carries room
/// noise, and saying so is a true thing to say.
const TOO_QUIET: f32 = 0.01;

/// Where the silence is, in a call.
///
/// The three numbers — what the peer sent, what the microphone picked
/// up and what the speaker was given — say which of the three things
/// went wrong. Saying "there is no sound" and stopping there is what
/// makes a call like this take an afternoon.
///
/// The peer is judged on the level **now** and not on the peak: a
/// line that carried a beep in the first second and room noise ever
/// since is quiet, and the peak would say the opposite.
fn silence_diagnosis(status: &qsoftphone_media::MediaStatus) -> Option<String> {
    if status.mic_muted {
        return Some("The microphone is muted: the peer hears silence.".to_string());
    }
    if status.speaker_muted {
        return Some("The speaker is muted.".to_string());
    }
    // The path is losing the audio. This is said before the levels,
    // because it is the reason the levels are what they are: a peer
    // whose frames are being dropped is not a quiet peer.
    //
    // And with G.722 it is worse than a dropout. That codec is ADPCM
    // and it keeps a state, so one lost frame leaves the decoder at
    // the other end out of step, and everything after it is noise.
    // G.711 loses a frame and carries on.
    let lost = status.peer_lost;
    let total = lost + status.peer_received;
    if total > 0 && lost * 10 >= total {
        let percent = lost * 100 / total;
        let mut sentence = format!(
            "The path is dropping the audio: {lost} of the peer's {total} frames never arrived \
             ({percent}%)."
        );
        if status.codec == qsoftphone_media::Codec::G722 {
            sentence.push_str(
                " G.722 keeps a state, so every lost frame leaves the peer's decoder out of \
                 step — G.711 would only drop out.",
            );
        }
        return Some(sentence);
    }
    if status.incoming_peak <= 1e-6 {
        return Some(
            "The peer is sending silence — there is nothing on the line to hear.".to_string(),
        );
    }
    if status.incoming_level < TOO_QUIET {
        return Some(
            "The peer is sending, but it is too quiet to hear — their microphone is muted, \
             too far away, or turned down."
                .to_string(),
        );
    }
    if status.played_peak <= 1e-6 {
        return Some(
            "The peer is sending audio, but the speaker is getting silence — the sound card is not playing it."
                .to_string(),
        );
    }
    if status.played_peak < TOO_QUIET {
        return Some(
            "The audio reached the speaker, but it is too quiet to hear — turn the speaker \
             volume up."
                .to_string(),
        );
    }
    if status.outgoing_peak <= 1e-6 {
        return Some(
            "The peer hears nothing: the microphone picked up nothing at all.".to_string(),
        );
    }
    if status.outgoing_peak < TOO_QUIET {
        return Some(
            "The microphone is picking up almost nothing — the peer will hear silence.".to_string(),
        );
    }
    None
}

/// The bar that shows how loud the microphone is.
///
/// The scale runs from **-60 dB to 0 dB**, because that is the range
/// a voice lives in: -60 dB is the floor of a quiet room, and 0 dB is
/// as loud as it can get without clipping.
///
/// `max_width` is how much of the row the bar may take: the settings
/// page gives it the whole width, and a call row has numbers to put
/// beside it.
///
/// `readout` says whether the number in decibels is painted next to
/// the bar. When it is, the number is given room in the layout: a
/// number painted outside the space the bar was given lands on top of
/// whatever comes next, and two pieces of text on top of each other
/// are two unreadable pieces of text.
fn draw_level_meter(ui: &mut egui::Ui, level: f32, peak: f32, max_width: f32, readout: bool) {
    let height = 14.0;
    let text = match decibels(level) {
        Some(db) => format!("{db:.0} dB"),
        None => "-inf dB".to_string(),
    };
    let font = egui::FontId::proportional(12.0);
    let colour = ui.visuals().text_color();

    // The number belongs to the meter, so it has to be given room in
    // the layout and not painted over whatever the meter does not
    // cover.
    let gap = 8.0;
    let text_width = if readout {
        ui.ctx()
            .fonts_mut(|f| f.layout_no_wrap(text.clone(), font.clone(), colour))
            .rect
            .width()
    } else {
        0.0
    };

    // The bar gets what is left of the row.
    let bar_width = (ui.available_width() - text_width - gap).clamp(24.0, max_width.max(24.0));
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(bar_width + text_width + gap, height),
        egui::Sense::hover(),
    );
    let bar = egui::Rect::from_min_size(rect.min, egui::vec2(bar_width, height));

    let to_fill = |value: f32| {
        let db = decibels(value).unwrap_or(-120.0);
        ((db + 60.0) / 60.0).clamp(0.0, 1.0)
    };

    let track = if ui.visuals().dark_mode { 45 } else { 200 };
    ui.painter()
        .rect_filled(bar, 2.0, egui::Color32::from_gray(track));

    let fill = to_fill(level);
    if fill > 0.0 {
        let filled =
            egui::Rect::from_min_size(bar.min, egui::vec2(bar.width() * fill, bar.height()));
        let colour = if decibels(level).unwrap_or(-120.0) > -6.0 {
            egui::Color32::from_rgb(231, 76, 60)
        } else {
            egui::Color32::from_rgb(61, 220, 151)
        };
        ui.painter().rect_filled(filled, 2.0, colour);
    }

    // The peak hold: where the loudest moment was.
    let hold = to_fill(peak);
    if hold > 0.0 {
        let x = bar.min.x + bar.width() * hold;
        ui.painter().line_segment(
            [egui::pos2(x, bar.min.y), egui::pos2(x, bar.max.y)],
            egui::Stroke::new(1.5, egui::Color32::from_gray(220)),
        );
    }

    if readout {
        ui.painter().text(
            rect.left_center() + egui::vec2(bar_width + gap, 0.0),
            egui::Align2::LEFT_CENTER,
            &text,
            font,
            colour,
        );
    }
}

/// Whether a device name is a headphone jack.
///
/// It is a guess from the name, and it is only used to say something
/// helpful: a headphone jack with nothing plugged into it is the
/// commonest reason a test is silent.
fn is_headphone(name: &str) -> bool {
    name.to_lowercase().contains("headphone")
}

/// What the echo test heard, in words.
///
/// The **device names** are in the text: a test that is quiet is usually
/// a test that ran on a device the user did not mean to use, and the
/// only way to know which device it was is to say so.
///
/// The **level** is in the text too, in decibels. A yes/no answer is
/// not enough: "the microphone was silent" and "the microphone heard a
/// whisper at -58 dB" are the same word and completely different
/// problems, and only the number tells them apart.
///
/// What the number cannot do is say *whose* voice it was: a quiet room
/// and a voice two metres away are at the same level. That is what the
/// **meter** is for — the user speaks, and watches it move.
fn echo_report_text(report: &qsoftphone_media::EchoReport) -> String {
    let devices = format!("{} into {}", report.microphone, report.speaker);
    if report.samples == 0 {
        return format!("The microphone sent nothing at all — it never ran ({devices}).");
    }
    let Some(db) = decibels(report.peak) else {
        return format!("The microphone was silent — nothing came in ({devices}).");
    };
    if db < -55.0 {
        return format!(
            "The microphone heard nothing: it stayed at {db:.0} dB ({devices}). \
             If you spoke, the microphone is muted or the wrong input is picked."
        );
    }
    format!("Something came in; the loudest moment was {db:.0} dB ({devices}).")
}

/// The widths of the history table's columns.
///
/// They are **fixed**, and that is the whole point of drawing the
/// table by hand instead of with a grid: the details of a row are
/// long — the full SIP URI, the exact timestamps — and a grid sizes
/// its columns by what is in them, so the details would widen the
/// table and then have nowhere to go but below the whole thing, where
/// a long history puts them out of sight. With fixed widths the
/// details go **under the row they belong to**.
const COL_DIR: f32 = 24.0;
const COL_NUMBER: f32 = 100.0;
const COL_DURATION: f32 = 68.0;
const COL_TIME: f32 = 60.0;
const COL_CODE: f32 = 44.0;
const COL_STATUS: f32 = 58.0;

/// What the row's buttons need: "Redial" and "Details" side by side.
///
/// Measured: the two buttons are 116 points wide together, and the
/// column has to hold them both.
const COL_ACTIONS: f32 = 124.0;

/// The gap between two columns of the history table.
const COL_GAP: f32 = 8.0;

/// The height of a row of the history table.
///
/// One line, or two when the call had a name under the number.
const ROW_HEIGHT: f32 = 18.0;
const ROW_TWO_LINES: f32 = 32.0;

/// One cell of the history table.
///
/// The cell is placed at a rect the caller computes, so the columns
/// line up and a long value cannot widen the table. egui's own grid
/// cannot do this: it sizes its columns by what is in them.
fn table_cell<R>(ui: &mut egui::Ui, rect: egui::Rect, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        add,
    )
    .inner
}

/// The size of a keypad button.
///
/// The keypad is in the middle of the page now, so the buttons can be
/// the size a keypad's buttons are.
const KEYPAD_BUTTON: egui::Vec2 = egui::vec2(46.0, 32.0);

/// How long the echo test listens.
///
/// Long enough to say a word in, short enough not to be a nuisance.
const ECHO_SECONDS: f64 = 5.0;

/// How long the echo test may take before it is given up on.
///
/// The test opens the sound card, and a sound card can fail to answer:
/// a device that was unplugged, a driver that is wedged. The test runs
/// on its own thread, so the window stays alive — but the button must
/// not sit at "Listening…" for ever.
const ECHO_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// The colour the history list shows a result in.
///
/// The colour is decided in the core crate, next to the label, so the
/// legend above the list and the rows below it cannot drift apart.
fn result_color(result: CallResult) -> egui::Color32 {
    let (r, g, b) = result.color();
    egui::Color32::from_rgb(r, g, b)
}

/// The colour the history list shows a direction's mark in.
///
/// Like the result colour, it is decided in the core crate — the mark
/// and its colour belong together, and the legend draws the same mark
/// in the same colour.
fn direction_color(direction: HistoryDirection) -> egui::Color32 {
    let (r, g, b) = direction.color();
    egui::Color32::from_rgb(r, g, b)
}

/// The mark that says which way a call was going.
///
/// It is **drawn**, not typed. The app's font has no arrows in it, and
/// a character the font does not have is a little empty box — which is
/// exactly what the history's direction column showed before this was
/// drawn by hand.
///
/// The character is still there, in the accessibility tree, so a screen
/// reader says "rightwards arrow" and not nothing at all; it is only
/// not painted.
fn direction_mark(ui: &mut egui::Ui, direction: HistoryDirection) {
    let text = egui::RichText::new(direction.arrow()).color(egui::Color32::TRANSPARENT);
    let rect = ui.add(egui::Label::new(text)).rect;
    let side = rect.height().min(11.0);
    let mark = egui::Rect::from_center_size(rect.center(), egui::vec2(side, side));
    let color = direction_color(direction);
    let painter = ui.painter();

    match direction {
        HistoryDirection::Outgoing => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(mark.left() + 1.0, mark.top()),
                    egui::pos2(mark.right(), mark.center().y),
                    egui::pos2(mark.left() + 1.0, mark.bottom()),
                ],
                color,
                egui::Stroke::NONE,
            ));
        }
        HistoryDirection::Incoming => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(mark.right() - 1.0, mark.top()),
                    egui::pos2(mark.left(), mark.center().y),
                    egui::pos2(mark.right() - 1.0, mark.bottom()),
                ],
                color,
                egui::Stroke::NONE,
            ));
        }
        HistoryDirection::Missed => {
            // A cross, the way the C++ marks a missed call.
            let stroke = egui::Stroke::new(1.8, color);
            let inset = 2.0;
            painter.line_segment(
                [
                    egui::pos2(mark.left() + inset, mark.top() + inset),
                    egui::pos2(mark.right() - inset, mark.bottom() - inset),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(mark.right() - inset, mark.top() + inset),
                    egui::pos2(mark.left() + inset, mark.bottom() - inset),
                ],
                stroke,
            );
        }
    }
}

/// What the codec is, for the settings page.
fn codec_note(codec: Codec) -> &'static str {
    match codec {
        Codec::Pcmu => "G.711 µ-law, 8 kHz",
        Codec::Pcma => "G.711 A-law, 8 kHz",
        Codec::G722 => "G.722, 16 kHz wideband",
    }
}

/// What the device picker shows for a device name.
fn device_label(name: &str) -> String {
    if name.trim().is_empty() {
        "System default".to_string()
    } else {
        name.to_string()
    }
}

impl eframe::App for QSoftphoneApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

/// How often the app looks at itself when nothing else is happening.
const POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// How often the app looks at itself while the echo test is running.
///
/// The level meter has to move, and a tenth of a second is too slow for
/// a bar to look like a bar.
const LEVEL_POLL: std::time::Duration = std::time::Duration::from_millis(30);

/// When the process started.
///
/// The first frame says how long it took to get there.
static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Say when the process started.
///
/// The first thing [`main`] does, so that the time to the first frame
/// includes everything: the database, the settings, the font and the
/// graphics device.
pub fn mark_started() {
    let _ = STARTED.set(std::time::Instant::now());
}

/// The media settings, read out of the settings file.
///
/// The settings file keeps the codecs as names — it is a plain JSON
/// document the user may edit by hand — and the media plane wants
/// them as its own type.
fn media_config_of(config: &AppConfig) -> qsoftphone_media::MediaConfig {
    let codecs: Vec<Codec> = config
        .media
        .codecs
        .iter()
        .filter_map(|name| Codec::from_name(name))
        .collect();

    // An empty or unusable list is not a reason to have no codec: the
    // defaults speak all three.
    let codecs = if codecs.is_empty() {
        Codec::ALL.to_vec()
    } else {
        codecs
    };

    qsoftphone_media::MediaConfig {
        codecs,
        microphone: Some(config.media.microphone.clone()).filter(|n| !n.trim().is_empty()),
        speaker: Some(config.media.speaker.clone()).filter(|n| !n.trim().is_empty()),
        microphone_gain: config.audio.microphone_volume,
        speaker_gain: config.audio.speaker_volume,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qsoftphone_core::model::call_event::CallEvent;
    use qsoftphone_core::storage::Db;
    use qsoftphone_core::storage::history::HistoryStore;

    /// An app with one account, and the bus its call events arrive on.
    fn build_app(handle: tokio::runtime::Handle) -> (QSoftphoneApp, CallBus) {
        build_app_with(Db::open_in_memory().expect("db"), handle)
    }

    /// An app with one account, over a given database.
    fn build_app_with(db: Db, handle: tokio::runtime::Handle) -> (QSoftphoneApp, CallBus) {
        let paths = AppPaths::under(std::env::temp_dir().join("qsoftphone-app-ui-test"));
        let sessions = SessionRegistry::new();
        let (bus, receiver) = CallBus::new();

        let mut app = QSoftphoneApp::new(
            paths,
            db,
            AppConfig::default(),
            handle,
            sessions,
            bus.clone(),
            receiver,
        );

        // The store is empty, so the account is put in directly. The
        // id is what the call events are keyed by.
        let mut account = Account::new("Office", "sip.example.com:5060", "104", "pw", "udp");
        account.id = "acct-ui-1".to_string();
        app.accounts.push(account);
        app.selected = Some(0);
        app.load_selected();

        (app, bus)
    }

    /// An incoming call, as the SIP layer would report it.
    ///
    /// The event carries the peer's **URI** — that is the C++'s
    /// `CallEvent` shape, and the history row needs the full URI.
    fn incoming_event() -> CallEvent {
        let mut event = CallEvent::new(CallEventType::IncomingCall);
        event.call_id = "call-1".to_string();
        event.call_direction = Some(Direction::Incoming);
        event.call_state = "ringing".to_string();
        event.caller = "sip:6666@sip.example.com".to_string();
        event.caller_id = "Alice".to_string();
        event.dialed_number = "6666".to_string();
        event
    }

    /// Run one frame of the UI, headlessly.
    ///
    /// The window is given a size: without one, egui lays the
    /// central panel out with no height at all, and everything below
    /// the first row is culled.
    fn frame(app: &mut QSoftphoneApp, ctx: &egui::Context) -> egui::FullOutput {
        frame_with(app, ctx, egui::RawInput::default())
    }

    /// Run one frame of the UI with a given input.
    fn frame_with(
        app: &mut QSoftphoneApp,
        ctx: &egui::Context,
        mut input: egui::RawInput,
    ) -> egui::FullOutput {
        // The window is the size the app opens at: without a screen
        // rect, egui lays the central panel out with no height at
        // all, and everything below the first row is culled.
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1100.0, 700.0),
        ));
        let mut output = ctx.run_ui(input, |ui| app.draw(ui));
        output.textures_delta.clear();
        output
    }

    /// Where a button with this label is on the page.
    ///
    /// The accessibility tree is what a screen reader gets, so it is
    /// also the one place that says where a button is.
    fn button_at(output: &egui::FullOutput, label: &str) -> Option<egui::Rect> {
        let update = output.platform_output.accesskit_update.as_ref()?;
        update.nodes.iter().find_map(|(_, node)| {
            if node.label()? != label {
                return None;
            }
            let bounds = node.bounds()?;
            Some(egui::Rect::from_min_max(
                egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
            ))
        })
    }

    /// Where a piece of text is on the page.
    ///
    /// A plain label is a **value** in the accessibility tree and a
    /// button is a **name**, so both are looked for.
    fn text_at(output: &egui::FullOutput, text: &str) -> Option<egui::Rect> {
        let update = output.platform_output.accesskit_update.as_ref()?;
        update.nodes.iter().find_map(|(_, node)| {
            if node.value() != Some(text) && node.label() != Some(text) {
                return None;
            }
            let bounds = node.bounds()?;
            Some(egui::Rect::from_min_max(
                egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
            ))
        })
    }

    /// Where a piece of text that **begins with** this is on the page.
    ///
    /// The history's details carry a timestamp, so the whole line is
    /// not known before the clock has been asked; the beginning of it
    /// is enough to find the line.
    fn text_starting_at(output: &egui::FullOutput, prefix: &str) -> Option<egui::Rect> {
        let update = output.platform_output.accesskit_update.as_ref()?;
        update.nodes.iter().find_map(|(_, node)| {
            let found = node.value().is_some_and(|v| v.starts_with(prefix))
                || node.label().is_some_and(|l| l.starts_with(prefix));
            if !found {
                return None;
            }
            let bounds = node.bounds()?;
            Some(egui::Rect::from_min_max(
                egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
            ))
        })
    }

    /// Click a button, by where it is.
    fn click_at(app: &mut QSoftphoneApp, ctx: &egui::Context, at: egui::Rect) {
        let pos = at.center();
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::PointerMoved(pos));
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        frame_with(app, ctx, input);
    }

    #[tokio::test]
    async fn a_frame_draws_without_a_panic() {
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        let output = frame(&mut app, &ctx);
        // egui laid the window out and drew something.
        assert!(!output.shapes.is_empty(), "the frame drew nothing");
        assert!(
            output.platform_output.commands.is_empty(),
            "the frame asked the backend to do something"
        );
    }

    #[tokio::test]
    async fn the_account_page_draws_too() {
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        app.view = View::Account;
        let output = frame(&mut app, &ctx);
        assert!(!output.shapes.is_empty(), "the frame drew nothing");
    }

    #[tokio::test]
    async fn an_incoming_call_opens_the_modal() {
        let (mut app, bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        assert!(app.incoming_call().is_none());
        bus.publish("acct-ui-1", incoming_event());
        frame(&mut app, &ctx);

        let call = app.incoming_call().expect("the modal is not up");
        assert_eq!(call.peer, "6666");
        assert_eq!(call.caller_id, "Alice");
        assert_eq!(call.dialed_number, "6666");
    }

    #[tokio::test]
    async fn the_incoming_call_modal_cannot_be_dismissed() {
        // §5.7: the modal is not closable. No close button, no
        // click-outside-to-dismiss, no Esc — only Answer and Decline
        // end it.
        //
        // `egui::Modal` never closes itself: `should_close()` is an
        // opt-in the caller has to ask for, and this app never asks.
        let (mut app, bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        bus.publish("acct-ui-1", incoming_event());
        frame(&mut app, &ctx);
        assert!(app.incoming_call().is_some());

        // Esc, and a click outside the modal.
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        input.events.push(egui::Event::PointerButton {
            pos: egui::pos2(2.0, 2.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        frame_with(&mut app, &ctx, input);
        assert!(
            app.incoming_call().is_some(),
            "the modal must not be dismissable by the user"
        );
    }

    #[tokio::test]
    async fn answering_the_call_closes_the_modal() {
        let (mut app, bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        bus.publish("acct-ui-1", incoming_event());
        frame(&mut app, &ctx);
        assert!(app.incoming_call().is_some());

        // The SIP layer reports the call as answered.
        let mut answered = CallEvent::new(CallEventType::CallAnswered);
        answered.call_id = "call-1".to_string();
        answered.call_direction = Some(Direction::Incoming);
        answered.call_state = "connected".to_string();
        bus.publish("acct-ui-1", answered);
        frame(&mut app, &ctx);

        assert!(app.incoming_call().is_none(), "the modal is still up");
    }

    #[tokio::test]
    async fn an_ended_call_closes_the_modal() {
        let (mut app, bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        bus.publish("acct-ui-1", incoming_event());
        frame(&mut app, &ctx);
        assert!(app.incoming_call().is_some());

        let mut ended = CallEvent::new(CallEventType::CallEnded);
        ended.call_id = "call-1".to_string();
        ended.call_direction = Some(Direction::Incoming);
        ended.call_state = "ended".to_string();
        ended.sip_code = 603;
        bus.publish("acct-ui-1", ended);
        frame(&mut app, &ctx);

        assert!(app.incoming_call().is_none(), "the modal is still up");
    }

    #[tokio::test]
    async fn the_call_panel_follows_the_newest_call() {
        let (mut app, bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        let mut outgoing = CallEvent::new(CallEventType::OutgoingCallInitiated);
        outgoing.call_id = "call-2".to_string();
        outgoing.call_direction = Some(Direction::Outgoing);
        outgoing.call_state = "initiating".to_string();
        outgoing.callee = "100".to_string();
        bus.publish("acct-ui-1", outgoing);
        frame(&mut app, &ctx);

        let call = app.active_call().expect("no call is shown");
        assert_eq!(call.call_id, "call-2");
        assert_eq!(call.peer, "100");
    }

    #[tokio::test]
    async fn a_call_from_an_unregistered_account_is_refused() {
        // The dialer is only live for a registered account, and the
        // call layer refuses it as well.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        app.dial_number = "100".to_string();
        app.place_call();

        assert!(
            app.status.contains("not registered"),
            "the app must say why: {}",
            app.status
        );
    }

    #[tokio::test]
    async fn the_window_opens_on_the_call_page() {
        // The account settings are a page, not the front of the
        // window: a softphone is for calling.
        let (app, _bus) = build_app(tokio::runtime::Handle::current());
        assert_eq!(app.view, View::Call);
    }

    #[tokio::test]
    async fn an_incoming_call_switches_to_the_call_page() {
        let (mut app, bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        // The user is looking at the account settings.
        app.view = View::Account;

        bus.publish("acct-ui-1", incoming_event());
        frame(&mut app, &ctx);

        assert_eq!(app.view, View::Call, "the call page must come forward");
    }

    #[tokio::test]
    async fn the_calls_are_listed_in_the_order_they_arrived() {
        let (mut app, bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        let mut first = CallEvent::new(CallEventType::OutgoingCallInitiated);
        first.call_id = "call-1".to_string();
        first.call_direction = Some(Direction::Outgoing);
        first.call_state = "initiating".to_string();
        first.callee = "111".to_string();

        let mut second = CallEvent::new(CallEventType::OutgoingCallInitiated);
        second.call_id = "call-2".to_string();
        second.call_direction = Some(Direction::Outgoing);
        second.call_state = "initiating".to_string();
        second.callee = "222".to_string();

        bus.publish("acct-ui-1", first);
        bus.publish("acct-ui-1", second);
        frame(&mut app, &ctx);

        let calls = app.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].call_id, "call-1");
        assert_eq!(calls[1].call_id, "call-2");
    }

    #[tokio::test]
    async fn the_newest_call_is_the_one_the_panel_shows() {
        let (mut app, bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        let mut first = CallEvent::new(CallEventType::OutgoingCallInitiated);
        first.call_id = "call-1".to_string();
        first.call_direction = Some(Direction::Outgoing);
        first.call_state = "initiating".to_string();
        first.callee = "111".to_string();

        let mut second = CallEvent::new(CallEventType::OutgoingCallInitiated);
        second.call_id = "call-2".to_string();
        second.call_direction = Some(Direction::Outgoing);
        second.call_state = "initiating".to_string();
        second.callee = "222".to_string();

        bus.publish("acct-ui-1", first);
        bus.publish("acct-ui-1", second);
        frame(&mut app, &ctx);

        let active = app.active_call().expect("no call is shown");
        assert_eq!(active.call_id, "call-1", "the first call stays active");
    }

    // -- M3: the history --

    /// Publish a call that starts, is answered and ends.
    async fn run_a_call(bus: &CallBus, call_id: &str, answered: bool) {
        let mut incoming = incoming_event();
        incoming.call_id = call_id.to_string();
        bus.publish("acct-ui-1", incoming);

        if answered {
            let mut ok = CallEvent::new(CallEventType::CallAnswered);
            ok.call_id = call_id.to_string();
            ok.call_direction = Some(Direction::Incoming);
            ok.call_state = "connected".to_string();
            bus.publish("acct-ui-1", ok);
        }

        let mut ended = CallEvent::new(CallEventType::CallEnded);
        ended.call_id = call_id.to_string();
        ended.call_direction = Some(Direction::Incoming);
        ended.call_state = "ended".to_string();
        ended.sip_code = if answered { 200 } else { 0 };
        ended.hangup_cause = "Normal".to_string();
        bus.publish("acct-ui-1", ended);
    }

    /// Wait for the history to answer, and drain it.
    async fn wait_for_history(app: &mut QSoftphoneApp, ctx: &egui::Context, wanted: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.history_rows.len() < wanted && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            frame(app, ctx);
        }
    }

    #[tokio::test]
    async fn a_finished_call_lands_in_the_history() {
        let (mut app, bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        run_a_call(&bus, "call-1", true).await;
        frame(&mut app, &ctx);
        wait_for_history(&mut app, &ctx, 1).await;

        assert_eq!(app.history_rows.len(), 1, "the call was not stored");
        let row = &app.history_rows[0];
        // §5.3: the remote party is the caller, never our own
        // extension.
        assert_eq!(row.remote_number, "6666");
        assert_eq!(row.remote_uri, "sip:6666@sip.example.com");
        assert_eq!(row.direction, HistoryDirection::Incoming);
        assert!(row.answered);
        assert_eq!(row.sip_code, 200);
        assert_eq!(row.dialed_number, "6666");
    }

    #[tokio::test]
    async fn an_unanswered_incoming_call_is_stored_as_missed() {
        let (mut app, bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        run_a_call(&bus, "call-1", false).await;
        frame(&mut app, &ctx);
        wait_for_history(&mut app, &ctx, 1).await;

        assert_eq!(app.history_rows.len(), 1, "the call was not stored");
        let row = &app.history_rows[0];
        assert_eq!(row.direction, HistoryDirection::Missed);
        assert!(!row.answered);
    }

    #[tokio::test]
    async fn the_history_page_draws() {
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        app.view = View::History;
        let output = frame(&mut app, &ctx);
        assert!(!output.shapes.is_empty(), "the frame drew nothing");
    }

    #[tokio::test]
    async fn the_history_row_offers_a_redial_without_being_expanded() {
        // The redial is what people come to the history for, so it is
        // in the row: making the user expand a row to find it is a step
        // too many.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();

        let record = CallRecord::from_incoming(
            "acct-ui-1",
            "call-in",
            "sip:6661@sip.example.com",
            "104",
            "",
            1_000,
            2_000,
            200,
            "Normal",
            true,
        );
        app.history_rows = vec![record.clone()];
        app.view = View::History;

        // The row is not expanded.
        assert!(app.history_expanded.is_none());

        let output = frame(&mut app, &ctx);
        let button = button_at(&output, "Redial").expect("the row has no redial");

        // Clicking it dials the number back.
        click_at(&mut app, &ctx, button);

        assert_eq!(app.dial_number, "6661", "the redial did not happen");
    }

    #[tokio::test]
    async fn the_history_is_a_table_with_a_header() {
        // The rows are read down the columns, and a row of marks with
        // nothing to say what the columns are is not a table.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();

        app.history_rows = vec![CallRecord::from_incoming(
            "acct-ui-1",
            "call-in",
            "sip:6661@sip.example.com",
            "104",
            "",
            1_000,
            2_000,
            200,
            "Normal",
            true,
        )];
        app.view = View::History;
        let output = frame(&mut app, &ctx);

        let mut headers = Vec::new();
        for title in ["dir", "Number", "Duration", "Time", "code", "status"] {
            let rect = text_at(&output, title)
                .unwrap_or_else(|| panic!("the table has no {title:?} column"));
            headers.push(rect);
        }

        // The header is a row: the columns are side by side, at the
        // same height, in the order they were asked for.
        let first = headers[0];
        for (index, rect) in headers.iter().enumerate().skip(1) {
            assert!(
                rect.min.y > first.min.y - 2.0 && rect.min.y < first.min.y + 2.0,
                "the {index} column is not on the header row"
            );
            assert!(
                rect.min.x > headers[index - 1].min.x,
                "the {index} column is to the left of the one before it"
            );
        }

        // And the row is under the header, in the column it belongs
        // to: the number is between "dir" and "Duration", and below
        // the header.
        let number = text_at(&output, "6661").expect("the row has no number");
        assert!(
            number.min.y > first.max.y,
            "the row is not under the header: {number:?} against {first:?}"
        );
        assert!(
            number.min.x >= headers[1].min.x && number.max.x <= headers[2].min.x,
            "the number is not in the Number column: {number:?}"
        );
    }

    #[tokio::test]
    async fn the_details_of_a_row_are_under_that_row() {
        // The bug the user reported: the row said "Hide", so it was
        // expanded, and the details were nowhere to be seen. They were
        // drawn under the **whole table**, and a history with twenty
        // calls put the details of the first row below the twentieth.
        //
        // The C++ expands the details inside the row
        // (`src/ui/HistoryPanel.qml`: an "Expanded details panel" that
        // fills the row's width), and so does this now: the details of
        // the first row are between the first row and the second.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();

        let mut rows = Vec::new();
        for (index, number) in ["6661", "6662", "6663"].into_iter().enumerate() {
            let mut record = CallRecord::from_incoming(
                "acct-ui-1",
                format!("call-{number}"),
                format!("sip:{number}@sip.example.com"),
                number,
                "",
                1_000 + index as i64 * 1_000,
                2_000 + index as i64 * 1_000,
                200,
                "Normal",
                true,
            );
            record.id = index as i64 + 1;
            rows.push(record);
        }
        app.history_rows = rows;
        app.history_expanded = Some(1);
        app.view = View::History;

        let output = frame(&mut app, &ctx);

        let first = text_at(&output, "6661").expect("the first row has no number");
        let second = text_at(&output, "6662").expect("the second row has no number");
        let details =
            text_starting_at(&output, "Started: ").expect("the expanded row shows no details");

        assert!(
            details.min.y > first.min.y,
            "the details are not under their own row: {details:?} against {first:?}"
        );
        assert!(
            details.max.y < second.min.y,
            "the details are under the next row, not under their own: {details:?} against {second:?}"
        );
    }

    #[test]
    fn the_app_says_where_the_silence_is() {
        // "There is no sound" is not a diagnosis, and a call has three
        // places a silence can be. The numbers say which one it is.
        let quiet = media_status(0.0, 0.0, 0.0);
        assert!(
            silence_diagnosis(&quiet)
                .expect("a quiet call says something")
                .contains("peer is sending silence"),
            "the peer was silent and the app did not say so"
        );

        let blocked = media_status(0.18, 0.0, 0.02);
        assert!(
            silence_diagnosis(&blocked)
                .expect("a call says something")
                .contains("the speaker is getting silence"),
            "the audio never reached the speaker and the app did not say so"
        );

        let mute = media_status(0.18, 0.14, 0.0);
        assert!(
            silence_diagnosis(&mute)
                .expect("a call says something")
                .contains("peer hears nothing"),
            "the microphone picked up nothing and the app did not say so"
        );

        let muted = qsoftphone_media::MediaStatus {
            mic_muted: true,
            ..media_status(0.18, 0.14, 0.02)
        };
        assert!(
            silence_diagnosis(&muted)
                .expect("a call says something")
                .contains("muted"),
            "the microphone was muted and the app did not say so"
        );

        // A call in which everything works says nothing at all.
        let good = media_status(0.18, 0.14, 0.02);
        assert!(
            silence_diagnosis(&good).is_none(),
            "a working call complained: {:?}",
            silence_diagnosis(&good)
        );
    }

    #[test]
    fn a_level_is_read_in_decibels() {
        assert_eq!(level_text(0.0), "silent");
        assert_eq!(level_text(0.1836), "-15 dB");
        assert_eq!(level_text(1.0), "0 dB");
    }

    /// A media status with the levels the test cares about.
    fn media_status(incoming: f32, played: f32, outgoing: f32) -> qsoftphone_media::MediaStatus {
        media_status_live(incoming, incoming, played, outgoing)
    }

    /// A media status where the peer's loudest moment and the peer's
    /// level right now are not the same thing.
    ///
    /// That is the shape of a line that carried a beep in the first
    /// second and room noise ever since.
    fn media_status_live(
        incoming_peak: f32,
        incoming_level: f32,
        played: f32,
        outgoing: f32,
    ) -> qsoftphone_media::MediaStatus {
        qsoftphone_media::MediaStatus {
            codec: qsoftphone_media::Codec::G722,
            payload_type: 9,
            port: 4000,
            remote: Some("10.0.0.1:4000".to_string()),
            running: true,
            microphone: Some("Microphone".to_string()),
            speaker: Some("Speaker".to_string()),
            problem: None,
            mic_muted: false,
            speaker_muted: false,
            incoming_peak,
            incoming_level,
            outgoing_peak: outgoing,
            played_peak: played,
            peer_lost: 0,
            peer_received: 0,
        }
    }

    #[tokio::test]
    async fn the_button_that_opens_the_details_is_on_the_page() {
        // The table is measured against the clip rectangle and not
        // against the available width: inside the scroll area the
        // available width runs past the edge of the window — the
        // panel's own margin is not clipped away — so a table sized
        // against it put the button that opens the details outside the
        // window, where it cannot be clicked.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();

        app.history_rows = vec![CallRecord::from_incoming(
            "acct-ui-1",
            "call-in",
            "sip:6661@sip.example.com",
            "104",
            "",
            1_000,
            2_000,
            200,
            "Normal",
            true,
        )];
        app.view = View::History;

        // A narrow window — the size the user has.
        let width = 716.0;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(width, 924.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| app.draw(ui));
        output.textures_delta.clear();

        let details = button_at(&output, "Details").expect("the row has no details button");
        assert!(
            details.max.x <= width,
            "the details button is outside the window: {details:?}"
        );
    }

    #[tokio::test]
    async fn the_history_table_has_a_header_when_there_is_nothing_to_put_under_it() {
        // A table that only grows a header once it has rows is a list
        // that occasionally grows a header.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();

        app.view = View::History;
        let output = frame(&mut app, &ctx);

        for title in ["dir", "Number", "Duration", "Time", "code", "status"] {
            assert!(
                text_at(&output, title).is_some(),
                "the empty table has no {title:?} column"
            );
        }
        let text = screen_text(&output);
        assert!(
            text.contains("No calls yet."),
            "the empty table does not say it is empty:\n{text}"
        );
    }

    #[tokio::test]
    async fn the_history_says_when_the_call_started_and_when_it_ended() {
        // The bug the user reported: "Started" and "Ended" always read
        // the same. A call that lasted twenty seconds starts and ends
        // in the same minute, so the timestamps have to carry the
        // seconds — otherwise the history looks like it does not know
        // when the call ended.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();

        let started = 1_000_000_000_000;
        let ended = started + 22_000;
        let mut record = CallRecord::from_incoming(
            "acct-ui-1",
            "call-in",
            "sip:559@193.178.169.164:5161",
            "559",
            "",
            started,
            ended,
            200,
            "OK",
            true,
        );
        record.id = 1;
        app.history_rows = vec![record];
        app.history_expanded = Some(1);
        app.view = View::History;

        let output = frame(&mut app, &ctx);
        let text = screen_text(&output);

        let start = text
            .lines()
            .find_map(|line| line.strip_prefix("Started: "))
            .unwrap_or_else(|| panic!("the details have no start:\n{text}"));
        let stop = text
            .lines()
            .find_map(|line| line.strip_prefix("Ended: "))
            .unwrap_or_else(|| panic!("the details have no end:\n{text}"));

        assert_ne!(start, stop, "the start and the end read the same: {start}");
        // And the seconds are in there — that is what tells them
        // apart.
        assert!(
            start.matches(':').count() == 2,
            "the timestamp has no seconds: {start}"
        );
    }

    #[tokio::test]
    async fn the_keypad_is_only_on_the_call_page() {
        // The C++ has one keypad, always on the dialer page. So is
        // this one.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();

        app.view = View::Call;
        let output = frame(&mut app, &ctx);
        for digit in ["1", "2", "3", "4", "5", "6", "7", "8", "9", "*", "0", "#"] {
            assert!(
                button_at(&output, digit).is_some(),
                "the keypad has no {digit:?}"
            );
        }

        // And it is only on the call page.
        app.view = View::History;
        let output = frame(&mut app, &ctx);
        assert!(
            button_at(&output, "5").is_none(),
            "the keypad is on the history page as well"
        );
    }

    #[tokio::test]
    async fn the_keypad_dials_when_the_app_is_idle_and_sends_tones_in_a_call() {
        // The C++ has one keypad that does both jobs: "it dials
        // numbers when idle and sends DTMF tones during the call"
        // (`src/ui/Dialer.qml`). This is the same keypad.
        let (mut app, bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();

        // Idle: the key dials the number.
        app.view = View::Call;
        let output = frame(&mut app, &ctx);
        let five = button_at(&output, "5").expect("the keypad has no 5");
        click_at(&mut app, &ctx, five);
        assert_eq!(app.dial_number, "5", "the key did not dial the number");

        // In a call: the same key sends a tone and does not touch the
        // number. The tone cannot be sent here — there is no stack
        // behind the test — but the branch is the one that matters,
        // and it says so.
        let mut ok = CallEvent::new(CallEventType::CallAnswered);
        ok.call_id = "call-1".to_string();
        ok.call_direction = Some(Direction::Incoming);
        ok.call_state = "connected".to_string();
        bus.publish("acct-ui-1", ok);
        frame(&mut app, &ctx);

        let output = frame(&mut app, &ctx);
        let five = button_at(&output, "5").expect("the keypad has no 5");
        click_at(&mut app, &ctx, five);

        assert_eq!(
            app.dial_number, "5",
            "the key dialled the number during the call"
        );
        assert!(
            app.status.starts_with("Could not send the digit"),
            "the key did not send a tone during the call: {:?}",
            app.status
        );
    }

    #[tokio::test]
    async fn the_keypad_is_in_the_middle_of_the_call_page() {
        // "Why is it not in the middle of the window?" — it is now
        // between the number card and the calls, in the middle of the
        // page, as the C++ has it.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();

        app.view = View::Call;
        let output = frame(&mut app, &ctx);

        let five = button_at(&output, "5").expect("the keypad has no 5");
        let number = text_at(&output, "Number").expect("the page has no number card");
        let calls = text_at(&output, "CALLS").expect("the page has no call list");

        // Between the number and the calls.
        assert!(
            five.min.y > number.max.y,
            "the keypad is above the number: {five:?} against {number:?}"
        );
        assert!(
            five.min.y < calls.min.y,
            "the keypad is below the calls: {five:?} against {calls:?}"
        );

        // And in the middle of the page, not in a corner.
        //
        // The call list starts at the left edge of the page's content,
        // so the middle of the content is halfway between it and the
        // right edge of the window.
        let content_left = calls.min.x;
        let content_middle = (content_left + 1100.0) / 2.0;
        assert!(
            (five.center().x - content_middle).abs() < 25.0,
            "the keypad is not in the middle: {five:?} against {content_middle}"
        );
    }

    #[tokio::test]
    async fn the_app_asks_to_be_drawn_again() {
        // egui only draws when something asks it to, and everything
        // that changes in this app arrives from outside egui: the
        // registration, the calls, the media, the history. Without
        // this the window sits on the last picture until the user
        // happens to move the mouse.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        frame(&mut app, &ctx);

        assert!(
            ctx.has_requested_repaint(),
            "the app does not ask to be drawn again: the window would sit on a stale picture"
        );
    }

    #[tokio::test]
    async fn the_echo_test_will_not_run_during_a_call() {
        // The sound card is a shared resource: a second client on one
        // endpoint is what makes cpal report an overrun and the audio
        // break up.
        let (mut app, bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        run_a_call(&bus, "call-1", true).await;
        frame(&mut app, &ctx);

        app.play_echo_test();

        assert!(
            matches!(app.echo_test, EchoTest::Failed(_)),
            "the echo test ran during a call: {:?}",
            app.echo_test
        );
    }

    #[tokio::test]
    async fn the_echo_test_is_given_up_on_if_the_sound_card_does_not_answer() {
        // The test runs on its own thread, so a sound card that never
        // answers cannot freeze the window — but the button must not sit
        // at "Listening…" for ever.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        app.echo_test = EchoTest::Running;
        app.echo_started = Some(std::time::Instant::now());
        frame(&mut app, &ctx);
        assert!(
            matches!(app.echo_test, EchoTest::Running),
            "the echo test was given up on before its time was up"
        );

        app.echo_started =
            Some(std::time::Instant::now() - ECHO_DEADLINE - std::time::Duration::from_secs(1));
        frame(&mut app, &ctx);
        assert!(
            matches!(app.echo_test, EchoTest::Failed(_)),
            "the echo test is still listening after its deadline: {:?}",
            app.echo_test
        );
    }

    #[tokio::test]
    async fn the_echo_test_is_on_the_settings_page() {
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();

        app.view = View::Settings;
        let output = frame(&mut app, &ctx);

        assert!(
            button_at(&output, "Echo test").is_some(),
            "the settings page has no echo test"
        );
        let text = screen_text(&output);
        assert!(
            text.contains("microphone"),
            "the echo test does not say it plays the microphone:\n{text}"
        );
    }

    #[test]
    fn the_echo_report_says_what_it_heard() {
        let nothing = qsoftphone_media::EchoReport {
            peak: 0.0,
            samples: 0,
            microphone: "Mic".to_string(),
            speaker: "Speaker".to_string(),
        };
        assert!(
            echo_report_text(&nothing).contains("nothing"),
            "a microphone that sent nothing does not say so"
        );

        let silent = qsoftphone_media::EchoReport {
            peak: 0.0,
            samples: 4_800,
            microphone: "Mic".to_string(),
            speaker: "Speaker".to_string(),
        };
        assert!(
            echo_report_text(&silent).contains("silent"),
            "a silent microphone does not say so"
        );

        let voice = qsoftphone_media::EchoReport {
            peak: 0.5,
            samples: 4_800,
            microphone: "Mic".to_string(),
            speaker: "Speaker".to_string(),
        };
        assert!(
            echo_report_text(&voice).contains("came in"),
            "a microphone that heard something does not say so"
        );
    }

    #[test]
    fn the_echo_report_says_which_devices_it_used() {
        // A quiet echo test is usually a device the user did not mean
        // to use: a laptop whose default speaker is "Headphone" while
        // the headphones are unplugged plays into nothing at all.
        let report = qsoftphone_media::EchoReport {
            peak: 0.5,
            samples: 4_800,
            microphone: "Microphone Array".to_string(),
            speaker: "Headphone".to_string(),
        };
        let text = echo_report_text(&report);
        assert!(
            text.contains("Microphone Array") && text.contains("Headphone"),
            "the echo report does not say which devices it used:\n{text}"
        );
    }

    #[test]
    fn the_echo_report_says_the_level_in_decibels() {
        // "The microphone was silent" and "the microphone heard a
        // whisper at -58 dB" are the same word and completely different
        // problems. Only the number tells them apart.
        let quiet = qsoftphone_media::EchoReport {
            peak: 0.000_465,
            samples: 240_128,
            microphone: "Microphone Array".to_string(),
            speaker: "Speaker".to_string(),
        };
        let text = echo_report_text(&quiet);
        assert!(
            text.contains("-67 dB"),
            "the report does not say how quiet the microphone was:\n{text}"
        );
        assert!(
            text.contains("heard nothing"),
            "the report does not say that nothing was heard:\n{text}"
        );

        let voice = qsoftphone_media::EchoReport {
            peak: 0.125,
            samples: 240_128,
            microphone: "Microphone Array".to_string(),
            speaker: "Speaker".to_string(),
        };
        let text = echo_report_text(&voice);
        assert!(
            text.contains("-18 dB") && text.contains("came in"),
            "the report does not say what it heard:\n{text}"
        );
    }

    #[tokio::test]
    async fn the_echo_test_says_when_the_speaker_is_a_headphone() {
        // A headphone jack with nothing plugged into it is the
        // commonest reason a test is silent.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();

        app.view = View::Settings;
        app.echo_test = EchoTest::Done {
            text: "Your voice came in; the loudest moment was -18 dB.".to_string(),
            speaker: "Headphone (Realtek(R) Audio)".to_string(),
        };
        let output = frame(&mut app, &ctx);

        let text = screen_text(&output);
        assert!(
            text.contains("Headphone") && text.contains("plugged into it"),
            "the echo test does not warn about a headphone speaker:\n{text}"
        );
    }

    #[tokio::test]
    async fn the_level_meter_is_there_while_the_echo_test_runs() {
        // The meter is the answer to "the microphone does not work":
        // if the user speaks and the bar does not move, the microphone
        // is not working.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();

        app.view = View::Settings;
        app.echo_test = EchoTest::Running;
        let output = frame(&mut app, &ctx);

        let text = screen_text(&output);
        assert!(
            text.contains("Speak now"),
            "the echo test does not ask the user to speak:\n{text}"
        );
    }

    #[tokio::test]
    async fn the_call_list_holds_only_the_calls_that_are_up() {
        // A finished call belongs to the history, and the history has
        // its own page.
        let (mut app, bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();

        // A call that is up: it is in the list.
        let mut incoming = incoming_event();
        incoming.call_id = "call-1".to_string();
        bus.publish("acct-ui-1", incoming);
        frame(&mut app, &ctx);
        let output = frame(&mut app, &ctx);
        assert!(
            screen_text(&output).contains("6666"),
            "the call that is up is not in the call list"
        );

        // It is hung up: it leaves the list.
        let mut ended = CallEvent::new(CallEventType::CallEnded);
        ended.call_id = "call-1".to_string();
        ended.call_direction = Some(Direction::Incoming);
        ended.call_state = "ended".to_string();
        ended.hangup_cause = "Normal".to_string();
        bus.publish("acct-ui-1", ended);
        frame(&mut app, &ctx);
        let output = frame(&mut app, &ctx);

        let text = screen_text(&output);
        assert!(
            text.contains("No calls up."),
            "the call list still holds a call that was hung up:\n{text}"
        );
    }

    #[tokio::test]
    async fn the_milestone_note_is_gone() {
        // "M5 — settings" is the build's own bookkeeping. It says
        // nothing to a user.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        for view in [View::Call, View::History, View::Account, View::Settings] {
            app.view = view;
            let output = frame(&mut app, &ctx);
            let text = screen_text(&output);
            assert!(
                !text.contains("M5 — settings"),
                "the milestone note is back on the {view:?} page:\n{text}"
            );
        }
    }

    #[tokio::test]
    async fn a_hung_up_call_leaves_the_status_ready() {
        // The status line used to stay on "Hanging up…" for ever,
        // because nothing ever said the call was over.
        let (mut app, bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        app.status = "Hanging up…".to_string();

        run_a_call(&bus, "call-1", true).await;
        frame(&mut app, &ctx);

        assert_eq!(
            app.status, "Ready.",
            "the status line still says {:?}",
            app.status
        );
    }

    #[tokio::test]
    async fn the_history_page_says_what_the_marks_mean() {
        // The rows use an arrow, a colour and a bare number, and none
        // of those explain themselves. The legend is the key.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();

        app.view = View::History;
        let output = frame(&mut app, &ctx);
        let text = screen_text(&output);

        for expected in [
            "outgoing",
            "incoming",
            "a bare number is the SIP status code",
            // The result labels, so the colours have a key.
            "Missed",
            "Busy",
            "No answer",
            "Rejected",
            "Failed",
        ] {
            assert!(
                text.contains(expected),
                "the legend does not say {expected:?}:\n{text}"
            );
        }
    }

    #[tokio::test]
    async fn the_history_loads_the_rows_that_were_already_there() {
        // The window opens on a database that already has calls in it
        // — from yesterday, or from before the app was closed.
        let db = Db::open_in_memory().expect("db");
        let store = HistoryStore::new(&db);
        store
            .record(&CallRecord::from_outgoing(
                "acct-ui-1",
                "call-old",
                "sip:6666@sip.example.com",
                "Alice",
                1_000,
                61_000,
                200,
                "Normal",
                true,
            ))
            .expect("the row was stored");

        let (mut app, _bus) = build_app_with(db, tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        wait_for_history(&mut app, &ctx, 1).await;

        assert_eq!(app.history_rows.len(), 1, "the old call is not there");
        assert_eq!(app.history_rows[0].remote_number, "6666");
        assert_eq!(app.history_rows[0].remote_name, "Alice");
    }

    #[tokio::test]
    async fn a_redial_uses_the_dialled_number() {
        // §5.1: for an outgoing call the redial dials the number that
        // was dialled — for a DID-forwarded call the remote party is
        // the PBX that forwarded it.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let mut record = CallRecord::from_outgoing(
            "acct-ui-1",
            "call-did",
            "sip:asterisk@sip.example.com",
            "",
            1_000,
            2_000,
            200,
            "Normal",
            true,
        );
        record.remote_number = "asterisk".to_string();
        record.dialed_number = "6666".to_string();

        app.redial(&record);

        // The account is not registered, so the call cannot be placed
        // — but the target is the dialled number, not the PBX.
        assert_eq!(app.dial_number, "6666");
    }

    #[tokio::test]
    async fn a_redial_of_an_incoming_call_calls_the_caller_back() {
        // The bug the user reported: a call from 6661 to 104 has a
        // dialled number of 104 — our own extension — and the redial
        // called us instead of the caller.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let record = CallRecord::from_incoming(
            "acct-ui-1",
            "call-in",
            "sip:6661@sip.example.com",
            "104",
            "",
            1_000,
            2_000,
            200,
            "Normal",
            true,
        );
        assert_eq!(record.remote_number, "6661");

        app.redial(&record);

        assert_eq!(app.dial_number, "6661");
    }

    #[tokio::test]
    async fn the_test_sound_is_played_off_the_interface_thread() {
        // The button opens the speaker and waits for the tone, so it
        // cannot run on the interface thread (§3). The work happens on
        // the runtime and the answer comes back on a channel.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        app.view = View::Settings;

        app.play_test_sound();
        assert_eq!(app.sound_test, SoundTest::Playing);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.sound_test == SoundTest::Playing && std::time::Instant::now() < deadline {
            frame(&mut app, &ctx);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        // The tone was played, or the machine has no speaker to play
        // it on — both are answers, and neither is a hang.
        assert_ne!(
            app.sound_test,
            SoundTest::Playing,
            "the test sound never finished"
        );
    }

    /// The text on the screen.
    ///
    /// The shapes carry the text that was laid out, so this is what
    /// the user can read.
    fn screen_text(output: &egui::FullOutput) -> String {
        let mut text = String::new();
        for clipped in &output.shapes {
            if let egui::epaint::Shape::Text(text_shape) = &clipped.shape {
                text.push_str(&text_shape.galley.job.text);
                text.push('\n');
            }
        }
        text
    }

    /// Where a piece of text is painted, left and right.
    ///
    /// Two pieces of text on top of each other are two unreadable
    /// pieces of text, so this is how they are caught.
    fn text_box(output: &egui::FullOutput, text: &str) -> Option<(f32, f32)> {
        for clipped in &output.shapes {
            if let egui::epaint::Shape::Text(text_shape) = &clipped.shape {
                if text_shape.galley.job.text == text {
                    let left = text_shape.pos.x;
                    let right = left + text_shape.galley.rect.width();
                    return Some((left, right));
                }
            }
        }
        None
    }

    #[tokio::test]
    async fn the_call_page_offers_no_codec_picker() {
        // M5: the codec picker is off the call page. It was in the
        // middle of the dialer, and a call page is no place for a
        // list of three codecs.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        let output = frame(&mut app, &ctx);
        let text = screen_text(&output);
        assert!(!text.contains("Codec"), "the codec picker is back:\n{text}");
    }

    #[tokio::test]
    async fn the_whole_call_page_fits_the_window() {
        // The dialer and the calls list all have to be on the screen
        // at the size the app opens at. A row that is culled is a
        // button the user cannot press.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        let output = frame(&mut app, &ctx);
        let text = screen_text(&output);

        assert!(text.contains("Number"), "the dialer is cut off:\n{text}");
        assert!(text.contains("Call"), "the call button is cut off:\n{text}");
        assert!(text.contains("CALLS"), "the call list is cut off:\n{text}");
        // The codec picker is not on the call page any more: it is in
        // the settings, where the rest of the media lives.
        assert!(
            !text.contains("Codec"),
            "the codec picker is back on the call page:\n{text}"
        );
    }

    #[tokio::test]
    async fn the_settings_page_holds_the_codecs_and_the_sound_card() {
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        app.view = View::Settings;
        let output = frame(&mut app, &ctx);
        let text = screen_text(&output);

        assert!(text.contains("CODECS"), "the codecs are cut off:\n{text}");
        assert!(text.contains("PCMU"), "the codecs are cut off:\n{text}");
        assert!(text.contains("G722"), "the codecs are cut off:\n{text}");
        assert!(
            text.contains("SOUND CARD"),
            "the sound card is cut off:\n{text}"
        );
        assert!(
            text.contains("Microphone"),
            "the microphone picker is cut off:\n{text}"
        );
        assert!(
            text.contains("Speaker"),
            "the speaker picker is cut off:\n{text}"
        );
        assert!(
            text.contains("Test sound"),
            "the test sound is cut off:\n{text}"
        );
    }

    #[tokio::test]
    async fn the_settings_page_says_when_the_session_is_remote() {
        // A remote session brings the playback and not the microphone,
        // and the switch for the microphone is on the **client**. The
        // user has to be told where to look, or the call is one-way
        // for a reason nobody can guess.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();
        app.view = View::Settings;

        // Not a remote session: nothing to say.
        app.remote_session = false;
        let output = frame(&mut app, &ctx);
        let text = screen_text(&output);
        assert!(
            !text.contains("remote session"),
            "a console session is being warned about:\n{text}"
        );

        // A remote session: the notice, and where to look.
        app.remote_session = true;
        let output = frame(&mut app, &ctx);
        let text = screen_text(&output);
        assert!(
            text.contains("remote session"),
            "the remote session is not on the page:\n{text}"
        );
        assert!(
            text.contains("Record from this computer"),
            "the notice does not say where to look:\n{text}"
        );
    }

    #[tokio::test]
    async fn the_codecs_can_be_reordered_removed_and_added() {
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        assert_eq!(
            app.config.media.codecs,
            vec!["PCMU", "PCMA", "G722"],
            "the wideband codec is there by default"
        );

        // G.722 to the front — that is what a user with a PBX that
        // allows G.722 wants.
        app.move_codec(Codec::G722, true);
        app.move_codec(Codec::G722, true);
        assert_eq!(app.config.media.codecs, vec!["G722", "PCMU", "PCMA"]);

        // And the call layer was told, because the registrar and the
        // call layer share the settings.
        let media = app.calls.media_config();
        assert_eq!(media.codecs, vec![Codec::G722, Codec::Pcmu, Codec::Pcma]);

        // Out it goes, and back in again.
        app.remove_codec(Codec::G722);
        assert_eq!(app.config.media.codecs, vec!["PCMU", "PCMA"]);
        assert!(!app.enabled_codecs().contains(&Codec::G722));

        app.add_codec(Codec::G722);
        assert_eq!(app.config.media.codecs, vec!["PCMU", "PCMA", "G722"]);
    }

    #[tokio::test]
    async fn the_last_codec_cannot_be_turned_off() {
        // A call with no codec has no media at all.
        let (mut app, _bus) = build_app(tokio::runtime::Handle::current());
        app.remove_codec(Codec::Pcmu);
        app.remove_codec(Codec::Pcma);
        app.remove_codec(Codec::G722);
        assert!(
            !app.config.media.codecs.is_empty(),
            "a codec has to stay on"
        );
    }

    #[tokio::test]
    async fn a_connected_call_says_what_the_media_is_doing() {
        // M4: the call page says what the media is doing, and offers
        // the two mute buttons.
        let (mut app, bus) = build_app(tokio::runtime::Handle::current());
        let ctx = egui::Context::default();

        let mut event = CallEvent::new(CallEventType::CallAnswered);
        event.user = "104".to_string();
        event.call_id = "call-media".to_string();
        event.call_direction = Some(Direction::Outgoing);
        event.call_state = "connected".to_string();
        event.callee = "sip:6666@sip.example.com".to_string();
        bus.publish("acct-ui-1", event);
        frame(&mut app, &ctx);

        app.active_call_id = Some("call-media".to_string());
        let output = frame(&mut app, &ctx);
        let text = screen_text(&output);

        assert!(text.contains("Media:"), "no media line:\n{text}");
        assert!(text.contains("Mute mic"), "no microphone button:\n{text}");
        assert!(text.contains("Mute speaker"), "no speaker button:\n{text}");
    }

    // TEMPORARY PROBE — what does egui lay out for the level row?
    #[tokio::test]
    async fn the_meters_number_does_not_land_on_the_next_thing() {
        // The meter painted its number outside the space the layout had
        // given it, so the number landed on top of the text that came
        // after it: two pieces of text on top of each other, and
        // neither of them readable.
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1100.0, 700.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.horizontal(|ui| {
                    draw_level_meter(ui, 0.18, 0.2, 140.0, true);
                    ui.label("after the meter");
                });
            });
        });
        output.textures_delta.clear();

        let number = text_box(&output, "-15 dB").expect("the meter has no number");
        let after = text_box(&output, "after the meter").expect("the label is gone");
        assert!(
            number.1 <= after.0,
            "the meter's number is painted over the next thing: {number:?} and {after:?}"
        );
    }

    #[test]
    fn a_path_that_drops_the_audio_says_so() {
        // The user's line: the echo comes back, but broken. The frames
        // the peer sent are not all there, and that is the fact that
        // explains it — and with G.722 it is worse than a dropout.
        let mut status = media_status(0.2, 0.2, 0.2);
        status.peer_lost = 216;
        status.peer_received = 574;
        let said = silence_diagnosis(&status).expect("a broken call says something");
        assert!(
            said.contains("dropping the audio"),
            "the path was dropping the audio and the app did not say so: {said}"
        );
        assert!(
            said.contains("G.722"),
            "the stateful codec was not mentioned: {said}"
        );

        // A call where the path carries everything says nothing about
        // it.
        let mut good = media_status(0.2, 0.2, 0.2);
        good.peer_lost = 1;
        good.peer_received = 574;
        assert!(
            silence_diagnosis(&good).is_none(),
            "a call with one lost frame complained: {:?}",
            silence_diagnosis(&good)
        );
    }

    #[test]
    fn a_call_that_is_too_quiet_says_so() {
        // "No sound" is not always silence: a line can carry room
        // noise and nothing else, and the user hears nothing for that
        // reason.
        let quiet = media_status(0.004, 0.004, 0.02);
        let said = silence_diagnosis(&quiet).expect("a quiet call says something");
        assert!(
            said.contains("too quiet to hear"),
            "the line was quiet and the app did not say so: {said}"
        );

        // The same for the other two directions.
        let quiet_out = media_status(0.2, 0.2, 0.004);
        let said = silence_diagnosis(&quiet_out).expect("a quiet call says something");
        assert!(
            said.contains("picking up almost nothing"),
            "the microphone was quiet and the app did not say so: {said}"
        );
    }

    #[test]
    fn a_line_that_carried_a_beep_and_then_went_quiet_is_called_quiet() {
        // The user's line: a burst in the first second and then almost
        // nothing. The peak says the peer was loud, the level says
        // there is nothing to hear — and the level is the one the
        // ear is given.
        let beeped = media_status_live(0.23, 0.002, 0.18, 0.012);
        let said = silence_diagnosis(&beeped).expect("a quiet call says something");
        assert!(
            said.contains("too quiet to hear"),
            "the line went quiet and the app did not say so: {said}"
        );

        // And a line that is loud right now is not complained about.
        let talking = media_status_live(0.23, 0.2, 0.18, 0.012);
        assert!(
            silence_diagnosis(&talking).is_none(),
            "a call with a voice on it was complained about: {:?}",
            silence_diagnosis(&talking)
        );
    }
}
