//! The call bar — the small window that says who is calling.
//!
//! The app lives in the tray, and a call that comes in is said in a
//! window of its own: a short bar with the caller, the number, the
//! account, and **Answer** and **Decline**.
//!
//! Answering in the bar brings the main window up — the user is going
//! to talk, and the call page is where the talk happens. Declining
//! leaves the app in the tray, where it was. The third button — the
//! **✕** — takes the bar off the screen and lets the call keep
//! ringing: the bar is how the call is said, not what the call is.
//!
//! ## How the window is made
//!
//! It is an egui **child viewport**: a native window of its own, owned
//! by the app's window. Three things about that are worth knowing:
//!
//! * the window has to **exist** before it can be shown, and a window
//!   is only made when the app draws a frame. So the bar's viewport is
//!   asked for from the first frame — with the window **hidden** — and
//!   the bar is only *shown* when a call comes in;
//! * a viewport that is not asked for again is **thrown away**, so the
//!   bar is asked for in every frame, visible or not;
//! * a hidden window is given the app's *logic* and no picture, so the
//!   bar is shown and hidden by a **command**: a command is the one
//!   thing that reaches a window while nothing is being drawn.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use egui::{Color32, RichText, Ui, Vec2, ViewportBuilder, ViewportId};
use qsoftphone_core::wake::Wake;

/// The id of the bar's viewport.
///
/// It is public because the app's tests look for the bar's window by
/// it: a command to the bar's window is the one thing a test can see.
pub fn viewport_id() -> ViewportId {
    ViewportId::from_hash_of("qsoftphone-call-bar")
}

/// The size of the bar, in points.
///
/// It is the size of what the bar says: the title, the caller, the
/// number, the account, and the two buttons. A bar with room to
/// spare is a bar with a hole in it.
const SIZE: [f32; 2] = [330.0, 158.0];

/// How far from the corner of the screen the bar sits.
const MARGIN: f32 = 16.0;

/// How much room the taskbar takes at the bottom of the screen.
///
/// The bar sits **above** the tray: the tray is where the eye already
/// is when the app is in it, and a bar under the taskbar would be a
/// bar nobody sees.
const TASKBAR: f32 = 48.0;

/// What the user did in the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallBarAction {
    /// Answer the call, and bring the main window up.
    Answer,
    /// Decline the call.
    Decline,
    /// Leave the call ringing, and take the bar off the screen.
    Ignore,
}

/// What the bar says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallBarView {
    /// The call the bar is about.
    pub call_id: String,
    /// The account the call is on.
    pub account: String,
    /// The number that is calling.
    pub number: String,
    /// The name of the caller, if the server said one.
    pub caller: String,
}

impl CallBarView {
    /// The one line that names the caller.
    ///
    /// It is the line the **tray tooltip** carries as well: the tray
    /// says who is calling even when the bar is not on the screen.
    pub fn headline(&self) -> String {
        match (self.caller.is_empty(), self.number.is_empty()) {
            (true, true) => "an incoming call".to_string(),
            (true, false) => self.number.clone(),
            (false, true) => self.caller.clone(),
            // The name and the number are the same thing: the server
            // sent no name, and the number is what the caller is
            // known by. Saying it twice is noise.
            (false, false) if self.caller == self.number => self.number.clone(),
            (false, false) => format!("{} ({})", self.caller, self.number),
        }
    }
}

/// The call bar: the window, what it says, and what the user did in it.
pub struct CallBar {
    /// What the bar says. The window is drawn in a pass of its own, so
    /// the two share this.
    view: Arc<Mutex<CallBarView>>,
    /// What the user did in the bar.
    actions: Sender<CallBarAction>,
    /// The other end of it: the app reads it.
    receiver: Receiver<CallBarAction>,
    /// Whether the bar is on the screen.
    open: bool,
    /// Whether the window has been told to be on the screen.
    ///
    /// The window is shown and hidden by a command, and a command is a
    /// message to the window: it is sent when the answer changes, and
    /// not every frame.
    shown: bool,
    /// The handle that asks the app's window to be drawn again.
    ///
    /// The bar is drawn in a pass of its own, and what the user did in
    /// it is read by the app's window — so the bar has to wake it.
    wake: Wake,
}

