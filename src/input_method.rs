//! `zwp_input_method_v2` — the input-method-v2 protocol client.
//!
//! `InputMethodState` is a `Resource`; the module binds `ZwpInputMethodManagerV2`
//! conditionally from `Globals`, creates the `ZwpInputMethodV2` object, and
//! registers a system for `ZwpInputMethodV2Event`.
//!
//! Inbound state is double-buffered: `activate`/`deactivate`/`content_type`
//! events fill a pending context that is applied on `done`. Surrounding text
//! and change cause are not tracked yet.

use mecha_wayland::prelude::*;

/// The text-input context the compositor reports for the focused field.
///
/// Inbound state is double-buffered: events fill `pending`, `Done` copies it
/// into `current`. Fields mirror the protocol's "initial values".
#[derive(Debug, Clone)]
pub struct InputMethodContext {
    /// `true` once `activate` has applied (on a `done`); `false` after
    /// `deactivate` applies. Drives whether taps route through IM2.
    pub active: bool,
    /// The focused field's content purpose; picks the view shown on activate.
    pub purpose: ZwpTextInputV3ContentPurpose,
}

impl Default for InputMethodContext {
    fn default() -> Self {
        Self {
            active: false,
            purpose: ZwpTextInputV3ContentPurpose::Normal,
        }
    }
}

/// All state the input-method-v2 client owns. Lives on the app as a `Resource`.
#[derive(Default)]
pub struct InputMethodState {
    /// The per-seat `zwp_input_method_v2` object; `None` until both the manager
    /// and a seat are available.
    pub input_method: Option<ZwpInputMethodV2>,

    /// The applied (post-`done`) inbound context — what the keyboard reads.
    pub current: InputMethodContext,
    /// The pending inbound context — what events are filling this batch.
    pending: InputMethodContext,

    /// Number of `done` events received. This is the serial echoed back in
    /// `commit(serial)`.
    pub serial: u32,

    /// `true` after `unavailable` — the object is inert.
    pub inert: bool,
}

impl Resource for InputMethodState {}

impl InputMethodState {
    /// Whether text taps should commit through IM2 rather than the
    /// virtual-keyboard-v1 transport. True only when active and not inert.
    pub fn should_commit(&self) -> bool {
        !self.inert && self.input_method.is_some() && self.current.active
    }
}

// ── keyboard surface visibility ──────────────────────────────────────────
//
// The keyboard surface's shown/hidden state lives in a `Resource` so that
// both the inbound IM2 state machine (`activate`/`deactivate` applied at
// `done`) and the debug visibility bar flip the *same* flag through the
// *same* setter. A `Signal` (`KeyboardVisibilityChanged`) is turned into a
// broadcast `Event` (`ApplyKeyboardVisibility`) by a system, which the
// `Shell` widget subscribes to in order to set `Display::Hidden`/`Flex` on
// the keyboard subtree. This mirrors the `theme` crate's `ThemeChanged` →
// `ApplyTheme` bridge — the only generic resource→widget path the app core
// exposes — so the bar and activate/deactivate share one mechanism (DRY).

/// The keyboard surface's shown/hidden state. Driven by IM2
/// `activate`/`deactivate` (via `Done`) and force-toggled by the debug
/// visibility bar; both write through [`KeyboardVisibilityExt`].
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyboardVisibility {
    pub visible: bool,
}
impl Resource for KeyboardVisibility {}

/// `Signal`: the visibility flag changed → fan out `ApplyKeyboardVisibility`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyboardVisibilityChanged;
impl Signal for KeyboardVisibilityChanged {}

/// `Event`: re-apply the current visibility to the surface. Broadcast to the
/// root and every descendant; the `Shell` widget handles it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyKeyboardVisibility;
impl Event for ApplyKeyboardVisibility {}

/// The single visibility setter (DRY): IM2 `Done` and the debug bar both go
/// through here. Implemented for `App` (systems) and `Context` (widget
/// handlers) so the same call reads at every site — mirroring `theme`'s
/// `AppThemeExt` / `ContextThemeExt` pair.
pub trait KeyboardVisibilityExt {
    /// The current visibility flag.
    fn keyboard_visible(&self) -> bool;
    /// Set the flag and signal the change (no-op if already `visible`).
    fn set_keyboard_visibility(&mut self, visible: bool);
    /// Flip the current visibility. Default method: every implementor gets it.
    fn toggle_keyboard_visibility(&mut self) {
        let next = !self.keyboard_visible();
        self.set_keyboard_visibility(next);
    }
}

impl KeyboardVisibilityExt for App {
    fn keyboard_visible(&self) -> bool {
        self.resource::<KeyboardVisibility>().visible
    }
    fn set_keyboard_visibility(&mut self, visible: bool) {
        if self.resource::<KeyboardVisibility>().visible == visible {
            return;
        }
        self.resource_mut::<KeyboardVisibility>().visible = visible;
        self.signal(KeyboardVisibilityChanged);
        tracing::info!(visible, "keyboard visibility");
    }
}

impl<W: Widget> KeyboardVisibilityExt for Context<'_, W> {
    fn keyboard_visible(&self) -> bool {
        self.resource::<KeyboardVisibility>().visible
    }
    fn set_keyboard_visibility(&mut self, visible: bool) {
        if self.resource::<KeyboardVisibility>().visible == visible {
            return;
        }
        self.resource_mut::<KeyboardVisibility>().visible = visible;
        self.signal(KeyboardVisibilityChanged);
        tracing::info!(visible, "keyboard visibility");
    }
}

