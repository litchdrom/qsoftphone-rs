//! The egui shell.
//!
//! M0 scope: the window, the account list and the account editor.
//! The dialer, the call window and the history list arrive with the
//! SIP stack in M1/M2.
//!
//! Note the egui 0.37 API: `eframe::App` is driven by `App::ui`,
//! which hands over a root `Ui` rather than a `Context`, and the
//! panel types are unified into `egui::Panel`.

use eframe::egui;
use qsoftphone_core::config::AppConfig;
use qsoftphone_core::model::account::Account;
use qsoftphone_core::paths::AppPaths;
use qsoftphone_core::storage::{AccountStore, Db};

/// The application state.
pub struct QSoftphoneApp {
    paths: AppPaths,
    db: Db,
    config: AppConfig,
    /// Handle to the tokio runtime the SIP stack will run on. Kept so
    /// the runtime is not dropped while the window is open.
    _runtime: tokio::runtime::Handle,
    accounts: Vec<Account>,
    /// Index into `accounts` of the selected row.
    selected: Option<usize>,
    draft: Draft,
    status: String,
    /// The theme is applied once, on the first frame.
    theme_applied: bool,
}

/// The account form. The same draft backs the "add account" fields
/// and the per-account identity-header policy of the selected
/// account.
#[derive(Debug, Clone, Default)]
struct Draft {
    label: String,
    registrar: String,
    user: String,
    password: String,
    transport: String,
    caller_id_header: String,
    dialed_number_header: String,
}

impl Draft {
    fn to_account(&self) -> Account {
        Account::new(
            self.label.trim(),
            self.registrar.trim(),
            self.user.trim(),
            self.password.clone(),
            self.transport.trim(),
        )
    }
}

impl QSoftphoneApp {
    pub fn new(
        paths: AppPaths,
        db: Db,
        config: AppConfig,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        let accounts = AccountStore::new(&db).list().unwrap_or_default();
        // Prefer the account the settings file points at; fall back
        // to the first one.
        let selected = config
            .active_account_id
            .as_ref()
            .and_then(|id| accounts.iter().position(|a| &a.id == id))
            .or_else(|| (!accounts.is_empty()).then_some(0));
        Self {
            paths,
            db,
            config,
            _runtime: runtime,
            accounts,
            selected,
            draft: Draft {
                transport: "udp".to_string(),
                ..Default::default()
            },
            status: "Ready.".to_string(),
            theme_applied: false,
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
        self.config.active_account_id = id;
        if let Err(e) = self.config.save(&self.paths.config_file()) {
            tracing::warn!(error = %e, "could not persist the active account");
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
        if self.draft.label.trim().is_empty() && self.draft.user.trim().is_empty() {
            self.status = "An account needs at least a label or a user.".to_string();
            return;
        }
        let account = self.draft.to_account();
        match AccountStore::new(&self.db).upsert(&account) {
            Ok(stored) => {
                self.status = format!("Saved account {}.", stored.label);
                self.draft = Draft {
                    transport: "udp".to_string(),
                    ..Default::default()
                };
                self.reload();
                self.selected = self.accounts.iter().position(|a| a.id == stored.id);
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
                self.reload();
            }
            Ok(false) => self.status = "That account was already gone.".to_string(),
            Err(e) => self.status = format!("Could not remove the account: {e}"),
        }
    }

    fn save_selected_policy(&mut self) {
        let Some(index) = self.selected else {
            return;
        };
        let Some(mut account) = self.accounts.get(index).cloned() else {
            return;
        };
        account.caller_id_header = self.draft.caller_id_header.trim().to_string();
        account.dialed_number_header = self.draft.dialed_number_header.trim().to_string();
        match AccountStore::new(&self.db).upsert(&account) {
            Ok(_) => {
                self.status = "Saved account policy.".to_string();
                self.reload();
            }
            Err(e) => self.status = format!("Could not save the account: {e}"),
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

        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("qsoftphone");
                ui.separator();
                ui.label("SIP: not registered");
                ui.separator();
                ui.label("M0 — workspace, config, accounts");
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
                let mut pending_policy: Option<(String, String)> = None;

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
                        if ui
                            .selectable_value(&mut self.selected, Some(index), text)
                            .clicked()
                        {
                            // Collected here and applied below: the
                            // loop borrows `self.accounts`, so the
                            // whole-self borrow of
                            // `persist_active_account` has to wait
                            // until the loop is done.
                            pending_policy = Some((
                                account.caller_id_header.clone(),
                                account.dialed_number_header.clone(),
                            ));
                        }
                    }
                });

                if let Some((caller, dialed)) = pending_policy {
                    self.draft.caller_id_header = caller;
                    self.draft.dialed_number_header = dialed;
                    self.persist_active_account();
                }

                ui.separator();
                egui::CollapsingHeader::new("Add account").show(ui, |ui| {
                    ui.label("Label");
                    ui.text_edit_singleline(&mut self.draft.label);
                    ui.label("Registrar");
                    ui.text_edit_singleline(&mut self.draft.registrar);
                    ui.label("User");
                    ui.text_edit_singleline(&mut self.draft.user);
                    ui.label("Password");
                    ui.text_edit_singleline(&mut self.draft.password);
                    ui.horizontal(|ui| {
                        ui.label("Transport");
                        egui::ComboBox::from_id_salt("transport")
                            .selected_text(&self.draft.transport)
                            .show_ui(ui, |ui| {
                                for t in ["udp", "tcp", "tls"] {
                                    ui.selectable_value(
                                        &mut self.draft.transport,
                                        t.to_string(),
                                        t,
                                    );
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
            ui.horizontal(|ui| {
                ui.heading("Dialer");
                ui.separator();
                ui.label("Available from M1, once the SIP stack is wired up.");
            });
            ui.separator();

            if let Some(index) = self.selected
                && let Some(account) = self.accounts.get(index)
            {
                let title = if account.label.is_empty() {
                    account.user.clone()
                } else {
                    account.label.clone()
                };
                ui.heading(title);
                egui::Grid::new("account_grid")
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label("Registrar");
                        ui.label(&account.registrar);
                        ui.end_row();

                        ui.label("User");
                        ui.label(&account.user);
                        ui.end_row();

                        ui.label("Transport");
                        ui.label(&account.transport);
                        ui.end_row();

                        ui.label("Domain");
                        ui.label(&account.domain);
                        ui.end_row();

                        ui.label("Outbound proxy");
                        ui.label(&account.outbound_proxy);
                        ui.end_row();
                    });

                ui.separator();
                ui.label("Identity headers (per-account policy)");
                ui.horizontal(|ui| {
                    ui.label("Caller name");
                    ui.text_edit_singleline(&mut self.draft.caller_id_header);
                });
                ui.horizontal(|ui| {
                    ui.label("Dialled number");
                    ui.text_edit_singleline(&mut self.draft.dialed_number_header);
                });
                if ui.button("Save policy").clicked() {
                    self.save_selected_policy();
                }
            } else {
                ui.weak("Select an account, or add one on the left.");
            }

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
