//! The system tray icon — the softphone's home in the notification area.
//!
//! The window can be hidden and the app keeps working: it stays
//! registered with the server, the lamps keep their state, and the calls
//! still come in. A call that comes in while the window is hidden brings
//! the window back (M6.30).
//!
//! The tray is also the way **out**. The window's own close button puts
//! the app in the tray instead of ending it, so the menu carries the
//! **Quit** — an app that hides itself has to say where it went.
//!
//! The icon is a lamp with a handset on it, and its colour says what the
//! app is doing: grey when it is not registered, green when it is, amber
//! when a call is waiting.

use std::sync::{Mutex, OnceLock};

use qsoftphone_core::wake::Wake;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// What the user asked for in the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// Bring the window back.
    Show,
    /// Hide the window — the app keeps running.
    Hide,
    /// End the app.
    Quit,
}

/// What the icon says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    /// The app is not registered with the server.
    Idle,
    /// The app is registered and there is nothing to say.
    Ready,
    /// A call is waiting to be answered.
    Calling,
}

/// The size of the icon. Windows asks for 32 pixels and scales it.
const SIZE: u32 = 32;

/// The id of the **Show** item of the menu.
const SHOW: &str = "show";
/// The id of the **Hide** item of the menu.
const HIDE: &str = "hide";
/// The id of the **Quit** item of the menu.
const QUIT: &str = "quit";

/// The actions the tray has asked for.
///
/// A tray crate has **one** handler for the whole process, and setting
/// it turns **off** the channel the crate would otherwise send to — the
/// two are alternatives, not a pair:
///
/// > This will not receive any events if `set_event_handler` has been
/// > called with a `Some` value.
///
/// So the handler puts the action in here, and the app reads it from
/// here. The queue is global for the same reason the handler is: there
/// is one handler, so there is one queue.
static ACTIONS: OnceLock<Mutex<Vec<TrayAction>>> = OnceLock::new();

/// The queue the handler writes to and the app reads from.
fn queue() -> &'static Mutex<Vec<TrayAction>> {
    ACTIONS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Put an action in the queue.
///
/// The handler runs on the tray's own thread and the app reads the
/// queue on the window's thread: the queue is what the two share.
fn push(action: TrayAction) {
    let mut actions = queue().lock().expect("the tray queue is poisoned");
    actions.push(action);
}

/// Take everything the tray has asked for since the last time.
fn take_actions() -> Vec<TrayAction> {
    let mut actions = queue().lock().expect("the tray queue is poisoned");
    std::mem::take(&mut *actions)
}

/// The tray icon and its menu.
pub struct Tray {
    /// The icon itself. Dropping it takes the icon off the screen.
    ///
    /// It is `None` in the tests, where a real icon in the notification
    /// area would be a mess — and in an app whose icon could not be
    /// made.
    icon: Option<TrayIcon>,
    /// The items of the menu.
    ///
    /// They are kept because the menu holds them: an item that is
    /// dropped is an item that is gone.
    items: Vec<MenuItem>,
    /// What the icon is showing now, so it is only redrawn when it
    /// changes: redrawing an icon is a message to the shell, and the
    /// shell is not a place to send messages for nothing.
    state: TrayState,
    /// The actions asked for in **this** tray.
    ///
    /// The actions the handler receives go into the global queue,
    /// because the handler is global. This one is the tray's own: it
    /// is what the tests put an action into, and it keeps a test from
    /// putting an action into every tray of the process at once.
    pending: Mutex<Vec<TrayAction>>,
}

