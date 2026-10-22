//! mechanix-keyboard — an on-screen keyboard built with themed widgets on the
//! mecha-wayland UI core.
//!
//! Builds a tree of themed `Widget` nodes: each key is a `Key` widget whose
//! background, label colour, and corner radius come from `MechanixTheme` /
//! `ColorRole`, and whose `on_theme` callback re-resolves them instantly when
//! the user toggles dark/light at runtime.
//!
//! The keymap (parsed from `resources/layout.yaml` by `layout.rs`) drives which
//! keys appear and what they do. View switching (Shift_L `locking`,
//! `show_symbols`, `show_eschars`) and one-shot modifier latching (Ctrl, Alt) are
//! supported. Text keys commit through `zwp_input_method_v2` when a text input
//! is focused, falling back to `zwp_virtual_keyboard_v1` keysym transport.

#![recursion_limit = "1024"]

use std::{collections::HashSet, time::Duration};

use mecha_wayland::prelude::*;

mod color_role;
mod font;
mod icons;
mod input_method;
mod key_style;
mod layout;
mod shape;
mod sound;
mod spacing;
mod virtual_keyboard;

use crate::icons::Icons;
use font::Fonts;
use input_method::{ApplyKeyboardVisibility, KeyboardVisibilityExt, TextInputFocused};
use key_style::{KeyLook, KeyState};
use layout::{KeyAction, KeyFace, Keymap, Modifier};
use sound::KeySound;
/// The default view: shown when the keyboard first appears and for text
/// fields with no more specific purpose. Must exist in `layout.yaml`.
const BASE_VIEW: &str = "base";
/// The view shown for `terminal`-purpose fields.
const TERMINAL_VIEW: &str = "terminal";
/// The view shown for `number`/`digits`-purpose fields.
const NUMBER_VIEW: &str = "numbers";

/// The view to show for a focused text field's content purpose.
fn view_for_purpose(purpose: ZwpTextInputV3ContentPurpose) -> &'static str {
    match purpose {
        ZwpTextInputV3ContentPurpose::Terminal => TERMINAL_VIEW,
        ZwpTextInputV3ContentPurpose::Number | ZwpTextInputV3ContentPurpose::Digits => NUMBER_VIEW,
        _ => BASE_VIEW,
    }
}

/// The debug visibility bar's height — also the collapsed surface height.
///
/// The layer surface is created at this height (keyboard hidden at launch,
/// no text input focused) and grows to the keymap's `keyboard.height` once a
/// text input activates or the bar is tapped.
const BAR_HEIGHT: f32 = 8.0;

/// Shared state for the one-shot modifier latches. Stored as a `Resource` so
/// both the Key widget's `on_theme` handler and the Keyboard's `dispatch`
/// function can read/write it.
#[derive(Default)]
struct LatchedState {
    armed: HashSet<Modifier>,
}
impl Resource for LatchedState {}

impl LatchedState {
    fn get(&self, m: Modifier) -> bool {
        self.armed.contains(&m)
    }

    /// Flip `m`'s latch; returns whether it is now armed.
    fn toggle(&mut self, m: Modifier) -> bool {
        if self.armed.remove(&m) {
            false
        } else {
            self.armed.insert(m);
            true
        }
    }
}

// ── Key widget (themed button) ────────────────────────────────────────────

/// What a key looks like — determines the `ColorRole` pair it draws with.
/// Derived from the key's `KeyAction` so the visual matches the behaviour.
#[derive(Clone, Copy)]
enum KeyKind {
    /// Regular character key (letters, digits, punctuation).
    Normal,
    /// Non-typing key: modifiers (Ctrl, Alt), view switches (Shift, abc, 123),
    /// and actions not wired yet (prefs, emoji).
    Function,
}

/// Classify a key's action into a visual kind for theme colour selection.
fn kind_of(action: &KeyAction) -> KeyKind {
    match action {
        KeyAction::EmitKeysym(_) | KeyAction::EmitText(_) => KeyKind::Normal,
        KeyAction::LatchModifier(_)
        | KeyAction::SetView(_)
        | KeyAction::ToggleView { .. }
        | KeyAction::Unhandled(_) => KeyKind::Function,
    }
}

