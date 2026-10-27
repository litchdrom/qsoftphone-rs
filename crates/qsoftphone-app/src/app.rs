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

use eframe::egui;
use qsoftphone_core::config::AppConfig;
use qsoftphone_core::model::account::Account;
use qsoftphone_core::model::call_event::{CallEventType, Direction};
use qsoftphone_core::paths::AppPaths;
use qsoftphone_core::storage::{AccountStore, Db};
use qsoftphone_sip::{
    CallBus, CallManager, RegistrationManager, RegistrationState, SessionRegistry,
};

/// The application state.
pub struct QSoftphoneApp {
    paths: AppPaths,
    db: Db,
    config: AppConfig,
    /// Handle to the tokio runtime the SIP stack runs on. Kept so
    /// the runtime is not dropped while the window is open.
    _runtime: tokio::runtime::Handle,
    /// The registrations, and the state each one is in.
    registrations: RegistrationManager,
    /// The calls, and the state each one is in.
    calls: CallManager,
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
}

/// Which page the middle panel shows.
///
/// Two pages, and the one that matters is the first: a softphone is
/// for calling, so the dialer is what the window opens on. The account
/// settings are there when they are wanted and out of the way when
/// they are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    /// The dialer and the calls.
    Call,
    /// The account settings.
    Account,
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
    /// outgoing one.
    peer: String,
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
        let mut app = Self {
            paths,
            db,
            config,
            _runtime: runtime.clone(),
            registrations: RegistrationManager::new(runtime.clone(), sessions.clone(), bus.clone()),
            calls: CallManager::new(runtime, sessions, bus, call_events),
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
        };
        app.load_selected();
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
            match event.event_type {
                CallEventType::IncomingCall => {
                    view.peer = event.caller.clone();
                    // A call is coming in: show the call page, not the
                    // account settings.
                    self.view = View::Call;
                }
                CallEventType::OutgoingCallInitiated => view.peer = event.callee.clone(),
                CallEventType::CallEnded => view.ended = true,
                _ => {}
            }

            // The call panel follows the newest call that is not over.
            if !view.ended && self.active_call_id.is_none() {
                self.active_call_id = Some(call_id);
            }
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
        let Some(account) = self.selected_account().cloned() else {
            return;
        };
        let target = self.dial_number.trim().to_string();
        if target.is_empty() {
            self.status = "There is no number to call.".to_string();
            return;
        }
        match self.calls.place_call(&account.id, &target) {
            Ok(call_id) => {
                self.status = format!("Calling {target}…");
                self.active_call_id = Some(call_id);
            }
            Err(e) => self.status = format!("Could not place the call: {e}"),
        }
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
            // Two pages, one line to switch between them.
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(self.view == View::Call, "Call")
                    .clicked()
                {
                    self.view = View::Call;
                }
                if ui
                    .selectable_label(self.view == View::Account, "Account")
                    .clicked()
                {
                    self.view = View::Account;
                }
                ui.separator();
                ui.label(egui::RichText::new("M2 — calling").small());
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
                View::Account => self.draw_account(ui),
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

        ui.add_space(6.0);
        ui.label(egui::RichText::new("CALLS").strong().size(11.0));
        ui.separator();

        let calls = self.calls();
        if calls.is_empty() {
            ui.weak("No calls yet.");
            return;
        }
        for call in calls {
            self.draw_call(ui, call);
            ui.separator();
        }
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
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("DTMF").strong().size(11.0));
                for row in [
                    ["1", "2", "3"],
                    ["4", "5", "6"],
                    ["7", "8", "9"],
                    ["*", "0", "#"],
                ] {
                    ui.vertical(|ui| {
                        for digit in row {
                            if ui
                                .add(egui::Button::new(digit).min_size(egui::vec2(34.0, 22.0)))
                                .clicked()
                            {
                                self.send_dtmf(digit);
                            }
                        }
                    });
                }
            });
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
}

impl eframe::App for QSoftphoneApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qsoftphone_core::model::call_event::CallEvent;
    use qsoftphone_core::storage::Db;

    /// An app with one account, and the bus its call events arrive on.
    fn build_app(handle: tokio::runtime::Handle) -> (QSoftphoneApp, CallBus) {
        let paths = AppPaths::under(std::env::temp_dir().join("qsoftphone-app-ui-test"));
        let db = Db::open_in_memory().expect("db");
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
    fn incoming_event() -> CallEvent {
        let mut event = CallEvent::new(CallEventType::IncomingCall);
        event.call_id = "call-1".to_string();
        event.call_direction = Some(Direction::Incoming);
        event.call_state = "ringing".to_string();
        event.caller = "6666".to_string();
        event.caller_id = "Alice".to_string();
        event.dialed_number = "6666".to_string();
        event
    }

    /// Run one frame of the UI, headlessly.
    fn frame(app: &mut QSoftphoneApp, ctx: &egui::Context) -> egui::FullOutput {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.draw(ui));
        // The font atlas the frame produced is of no use here, and
        // epaint panics when an unapplied delta is dropped.
        output.textures_delta.clear();
        output
    }

    /// Run one frame of the UI with a given input.
    fn frame_with(
        app: &mut QSoftphoneApp,
        ctx: &egui::Context,
        input: egui::RawInput,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(input, |ui| app.draw(ui));
        output.textures_delta.clear();
        output
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
}
