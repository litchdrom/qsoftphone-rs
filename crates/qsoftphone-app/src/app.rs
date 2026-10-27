//! The egui shell.
//!
//! M0 scope: the window, the account list and the account editor.
//! M1 adds the registration state. The dialer, the call window and the
//! history list arrive with the calling stack in M2/M3.
//!
//! Note the egui 0.37 API: `eframe::App` is driven by `App::ui`,
//! which hands over a root `Ui` rather than a `Context`, and the
//! panel types are unified into `egui::Panel`.

use std::collections::HashMap;

use eframe::egui;
use qsoftphone_core::config::AppConfig;
use qsoftphone_core::model::account::Account;
use qsoftphone_core::paths::AppPaths;
use qsoftphone_core::storage::{AccountStore, Db};
use qsoftphone_sip::{RegistrationManager, RegistrationState};

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
        registrations: RegistrationManager,
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
            _runtime: runtime,
            registrations,
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
    /// last frame (§3: events come on a channel and are drained at
    /// the top of each frame).
    fn drain_registrations(&mut self) {
        for event in self.registrations.drain() {
            let state = event.state.clone();
            self.status = format!("{} — {}", event.user, state.summary());
            self.states.insert(event.account_id, state);
        }
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

impl eframe::App for QSoftphoneApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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

        egui::Panel::top("top_bar").show(ui, |ui| {
            // The registration state of the selected account, read from
            // the cached state the SIP layer pushed over the channel.
            let sip_state = self
                .selected
                .and_then(|i| self.accounts.get(i))
                .map(|a| self.registration_state(&a.id).summary())
                .unwrap_or_else(|| "No account selected".to_string());
            ui.horizontal(|ui| {
                ui.heading("qsoftphone");
                ui.separator();
                ui.label(format!("SIP: {sip_state}"));
                ui.separator();
                ui.label("M1 — registration");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("v{}", env!("CARGO_PKG_VERSION")));
                });
            });
        });

        egui::Panel::left("accounts")
            .resizable(true)
            .default_size(280.0)
            .show(ui, |ui| {
                // Set when the user picks a different account; applied
                // once the account list's borrow has ended.
                let mut pending_select: Option<usize> = None;

                ui.heading("Accounts");
                ui.separator();

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
                            format!("(unnamed account #{index})")
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
                egui::CollapsingHeader::new("Add account").show(ui, |ui| {
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

                if ui.button("Remove selected").clicked() {
                    self.delete_selected();
                }
            });

        egui::CentralPanel::default().show(ui, |ui| {
            let account = self.selected_account().cloned();

            let Some(account) = account else {
                ui.weak("Select an account, or add one on the left.");
                ui.separator();
                ui.label(format!("Status: {}", self.status));
                return;
            };

            ui.heading(if account.label.is_empty() {
                "(unnamed account)".to_string()
            } else {
                account.label.clone()
            });
            ui.weak(format!("id {}", account.id));
            ui.separator();

            // The editor. The borrow of the draft ends with this
            // block, so the buttons below can take `&mut self`.
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
            ui.label("Registration");
            ui.horizontal(|ui| {
                ui.label(self.registration_state(&account.id).summary());
            });
            ui.horizontal(|ui| {
                if ui.button("Register").clicked() {
                    self.register_selected();
                }
                if ui.button("Unregister").clicked() {
                    self.unregister_selected();
                }
            });

            ui.separator();
            ui.label(format!("Status: {}", self.status));
            ui.separator();
            egui::CollapsingHeader::new("Paths").show(ui, |ui| {
                ui.monospace(format!("config  {}", self.paths.config_file().display()));
                ui.monospace(format!("data    {}", self.paths.data_dir().display()));
                ui.monospace(format!("logs    {}", self.paths.log_dir().display()));
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An app on a throwaway database, with a runtime to hold the
    /// registration manager.
    fn app(test_name: &str) -> (QSoftphoneApp, tokio::runtime::Runtime) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let paths = AppPaths::under(
            std::env::temp_dir()
                .join("qsoftphone-app-tests")
                .join(test_name),
        );
        paths.ensure().expect("paths");
        let db = Db::open_in_memory().expect("db");
        let config = AppConfig::load(&paths.config_file()).expect("config");
        let app = QSoftphoneApp::new(
            paths,
            db,
            config,
            runtime.handle().clone(),
            RegistrationManager::new(runtime.handle().clone()),
        );
        (app, runtime)
    }

    /// Fill in the "add account" form.
    fn fill_new_account(app: &mut QSoftphoneApp, registrar: &str) {
        app.new_account.label = "Office".to_string();
        app.new_account.registrar = registrar.to_string();
        app.new_account.user = "104".to_string();
        app.new_account.password = "secret".to_string();
        app.new_account.transport = "udp".to_string();
    }

    #[test]
    fn an_account_can_be_edited_in_place() {
        let (mut app, _runtime) = app("edit-in-place");

        fill_new_account(&mut app, "old.example.com");
        app.add_account();
        assert_eq!(app.accounts.len(), 1);

        let id = app.accounts[0].id.clone();
        let created_at = app.accounts[0].created_at;

        // The editor is loaded from the selected account.
        assert_eq!(app.edit_account.registrar, "old.example.com");
        assert_eq!(app.editing_id.as_deref(), Some(id.as_str()));

        // Edit it.
        app.edit_account.registrar = "new.example.com".to_string();
        app.edit_account.transport = "tcp".to_string();
        app.save_selected();

        // One account, the same one, with the new settings.
        assert_eq!(app.accounts.len(), 1);
        assert_eq!(app.accounts[0].id, id, "the account must not be duplicated");
        assert_eq!(app.accounts[0].created_at, created_at);
        assert_eq!(app.accounts[0].registrar, "new.example.com");
        assert_eq!(app.accounts[0].transport, "tcp");
    }

    #[test]
    fn the_new_account_form_does_not_touch_the_editor() {
        // One draft for adding, one for editing. With a single shared
        // draft, typing a new account silently changed what "save"
        // would write to the selected one.
        let (mut app, _runtime) = app("separate-drafts");

        fill_new_account(&mut app, "old.example.com");
        app.add_account();

        app.new_account.registrar = "typed-but-not-saved.example.com".to_string();
        assert_eq!(app.edit_account.registrar, "old.example.com");
    }

    #[test]
    fn removing_the_account_clears_the_editor() {
        let (mut app, _runtime) = app("remove");

        fill_new_account(&mut app, "old.example.com");
        app.add_account();
        assert!(app.editing_id.is_some());

        app.delete_selected();

        assert!(app.accounts.is_empty());
        assert!(app.editing_id.is_none());
        assert!(app.edit_account.registrar.is_empty());
    }
}