/// Resolve a `KeyKind` to its (background, foreground) colour roles. A latched
/// modifier uses the `Primary` palette so the armed state reads at a glance.
fn roles_for(kind: KeyKind, state: KeyState) -> (ColorRole, ColorRole) {
    match state {
        KeyState::Latched => (ColorRole::Primary, ColorRole::OnPrimary),
        KeyState::Normal => match kind {
            KeyKind::Normal => (ColorRole::SurfaceContainerHigh, ColorRole::OnSurface),
            KeyKind::Function => (
                ColorRole::SecondaryContainer,
                ColorRole::OnSecondaryContainer,
            ),
        },
    }
}

fn key_roles(look: KeyLook, kind: KeyKind, state: KeyState) -> (ColorRole, ColorRole) {
    let style = look.style(state);
    let (bg, fg) = roles_for(kind, state);
    (
        style.background.unwrap_or(bg),
        style.foreground.unwrap_or(fg),
    )
}

/// A themed key button: a `div` with a `Paint::Quad` background resolved from
/// `ColorRole`, a centred text label, and an `on_theme` callback that
/// re-resolves both colours when the theme mode flips at runtime.
///
/// The key's `Clicked` handler dispatches its `KeyAction` through the virtual
/// keyboard / input method / view-switch logic.
struct Key {
    label: Label,
    look: KeyLook,
}

#[derive(Clone, Copy)]
enum Label {
    Text(Handle<Text>),
    Icon(Handle<Icon>),
}

/// Recolour a key's label, whichever kind it is.
fn set_label_color<W: Widget>(ctx: &mut Context<'_, W>, label: Label, fg: Color) {
    match label {
        Label::Text(h) => ctx.at(h).unwrap().set_color(fg),
        Label::Icon(h) => ctx.at(h).unwrap().set_color(fg),
    }
}

struct KeyBuilder {
    font: FontId,
    font_size: u16,
    key: layout::Key,
    radius: f32,
}

impl Build for KeyBuilder {
    type Widget = Key;
}

impl Widget for Key {
    type Builder = KeyBuilder;
    fn build(b: KeyBuilder, me: Handle<Self>, s: &mut Spawner<'_, Self>) -> Self {
        let kind = kind_of(&b.key.action);
        let look = b.key.look;
        let (bg_role, fg_role) = key_roles(look, kind, KeyState::Normal);
        let radius = b.radius;

        *s.component_mut::<LayoutStyle>(me).unwrap() = LayoutStyle::default()
            .center()
            // Outline width is the flex-grow weight; only ratios within a row
            // matter. Kept unscaled so a row's weights never sum below 1, which
            // would leave part of the row's free space unfilled.
            .grow(b.key.width)
            .padding_all(px(2.0));

        *s.component_mut::<Paint>(me).unwrap() =
            Paint::Quad(Quad::new(s.color(bg_role)).radius(radius));

        *s.component_mut::<PaintAnimationSettings>(me).unwrap() = PaintAnimationSettings::new(
            AnimationTime::Duration(Duration::from_millis(150)),
            Easing::Linear,
        );

        let fg = s.color(fg_role);
        let label = match &b.key.face {
            KeyFace::Icon(name) => match s.resource::<Icons>().get(name) {
                Some(sprite) => Label::Icon(s.spawn(me, icon(sprite).color(fg))),
                None => {
                    tracing::warn!("no icon named {name:?} in resources/icons; drawing empty key");
                    // set empty labe if icons not found
                    Label::Text(s.spawn(me, text(b.font, "").color(fg).size(b.font_size)))
                }
            },
            KeyFace::Text(t) => {
                Label::Text(s.spawn(me, text(b.font, t).color(fg).size(b.font_size)))
            }
        };

        let latch = match b.key.action {
            KeyAction::LatchModifier(m) => Some(m),
            _ => None,
        };
        let is_latch = latch.is_some();
        s.on_theme(me, move |ctx| {
            let state =
                KeyState::latched_if(latch.is_some_and(|m| ctx.resource::<LatchedState>().get(m)));
            let (bg_role, fg_role) = key_roles(look, kind, state);
            let bg = ctx.color(bg_role);
            let fg = ctx.color(fg_role);
            ctx.set_paint(Paint::Quad(Quad::new(bg).radius(radius)));
            set_label_color(ctx, label, fg);
        });

        if !is_latch {
            s.on::<Press>(me, move |ctx, _| {
                let bg = ctx.color(bg_role);
                let fg = ctx.color(fg_role);

                set_label_color(ctx, label, bg);
                ctx.set_paint(Paint::Quad(Quad::new(fg).radius(radius)));
            });
            s.on::<Release>(me, move |ctx, _| {
                let bg = ctx.color(bg_role);
                let fg = ctx.color(fg_role);

                set_label_color(ctx, label, fg);
                ctx.set_paint(Paint::Quad(Quad::new(bg).radius(radius)));
            });
        }

        Key { label, look }
    }
}