impl CallBar {
    /// A bar that says nothing yet.
    pub fn new(wake: Wake) -> Self {
        let (actions, receiver) = channel();
        Self {
            view: Arc::new(Mutex::new(CallBarView::default())),
            actions,
            receiver,
            open: false,
            shown: false,
            wake,
        }
    }

    /// Say who is calling, and put the bar on the screen.
    pub fn open(&mut self, view: CallBarView) {
        let mut current = self.view.lock().expect("the call bar is poisoned");
        *current = view;
        drop(current);
        self.open = true;
    }

    /// Take the bar off the screen.
    ///
    /// The call keeps ringing: the bar is how the call is said, not
    /// what the call is.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Whether the bar is on the screen.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// What the bar is saying now.
    ///
    /// The tests use it: what the bar says is the whole of the bar's
    /// job, and the job is proved by looking at it.
    #[cfg(test)]
    pub fn view(&self) -> CallBarView {
        self.view.lock().expect("the call bar is poisoned").clone()
    }

    /// The one line that names the caller, for the tray's tooltip.
    ///
    /// The tests use it: the tray says who is calling even when the
    /// bar is not on the screen, and it says the same line.
    #[cfg(test)]
    pub fn headline(&self) -> Option<String> {
        if !self.open {
            return None;
        }
        Some(self.view().headline())
    }

    /// What the user did in the bar, since the last time.
    pub fn take_actions(&self) -> Vec<CallBarAction> {
        let mut actions = Vec::new();
        while let Ok(action) = self.receiver.try_recv() {
            actions.push(action);
        }
        actions
    }

    /// Ask for something, as the user would in the bar.
    ///
    /// The tests use it: the bar is a window of its own, and a test
    /// cannot click in a window it has not got.
    #[cfg(test)]
    pub fn ask(&self, action: CallBarAction) {
        let _ = self.actions.send(action);
    }

    /// Ask for the bar's window.
    ///
    /// This is called in **every** frame — the window is made in the
    /// first one, and a viewport that is not asked for again is thrown
    /// away.
    ///
    /// The window is asked for **hidden**: it is shown and hidden by a
    /// command, and a command is the only thing that reaches a window
    /// while the app's own window is hidden and nothing is drawn.
    pub fn show(&self, ctx: &egui::Context) {
        let view = self.view.clone();
        let actions = self.actions.clone();
        let wake = self.wake.clone();
        let builder = ViewportBuilder::default()
            .with_title("qsoftphone — incoming call")
            .with_inner_size(SIZE)
            .with_min_inner_size(SIZE)
            .with_resizable(false)
            .with_decorations(false)
            .with_always_on_top()
            .with_visible(false)
            .with_position(corner(ctx));
        ctx.show_viewport_deferred(viewport_id(), builder, move |ui, _class| {
            let view = view.lock().expect("the call bar is poisoned");
            body(ui, &view, &actions, &wake);
        });
    }

    /// Whether the user asked for the bar to be shown or hidden.
    ///
    /// The window is made in the first frame and shown and hidden by a
    /// **command**, because a command is the only thing that reaches
    /// the window while the app's own window is hidden and nothing is
    /// being drawn.
    pub fn sync_visibility(&mut self, ctx: &egui::Context) {
        let visible = self.open;
        if visible == self.shown {
            return;
        }
        self.shown = visible;
        ctx.send_viewport_cmd_to(viewport_id(), egui::ViewportCommand::Visible(visible));
        if !visible {
            return;
        }
        // The size and the place: the bar is the size it says it is,
        // and it is in the corner of the screen the tray is in.
        //
        // They are said again here, and not only in the builder: a
        // window that was made before the screen was known is the
        // wrong size, and the right size is what the user sees.
        ctx.send_viewport_cmd_to(
            viewport_id(),
            egui::ViewportCommand::InnerSize(egui::vec2(SIZE[0], SIZE[1])),
        );
        ctx.send_viewport_cmd_to(
            viewport_id(),
            egui::ViewportCommand::OuterPosition(corner(ctx)),
        );
        // The bar may take the focus: the call is waiting, and the
        // user is going to answer it.
        ctx.send_viewport_cmd_to(viewport_id(), egui::ViewportCommand::Focus);
    }
}