impl Tray {
    /// The tray icon, its menu, and the wake that says something
    /// happened.
    ///
    /// The wake matters: the tray's own events arrive on their own
    /// thread, and nothing would look at them until the window happened
    /// to be drawn. The wake says *now*.
    pub fn new(wake: &Wake) -> Result<Self, String> {
        let show = MenuItem::with_id(MenuId::new(SHOW), "Show", true, None);
        let hide = MenuItem::with_id(MenuId::new(HIDE), "Hide", true, None);
        let quit = MenuItem::with_id(MenuId::new(QUIT), "Quit", true, None);

        let menu = Menu::new();
        for item in [&show, &hide, &quit] {
            menu.append(item)
                .map_err(|e| format!("the tray menu: {e}"))?;
        }

        // The ids the handler compares against. They are the items'
        // own ids, so the menu and the handler cannot drift apart.
        let show_id = show.id().clone();
        let hide_id = hide.id().clone();
        let quit_id = quit.id().clone();

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("qsoftphone")
            .with_icon(draw(TrayState::Idle))
            .build()
            .map_err(|e| format!("the tray icon: {e}"))?;

        // The tray's events are on their own thread. Without a wake the
        // window would not hear about a click until it was drawn for
        // some other reason.
        //
        // The handler does two things: it puts the event in the queue,
        // and it wakes the window. It cannot leave the event to the
        // crate's own channel — setting a handler turns that channel
        // off, and an event that is nowhere is an event that never
        // happened.
        let waker = wake.clone();
        TrayIconEvent::set_event_handler(Some(move |event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                // A click on the icon is the shortest way back to the
                // window: the tray is where the app is, and a click on
                // it means "show me".
                push(TrayAction::Show);
            }
            waker.wake();
        }));
        let waker = wake.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            // The ids are the items' own, so the menu and the handler
            // cannot drift apart.
            let action = if event.id == show_id {
                Some(TrayAction::Show)
            } else if event.id == hide_id {
                Some(TrayAction::Hide)
            } else if event.id == quit_id {
                Some(TrayAction::Quit)
            } else {
                None
            };
            if let Some(action) = action {
                push(action);
            }
            waker.wake();
        }));

        let mut tray = Self {
            icon: Some(icon),
            items: vec![show, hide, quit],
            state: TrayState::Idle,
            pending: Mutex::new(Vec::new()),
        };
        // The window is there to begin with: the menu says *Hide*.
        tray.set_window_visible(true);
        Ok(tray)
    }

    /// A tray that is not there.
    ///
    /// The tests use it: the tests are about the window, and a real
    /// icon in the notification area during a test run is a mess. The
    /// app behaves the same either way — there is a tray, and the close
    /// button hides the window.
    #[cfg(test)]
    pub fn absent() -> Self {
        Self {
            icon: None,
            items: Vec::new(),
            state: TrayState::Idle,
            pending: Mutex::new(Vec::new()),
        }
    }

    /// Ask for something, as the user would in the tray.
    ///
    /// The tests use it: the menu of a tray that is not there cannot be
    /// clicked, and the thing that is being tested is what the app
    /// does with what the tray says.
    #[cfg(test)]
    pub fn ask(&self, action: TrayAction) {
        let mut pending = self.pending.lock().expect("the tray is poisoned");
        pending.push(action);
    }

    /// What the user asked for, since the last time.
    pub fn poll(&self) -> Vec<TrayAction> {
        let mut actions = take_actions();
        let mut pending = self.pending.lock().expect("the tray is poisoned");
        actions.append(&mut pending);
        actions
    }

    /// Say what the app is doing, in the icon and in the tooltip.
    ///
    /// The icon is only redrawn when the state changes: every redraw is
    /// a message to the shell.
    pub fn set_state(&mut self, state: TrayState) {
        if self.state == state {
            return;
        }
        self.state = state;
        let Some(icon) = self.icon.as_ref() else {
            return;
        };
        // The icon and the tooltip are what the user sees; a failure to
        // change them is worth saying out loud rather than swallowing.
        if let Err(e) = icon.set_icon(Some(draw(state))) {
            tracing::warn!(error = %e, "the tray icon could not be changed");
        }
        let tooltip = match state {
            TrayState::Idle => "qsoftphone — not registered",
            TrayState::Ready => "qsoftphone — ready",
            TrayState::Calling => "qsoftphone — a call is waiting",
        };
        if let Err(e) = icon.set_tooltip(Some(tooltip.to_string())) {
            tracing::warn!(error = %e, "the tray tooltip could not be changed");
        }
        tracing::info!(state = ?state, "the tray says");
    }

    /// What the icon is saying now.
    ///
    /// The tests use it: what the icon says is the whole of the icon's
    /// job, and the job is proved by looking at it.
    #[cfg(test)]
    pub fn state(&self) -> TrayState {
        self.state
    }

    /// Whether the window is there: the menu says *Hide* only when
    /// there is a window to hide.
    pub fn set_window_visible(&mut self, visible: bool) {
        for item in &self.items {
            let id = item.id().0.as_str();
            if id == SHOW {
                item.set_enabled(!visible);
            } else if id == HIDE {
                item.set_enabled(visible);
            }
        }
    }
}
/// The icon: a lamp with a handset on it.
///
/// The colour says what the app is doing, and the lamp is the app's own
/// idea of a lamp: the Busy Lamp Field is what this app is about.
fn draw(state: TrayState) -> Icon {
    let (rgba, width, height) = pixels(state);
    Icon::from_rgba(rgba, width, height).expect("the icon is the size it says it is")
}