// ── Keyboard widget ───────────────────────────────────────────────────────

/// The on-screen keyboard: manages the current view, the one-shot modifier latches,
/// and spawns themed `Key` widgets from the parsed keymap. All views are
/// spawned at build time; non-current views are `Display::Hidden` and toggled
/// visible on view-switch key taps.
struct Keyboard {
    /// Every view, in keymap order.
    views: Vec<ViewNode>,
    /// Index into `views` of the view currently shown.
    current_view: usize,
    /// Every latching modifier key across all views, for armed-state repaint on
    /// latch toggle.
    latch_keys: Vec<(Modifier, Handle<Key>)>,
    /// Key corner radius, for repainting latch keys.
    radius: f32,
}

/// One spawned view: its keymap name and the node toggled
/// `Display::Hidden`/`Flex` on switch.
struct ViewNode {
    name: String,
    node: Handle<Div>,
}

impl Keyboard {
    /// The index of the view with this name, for storing as the current view.
    /// Resolved once per switch so later reads stay O(1) array indexing.
    fn index_of(&self, name: &str) -> Option<usize> {
        self.views.iter().position(|v| v.name == name)
    }

    fn current_view_name(&self) -> &str {
        &self.views[self.current_view].name
    }
}

/// Ask the keyboard to show a named view.
struct SwitchView {
    view: &'static str,
}
impl Event for SwitchView {}

/// System: a text field gained focus or changed purpose — show the view for
/// its purpose.
fn on_text_input_focused(app: &mut App, focused: &TextInputFocused) {
    let view = view_for_purpose(focused.purpose);
    let targets: Vec<NodeId> = app.widgets::<Keyboard>().map(|(id, _)| id).collect();
    app.emit(SwitchView { view }, targets);
}

struct KeyboardBuilder {
    fonts: Fonts,
    keymap: Keymap,
}

impl Build for KeyboardBuilder {
    type Widget = Keyboard;
}

impl Widget for Keyboard {
    type Builder = KeyboardBuilder;
    fn build(b: KeyboardBuilder, me: Handle<Self>, s: &mut Spawner<'_, Self>) -> Self {
        // Keyboard container: column filling the window, surface background.
        // Its gap only separates views, and just one view is ever shown.
        let style = b.keymap.keyboard;
        *s.component_mut::<LayoutStyle>(me).unwrap() = LayoutStyle::default()
            .column()
            .fill()
            .gap(px(style.row_gap))
            .padding_all(px(style.padding));
        let bg_role = b.keymap.keyboard.background_color;
        *s.component_mut::<Paint>(me).unwrap() = Paint::Quad(Quad::new(s.color(bg_role)));

        s.on_theme(me, move |ctx| {
            let bg = ctx.color(bg_role);
            ctx.set_paint(Paint::Quad(Quad::new(bg)));
        });

        let initial_view = b
            .keymap
            .views
            .iter()
            .position(|v| v.name == BASE_VIEW)
            .unwrap_or(0);

        // ── spawn all views ─────────────────────────────────────────────
        let mut views = Vec::with_capacity(b.keymap.views.len());
        let mut latch_keys = Vec::new();

        for (vi, view) in b.keymap.views.iter().enumerate() {
            let view_style = LayoutStyle::default()
                .column()
                .fill()
                .gap(px(style.row_gap));
            let view_style = if vi == initial_view {
                view_style
            } else {
                view_style.hidden()
            };
            let view_div = s.spawn(me, div().style(view_style));
            views.push(ViewNode {
                name: view.name.clone(),
                node: view_div,
            });

            for row in &view.rows {
                let row_div = s.spawn(
                    view_div,
                    div().style(LayoutStyle::default().row().fill().gap(px(style.gap))),
                );

                for key in &row.keys {
                    // The label keeps its normal font while latched.
                    let label_style = key.look.style(KeyState::Normal);
                    let key_handle = s.spawn(
                        row_div,
                        KeyBuilder {
                            font: b.fonts.id(label_style.font.unwrap_or(style.font)),
                            font_size: label_style.font_size.unwrap_or(style.font_size),
                            key: key.clone(),
                            radius: style.radius,
                        },
                    );

                    if let KeyAction::LatchModifier(m) = key.action {
                        latch_keys.push((m, key_handle));
                    }

                    // Clone the action into the Clicked closure. Each key
                    // gets its own handler that dispatches through the
                    // Keyboard's action logic.

                    /*
                    NOTE: this is why action has to be handled here instead of
                    in Key::build
                    */
                    let action = key.action.clone();
                    s.on::<Clicked>(key_handle, move |ctx, _| {
                        ctx.resource::<KeySound>().play();
                        dispatch(ctx, &action);
                    });
                }
            }
        }

        s.on::<SwitchView>(me, |ctx, event| {
            switch_view(ctx, event.view);
        });
        Keyboard {
            views,
            current_view: initial_view,
            latch_keys,
            radius: style.radius,
        }
    }
}