/// `Signal`: a text field gained focus, or changed purpose while focused.
/// The keyboard picks the view to show from the purpose.
#[derive(Debug, Clone, Copy)]
pub struct TextInputFocused {
    pub purpose: ZwpTextInputV3ContentPurpose,
}
impl Signal for TextInputFocused {}

/// System: on `KeyboardVisibilityChanged`, broadcast `ApplyKeyboardVisibility`
/// to the root and every descendant so the `Shell` handler re-applies
/// `Display`. Mirrors `theme::on_theme_changed`.
fn on_keyboard_visibility_changed(app: &mut App, _: &KeyboardVisibilityChanged) {
    let mut targets = vec![app.root()];
    targets.extend(app.tree().descendants(app.root()));
    app.emit(ApplyKeyboardVisibility, targets);
}

/// Module that binds the input-method manager global, creates the IM object,
/// and handles `ZwpInputMethodV2Event`. Install after `WaylandModule`.
pub struct InputMethodModule;

impl Module for InputMethodModule {
    fn install(self, app: &mut App) {
        app.insert_resource(InputMethodState::default());
        // Keyboard surface visibility — starts hidden; IM2 `activate` and the
        // debug visibility bar flip it through `KeyboardVisibilityExt`.
        app.insert_resource(KeyboardVisibility { visible: false });
        app.system(on_keyboard_visibility_changed);
        init(app);
        app.system(on_input_method_event);
    }
}

/// Try to bind the input-method manager and create the IM object.
fn init(app: &mut App) {
    if app.resource::<InputMethodState>().input_method.is_some()
        || app.resource::<InputMethodState>().inert
    {
        return;
    }

    let seat = *app.resource::<WlSeat>();

    // The manager is an optional global — not every compositor supports
    // input-method-v2. Bind it from `Globals` if advertised.
    let manager = app
        .resource::<Globals>()
        .find(ZwpInputMethodManagerV2::NAME)
        .cloned()
        .map(|g| {
            let (globals, mut wl) = app.query::<(Res<Globals>, ResMut<Wayland>)>();
            globals.bind::<ZwpInputMethodManagerV2>(&g, &mut wl)
        });

    let Some(manager) = manager else {
        tracing::warn!("compositor does not advertise zwp_input_method_manager_v2");
        return;
    };

    let im = {
        let mut wl = app.resource_mut::<Wayland>();
        manager.get_input_method(&mut wl, seat)
    };

    tracing::info!("input-method-v2 bound");
    app.resource_mut::<InputMethodState>().input_method = Some(im);
}

/// Handle the inbound double-buffered state machine: events fill `pending`,
/// `done` applies it to `current` and bumps the serial, `unavailable` marks the
/// object inert.
fn on_input_method_event(app: &mut App, event: &ZwpInputMethodV2Event) {
    // Both are computed inside the `InputMethodState` borrow and acted on
    // after the guard drops, since signalling needs `&mut App`.
    let mut focused: Option<TextInputFocused> = None;
    let apply_visible: Option<bool> = {
        let st = &mut app.resource_mut::<InputMethodState>();
        if st.inert {
            return;
        }
        match event {
            ZwpInputMethodV2Event::Activate { .. } => {
                // Activate resets all prior inbound state then arms active.
                st.pending = InputMethodContext {
                    active: true,
                    ..Default::default()
                };
                tracing::info!("input-method: activate (pending)");
                None
            }
            ZwpInputMethodV2Event::Deactivate { .. } => {
                st.pending.active = false;
                tracing::info!("input-method: deactivate (pending)");
                None
            }
            ZwpInputMethodV2Event::SurroundingText { .. }
            | ZwpInputMethodV2Event::TextChangeCause { .. } => None,
            ZwpInputMethodV2Event::ContentType { purpose, .. } => {
                st.pending.purpose = *purpose;
                None
            }
            ZwpInputMethodV2Event::Done { .. } => {
                let next = st.pending.clone();
                let prev = std::mem::replace(&mut st.current, next);
                st.serial = st.serial.wrapping_add(1);
                let now = &st.current;
                tracing::info!(
                    active = now.active,
                    serial = st.serial,
                    "input-method: state applied"
                );

                if now.active && (!prev.active || prev.purpose != now.purpose) {
                    focused = Some(TextInputFocused {
                        purpose: now.purpose,
                    });
                }
                // Only an activate/deactivate moves the surface, so other
                // `done`s (e.g. surrounding-text updates) don't undo a manual
                // toggle from the visibility bar.
                (prev.active != now.active).then_some(now.active)
            }
            ZwpInputMethodV2Event::Unavailable { .. } => {
                st.inert = true;
                tracing::warn!("input-method: unavailable; object now inert");
                None
            }
        }
    };
    if let Some(focused) = focused {
        app.signal(focused);
    }
    if let Some(visible) = apply_visible {
        app.set_keyboard_visibility(visible);
    }
}

// ── outbound edits ────────────────────────────────────────────────────────

// Text commit is done by `im_commit_text` in main.rs, which sends
// `commit_string` + `commit(serial)` over Wayland.