/// The pixels of the icon: the colour of the lamp, and the handset on
/// it.
fn pixels(state: TrayState) -> (Vec<u8>, u32, u32) {
    let (red, green, blue) = match state {
        TrayState::Idle => (74u8, 79u8, 92u8),
        TrayState::Ready => (58u8, 160u8, 108u8),
        TrayState::Calling => (233u8, 168u8, 48u8),
    };

    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            // The lamp: a disc.
            let disc = cover(
                distance(px, py, SIZE as f32 / 2.0, SIZE as f32 / 2.0),
                SIZE as f32 / 2.0 - 1.0,
            );
            if disc <= 0.0 {
                continue;
            }

            // The handset: a bar with an ear at each end, tilted.
            let bar = cover(segment_distance(px, py, 10.0, 8.0, 22.0, 24.0), 2.4);
            let near_ear = cover(distance(px, py, 8.6, 7.4), 4.0);
            let far_ear = cover(distance(px, py, 23.4, 24.6), 4.0);
            let handset = (bar + near_ear + far_ear).min(1.0) * disc;

            let mix = |from: u8| {
                let from = from as f32;
                (from + (255.0 - from) * handset).round().clamp(0.0, 255.0) as u8
            };
            let index = ((y * SIZE + x) * 4) as usize;
            rgba[index] = mix(red);
            rgba[index + 1] = mix(green);
            rgba[index + 2] = mix(blue);
            rgba[index + 3] = (disc * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }

    (rgba, SIZE, SIZE)
}

/// How much of a pixel is inside a shape of this radius.
///
/// The edge is one pixel wide and fades: a disc with a hard edge looks
/// like a staircase at this size.
fn cover(distance: f32, radius: f32) -> f32 {
    let edge = 0.5;
    if distance <= radius - edge {
        return 1.0;
    }
    if distance >= radius + edge {
        return 0.0;
    }
    let t = (distance - (radius - edge)) / (2.0 * edge);
    1.0 - t * t * (3.0 - 2.0 * t)
}

/// The distance from a point to another one.
fn distance(x: f32, y: f32, to_x: f32, to_y: f32) -> f32 {
    ((x - to_x).powi(2) + (y - to_y).powi(2)).sqrt()
}

/// The distance from a point to a line segment.
fn segment_distance(x: f32, y: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let dx = bx - ax;
    let dy = by - ay;
    let length = (dx * dx + dy * dy).sqrt();
    if length == 0.0 {
        return distance(x, y, ax, ay);
    }
    let t = (((x - ax) * dx + (y - ay) * dy) / (length * length)).clamp(0.0, 1.0);
    distance(x, y, ax + t * dx, ay + t * dy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_is_the_size_it_says() {
        let (_, width, height) = pixels(TrayState::Ready);
        assert_eq!(width, SIZE);
        assert_eq!(height, SIZE);
    }

    #[test]
    fn the_icon_says_what_the_app_is_doing() {
        // Three states, three colours: an icon that says the same thing
        // in three colours says nothing.
        let (ready, _, _) = pixels(TrayState::Ready);
        let (calling, _, _) = pixels(TrayState::Calling);
        assert_ne!(ready, calling);
    }

    #[test]
    fn the_centre_of_the_icon_is_the_handset() {
        // The handset is white on every state, and it is in the middle.
        let (rgba, _, _) = pixels(TrayState::Calling);
        let centre = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
        assert!(rgba[centre + 3] > 200, "the centre is not painted");
        assert!(
            rgba[centre] > 200 && rgba[centre + 1] > 200 && rgba[centre + 2] > 200,
            "the centre is not the handset"
        );
    }

    #[test]
    fn the_corner_of_the_icon_is_empty() {
        // The corners are outside the lamp: an icon with a square of
        // background in it is a square, not a lamp.
        let (rgba, _, _) = pixels(TrayState::Idle);
        assert_eq!(rgba[3], 0, "the corner is painted");
    }
}