// ── action dispatch ───────────────────────────────────────────────────────

/// Route a tapped key's action. View switches and modifier latches mutate the
/// Keyboard widget state; every other action is a keystroke the virtual
/// keyboard emits (consuming any latched modifier).
fn dispatch(ctx: &mut Context<'_, Keyboard>, action: &KeyAction) {
    match action {
        KeyAction::SetView(name) => {
            switch_view(ctx, name);
        }
        KeyAction::ToggleView { lock, unlock } => {
            let target = if ctx.me().current_view_name() == lock {
                unlock
            } else {
                lock
            };
            switch_view(ctx, target);
        }
        KeyAction::LatchModifier(m) => {
            toggle_latch(ctx, *m);
        }
        KeyAction::EmitKeysym(ks) => {
            let latched_mask = latched_mask(ctx);
            // Printable keysyms commit via IM2; control keysyms fall back to
            // the virtual-keyboard-v1 keysym transport. A latched modifier
            // forces the keycode path — IM2 commits plain text and would drop
            // the modifier.
            if latched_mask == 0
                && let Some(text) = virtual_keyboard::keysym_text(*ks)
                && im_commit_text(ctx, &text)
            {
                tracing::info!(input = %text, "input method");
            } else {
                vk_emit_keysym(ctx, *ks, latched_mask);
            }
            consume_latch(ctx);
        }
        KeyAction::EmitText(text) => {
            let latched_mask = latched_mask(ctx);
            if latched_mask == 0 && im_commit_text(ctx, text) {
                tracing::info!("Input method: {text}");
            } else {
                for ch in text.chars() {
                    vk_emit_keysym(ctx, xkbcommon::xkb::Keysym::from_char(ch), latched_mask);
                }
            }
            consume_latch(ctx);
        }
        KeyAction::Unhandled(name) => {
            tracing::info!(action = %name, "tapped key with no wired action");
        }
    }
}

/// The combined modifier mask of every currently-latched modifier, to OR into
/// a keystroke. Reads from the `LatchedState` resource.
fn latched_mask(ctx: &Context<'_, Keyboard>) -> u32 {
    let latched = ctx.resource::<LatchedState>();
    let masks = &ctx
        .resource::<virtual_keyboard::VirtualKeyboardState>()
        .mod_masks;
    Modifier::ALL
        .into_iter()
        .filter(|&m| latched.get(m))
        .filter_map(|m| masks.get(&m).copied())
        .fold(0, |acc, mask| acc | mask)
}