/// Where the bar sits: the bottom right, above the taskbar.
///
/// The screen is the **monitor**, and not the app's own window: the
/// window is somewhere on the screen, and the bar is put in the
/// corner of the screen itself — the corner the tray is in.
fn corner(ctx: &egui::Context) -> egui::Pos2 {
    let screen = ctx
        .input(|input| input.viewport().monitor_size)
        .unwrap_or_else(|| egui::vec2(1920.0, 1080.0));
    egui::pos2(
        screen.x - SIZE[0] - MARGIN,
        screen.y - SIZE[1] - TASKBAR - MARGIN,
    )
}

/// The bar itself.
fn body(ui: &mut Ui, view: &CallBarView, actions: &Sender<CallBarAction>, wake: &Wake) {
    egui::Frame::window(ui.style())
        .inner_margin(14.0)
        .fill(ui.visuals().panel_fill)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Incoming call")
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("✕").clicked() {
                        let _ = actions.send(CallBarAction::Ignore);
                        wake.wake();
                    }
                });
            });

            ui.add_space(4.0);

            // The caller. The name is what the user knows the caller
            // by; the number is what the server says. When they are the
            // same thing, the number is not said again.
            let name = if view.caller.is_empty() {
                view.number.clone()
            } else {
                view.caller.clone()
            };
            ui.heading(if name.is_empty() {
                "Unknown".to_string()
            } else {
                name
            });
            if !view.caller.is_empty() && !view.number.is_empty() && view.caller != view.number {
                ui.label(RichText::new(&view.number).color(ui.visuals().weak_text_color()));
            }
            if !view.account.is_empty() {
                ui.label(
                    RichText::new(format!("on {}", view.account))
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            }

            ui.add_space(8.0);

            ui.horizontal(|ui| {
                let size = Vec2::new(96.0, 30.0);
                if ui
                    .add(
                        egui::Button::new(RichText::new("Answer").color(Color32::WHITE))
                            .fill(Color32::from_rgb(38, 130, 70))
                            .min_size(size),
                    )
                    .clicked()
                {
                    let _ = actions.send(CallBarAction::Answer);
                    wake.wake();
                }
                if ui
                    .add(
                        egui::Button::new(RichText::new("Decline").color(Color32::WHITE))
                            .fill(Color32::from_rgb(150, 54, 54))
                            .min_size(size),
                    )
                    .clicked()
                {
                    let _ = actions.send(CallBarAction::Decline);
                    wake.wake();
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bar with a wake of its own, so the test can count the wakes.
    fn bar_with_a_wake() -> (CallBar, Wake) {
        let wake = Wake::new();
        (CallBar::new(wake.clone()), wake)
    }

    /// The commands the bar sent for its own window.
    fn bar_commands(output: &egui::FullOutput) -> Vec<egui::ViewportCommand> {
        output
            .viewport_output
            .get(&viewport_id())
            .map(|viewport| viewport.commands.clone())
            .unwrap_or_default()
    }

    /// A context that gives the bar a window of its own.
    ///
    /// eframe on the desktop does not **embed** the viewports: the
    /// call bar is a native window, and not a panel of the app's own
    /// window. A test that embedded it would be testing something the
    /// app does not do.
    fn context() -> egui::Context {
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        ctx
    }

    /// Run one frame and throw the picture away.
    ///
    /// The textures are cleared because there is no painter here to
    /// apply them: a texture delta that is dropped unapplied is a
    /// panic in epaint.
    fn frame(ctx: &egui::Context, body: impl FnOnce(&egui::Context)) -> egui::FullOutput {
        let mut body = Some(body);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let body = body.take().expect("the frame runs once");
            body(ui.ctx());
        });
        output.textures_delta.clear();
        output
    }

    #[test]
    fn the_headline_names_the_caller_and_the_number() {
        let mut view = CallBarView::default();
        assert_eq!(view.headline(), "an incoming call");

        view.number = "6666".to_string();
        assert_eq!(view.headline(), "6666");

        view.caller = "Alice".to_string();
        assert_eq!(view.headline(), "Alice (6666)");
    }

    #[test]
    fn a_name_that_is_the_number_is_not_said_twice() {
        // Most servers send no name at all, and the number is what the
        // caller is known by: "6666 (6666)" says nothing twice.
        let view = CallBarView {
            call_id: "call-1".to_string(),
            account: "104".to_string(),
            number: "6666".to_string(),
            caller: "6666".to_string(),
        };
        assert_eq!(view.headline(), "6666");
    }

    #[test]
    fn the_bar_is_not_saying_anything_until_it_is_opened() {
        let (mut bar, _wake) = bar_with_a_wake();
        assert!(!bar.is_open());
        assert_eq!(bar.headline(), None);

        bar.open(CallBarView {
            call_id: "call-1".to_string(),
            account: "104".to_string(),
            number: "6666".to_string(),
            caller: "Alice".to_string(),
        });
        assert!(bar.is_open());
        assert_eq!(bar.headline(), Some("Alice (6666)".to_string()));

        bar.close();
        assert!(!bar.is_open());
        assert_eq!(bar.headline(), None);
    }

    #[test]
    fn what_the_user_did_comes_back_once() {
        let (bar, _wake) = bar_with_a_wake();
        assert!(bar.take_actions().is_empty());

        bar.actions.send(CallBarAction::Answer).expect("sent");
        let actions = bar.take_actions();
        assert_eq!(actions, vec![CallBarAction::Answer]);
        assert!(bar.take_actions().is_empty(), "the action came back twice");
    }

    #[test]
    fn the_window_is_asked_for_in_every_frame() {
        // A viewport that is not asked for again is thrown away, and
        // the window is only made when the app draws a frame.
        let (bar, _wake) = bar_with_a_wake();
        let ctx = context();

        let output = frame(&ctx, |ctx| bar.show(ctx));

        assert!(
            output.viewport_output.contains_key(&viewport_id()),
            "the bar did not ask for its window"
        );
    }

    #[test]
    fn the_window_is_shown_by_a_command_and_only_when_it_changes() {
        // The app's own window may be hidden, and a hidden window is
        // given the logic and no picture — so the bar's window is shown
        // by a command, and the command is not sent every frame.
        let (mut bar, _wake) = bar_with_a_wake();
        let ctx = context();

        let output = frame(&ctx, |ctx| {
            bar.show(ctx);
            bar.sync_visibility(ctx);
        });
        assert!(
            !bar_commands(&output)
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Visible(true))),
            "the bar was on the screen before there was a call"
        );

        bar.open(CallBarView {
            call_id: "call-1".to_string(),
            account: "104".to_string(),
            number: "6666".to_string(),
            caller: "Alice".to_string(),
        });
        let output = frame(&ctx, |ctx| {
            bar.show(ctx);
            bar.sync_visibility(ctx);
        });
        let commands = bar_commands(&output);
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Visible(true))),
            "the bar was not put on the screen"
        );

        // And again: the window is already on the screen, so nothing is
        // said about it a second time.
        let output = frame(&ctx, |ctx| {
            bar.show(ctx);
            bar.sync_visibility(ctx);
        });
        assert!(
            !bar_commands(&output)
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Visible(true))),
            "the bar was told about the screen twice"
        );

        // The call is over: the bar goes away.
        bar.close();
        let output = frame(&ctx, |ctx| {
            bar.show(ctx);
            bar.sync_visibility(ctx);
        });
        assert!(
            bar_commands(&output)
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Visible(false))),
            "the bar was not taken off the screen"
        );
    }
}