/// Try to commit text through `zwp_input_method_v2`. Returns `true` if the edit
/// was sent (IM bound + active). Uses `Context`'s resource access — borrows each
/// resource sequentially to avoid conflicts.
fn im_commit_text(ctx: &mut Context<'_, Keyboard>, text: &str) -> bool {
    if !ctx
        .resource::<input_method::InputMethodState>()
        .should_commit()
    {
        return false;
    }
    // Copy out the IM object (Copy) and serial.
    let (im_obj, serial) = {
        let st = ctx.resource::<input_method::InputMethodState>();
        (st.input_method, st.serial)
    };
    let Some(im) = im_obj else {
        return false;
    };
    // Send the requests via Wayland.
    let mut wl = ctx.resource_mut::<Wayland>();
    im.commit_string(&mut wl, text);
    im.commit(&mut wl, serial);
    true
}

/// Send one keysym as a keycode down+up via `zwp_virtual_keyboard_v1`, holding
/// the keystroke's modifiers around it and clearing them after.
fn vk_emit_keysym(ctx: &mut Context<'_, Keyboard>, ks: xkbcommon::xkb::Keysym, latched_mask: u32) {
    let name = xkbcommon::xkb::keysym_get_name(ks);
    // Copy out the keystroke + VK handle (all Copy) from the shared resource.
    let (vkbd, stroke) = {
        let vk = ctx.resource::<virtual_keyboard::VirtualKeyboardState>();
        (vk.virtual_keyboard, vk.keycodes.get(&ks).copied())
    };
    let Some(vkbd) = vkbd else {
        tracing::warn!(keysym = %name, "virtual keyboard not ready; key dropped");
        return;
    };
    let Some(stroke) = stroke else {
        tracing::warn!(keysym = %name, "keysym absent from keymap; not typed");
        return;
    };
    let mods = stroke.mods | latched_mask;
    let time = (std::time::Instant::now()
        - ctx
            .resource::<virtual_keyboard::VirtualKeyboardState>()
            .start_time)
        .as_millis() as u32;
    let mut wl = ctx.resource_mut::<Wayland>();
    if mods != 0 {
        vkbd.modifiers(&mut wl, mods, 0, 0, 0);
    }
    vkbd.key(
        &mut wl,
        time,
        stroke.code,
        u32::from(WlKeyboardKeyState::Pressed),
    );
    vkbd.key(
        &mut wl,
        time,
        stroke.code,
        u32::from(WlKeyboardKeyState::Released),
    );
    if mods != 0 {
        vkbd.modifiers(&mut wl, 0, 0, 0, 0);
    }
    tracing::info!(keysym = %name, code = stroke.code, mods, "typed");
}

/// Clear every modifier latch after a keystroke fires (one-shot).
fn consume_latch(ctx: &mut Context<'_, Keyboard>) {
    if !ctx.resource::<LatchedState>().armed.is_empty() {
        ctx.resource_mut::<LatchedState>().armed.clear();
        repaint_latch_keys(ctx);
    }
}

/// Switch the current view to the named one, hiding the old and showing the
/// new via `Display::Hidden` / `Display::Flex`.
fn switch_view(ctx: &mut Context<'_, Keyboard>, target: &str) {
    let Some(idx) = ctx.me().index_of(target) else {
        tracing::warn!(view = %target, "view switch to unknown view; ignored");
        return;
    };
    let old_idx = ctx.me().current_view;
    if idx == old_idx {
        return;
    }
    let (old, new) = {
        let me = ctx.me();
        (me.views[old_idx].node, me.views[idx].node)
    };
    ctx.at(old).unwrap().set_display(Display::Hidden);
    ctx.at(new).unwrap().set_display(Display::Flex);
    ctx.me().current_view = idx;
    tracing::info!(view = %target, "switched view");
}

/// Toggle a modifier's one-shot latch and repaint every latching key across
/// all views to reflect the armed/disarmed state.
fn toggle_latch(ctx: &mut Context<'_, Keyboard>, m: Modifier) {
    let armed = ctx.resource_mut::<LatchedState>().toggle(m);
    repaint_latch_keys(ctx);
    tracing::info!(modifier = ?m, armed, "latch toggled");
}

/// Repaint all latching modifier keys to reflect the current latched state.
fn repaint_latch_keys(ctx: &mut Context<'_, Keyboard>) {
    let radius = ctx.me().radius;

    let keys = ctx.me().latch_keys.clone();
    for (m, k) in keys {
        let state = KeyState::latched_if(ctx.resource::<LatchedState>().get(m));
        let mut key = ctx.at(k).unwrap();
        let (label, look) = (key.me().label, key.me().look);
        let (bg_role, fg_role) = key_roles(look, KeyKind::Function, state);
        let bg = key.color(bg_role);
        let fg = key.color(fg_role);

        key.set_paint(Paint::Quad(Quad::new(bg).radius(radius)));
        set_label_color(ctx, label, fg);
    }
}

// ── Shell widget (layer-shell window) ─────────────────────────────────────

/// Spawns a `zwlr_layer_surface` anchored to the bottom edge of the output.
/// When no fixed width is configured it is stretched full-width (left + right
/// anchors); otherwise it is anchored to the bottom only at the configured
/// width. An exclusive zone of `-1` lets the surface overlap other content
/// without reserving compositor space. `KeyboardInteractivity::None` keeps the
/// OSK from stealing keyboard focus from the focused app; pointer taps on the
/// layer surface are still delivered.
struct Shell;

struct ShellBuilder {
    root: NodeId,
    fonts: Fonts,
    keymap: Keymap,
}

impl Build for ShellBuilder {
    type Widget = Shell;
}

impl Widget for Shell {
    type Builder = ShellBuilder;
    fn build(b: ShellBuilder, me: Handle<Self>, s: &mut Spawner<'_, Self>) -> Self {
        let style = b.keymap.keyboard;
        let kb_height = style.height;
        let (anchor, width) = match style.width {
            Some(w) => (Anchor::BOTTOM, px(w)),
            _ => (Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT, auto()),
        };
        let role = Role::Layer(LayerRole {
            layer: Layer::Top,
            anchor,
            exclusive_zone: -1,
            namespace: "mechanix-keyboard".into(),
            keyboard_interactivity: KeyboardInteractivity::None,
        });

        // Window column. The window's requested height starts at the bar
        // height (8px) so the layer surface is created collapsed — the
        // keyboard starts hidden (no text input is focused at launch) and
        // only the bar is on screen. The visibility handler re-sends
        // `set_size` to grow the surface to the keyboard height on show
        // and shrink it back to the bar height on hide, so no empty black
        // rectangle remains behind the bar when hidden.
        //
        // The surface height is otherwise compositor-driven (WSI → layout):
        // `request_layer_size` asks the compositor for a new size, it
        // configures, and `settle` reports `Resized` so the window's
        // `LayoutStyle` is settled by the compositor answer.
        let win = s.spawn_with(
            b.root,
            window().layout(
                LayoutStyle::default()
                    .column()
                    .justify(Justify::End)
                    .size(width, px(BAR_HEIGHT)),
            ),
            (role,),
        );
        let win_id = win.id();
        // Width to (re-)request on resize: `0` only stretches full-width
        // when left+right anchors are set; a fixed-width keyboard must
        // echo its configured width, since it is anchored to `BOTTOM` only.
        let req_w = match style.width {
            Some(w) => w as u32,
            None => 0,
        };

        let keyboard = s.spawn(
            win,
            KeyboardBuilder {
                fonts: b.fonts,
                keymap: b.keymap,
            },
        );

        // ── always-visible toggle bar ───────────────────────────────────
        //
        // A short, full-width bar below the keyboard that force-toggles
        // surface visibility through the *same* `KeyboardVisibilityExt`
        // mechanism IM2 `activate`/`deactivate` use. Always visible (never
        // `Display::Hidden`) so it remains a handle to bring the keyboard
        // back when no text input is focused.
        let bar = s.spawn(
            win,
            div()
                .style(
                    LayoutStyle::default()
                        .row()
                        .width(percent(100.0))
                        .height(px(BAR_HEIGHT))
                        .shrink(0.0),
                )
                // A visible fill so the bar reads as a tappable handle.
                .background(s.color(ColorRole::PrimaryContainer)),
        );
        s.on::<Clicked>(bar, |ctx, _| {
            ctx.toggle_keyboard_visibility();
        });

        // Apply visibility on every change — activate/deactivate or the bar.
        // Two things happen, through the *same* `KeyboardVisibilityExt` flip:
        //   1. The keyboard subtree is `Display::Hidden`/`Flex` so it takes
        //      no space when hidden.
        //   2. The layer surface itself is resized via `request_layer_size`
        //      so the on-screen surface collapses to just the bar (no empty
        //      black rectangle remains) when hidden, and grows back to the
        //      keyboard + bar height when shown. The compositor answers
        //      with a configure; the framework's `settle` reports
        //      `Resized` and lays out at the new size.
        s.on::<ApplyKeyboardVisibility>(me, move |ctx, _| {
            let visible = ctx.keyboard_visible();
            ctx.at(keyboard).unwrap().set_display(if visible {
                Display::Flex
            } else {
                Display::Hidden
            });
            // `0` width stretches full-width only when left+right anchors
            // are set; a fixed-width keyboard echoes its configured width.
            let h = if visible { kb_height } else { BAR_HEIGHT };
            let (mut surfaces, mut wl) = ctx.fetch::<(ResMut<Surfaces>, ResMut<Wayland>)>();
            surfaces.request_layer_size(win_id, req_w, h as u32, &mut wl);
        });

        // Apply the initial visibility (starts hidden). The change handler
        // above only fires on subsequent flips, so seed the keyboard's
        // `Display` here to match the resource's initial value.
        if !s.resource::<input_method::KeyboardVisibility>().visible
            && let Some(mut kstyle) = s.component_mut::<LayoutStyle>(keyboard)
        {
            kstyle.display = Display::Hidden;
        }

        Shell
    }
}

// ── main ──────────────────────────────────────────────────────────────────

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Load the keymap from layout.yaml (env var, local file, or bundled fallback).
    let keymap = layout::load_keymap();

    let mut app = App::new();
    app.add_module(LayoutModule)
        .add_module(PaintModule)
        .add_module(WindowModule)
        .add_module(InteractivityModule)
        .add_module(AnimationModule)
        .add_module(RenderModule::default())
        .insert_resource(Atlas::new());
    app.insert_resource(LatchedState::default());
    // Open the audio stream up front so the first tap has no device-open lag.
    app.insert_resource(KeySound::load());

    let theme = match keymap.keyboard.mode {
        ThemeMode::Dark => MechanixTheme::dark(),
        ThemeMode::Light => MechanixTheme::light(),
    };
    app.add_module(theme)
        .add_module(RingModule::default())
        .add_module(
            WaylandModule::new()
                .bind::<WlCompositor>()
                .bind::<ZwpLinuxDmabufV1>()
                .bind::<XdgWmBase>()
                .bind::<WlSeat>(),
        )
        .add_module(PresentationModule {
            app_id: "mecha.keyboard".into(),
            budget: Budget::default(),
        });

    // Virtual-keyboard-v1 and input-method-v2 modules — bind their globals
    // conditionally from `Globals` and handle their protocol events. These
    // must install after `WaylandModule` (provides `Globals` + `Wayland`) and
    // after the seat is bound.
    app.add_module(virtual_keyboard::VirtualKeyboardModule);
    app.add_module(input_method::InputMethodModule);
    app.system(on_text_input_focused);

    let fonts = Fonts::load(&mut app.resource_mut::<Atlas>());
    let icons = Icons::load(
        &mut app.resource_mut::<Atlas>(),
        keymap.keyboard.font_size as u32,
    );
    app.insert_resource(icons);

    let root = app.root();
    app.spawn(
        root,
        ShellBuilder {
            root,
            fonts,
            keymap,
        },
    );
    app.run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purpose_picks_view() {
        use ZwpTextInputV3ContentPurpose as P;
        assert_eq!(view_for_purpose(P::Terminal), TERMINAL_VIEW);
        assert_eq!(view_for_purpose(P::Number), NUMBER_VIEW);
        assert_eq!(view_for_purpose(P::Digits), NUMBER_VIEW);
        assert_eq!(view_for_purpose(P::Normal), BASE_VIEW);
        assert_eq!(view_for_purpose(P::Email), BASE_VIEW);
    }

    #[test]
    fn auto_switch_views_exist_in_bundled_layout() {
        let keymap = layout::load_keymap();
        for view in [BASE_VIEW, TERMINAL_VIEW, NUMBER_VIEW] {
            assert!(
                keymap.views.iter().any(|v| v.name == view),
                "layout.yaml has no `{view}` view"
            );
        }
    }
}
