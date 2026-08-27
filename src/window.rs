use interactivity::pointer::MouseButton;
use utils::{Point, Rect};
use wayland::*;

use crate::layout::{KeyAction, View};
use crate::render;
use crate::{MechanixKeyboardState, virtual_keyboard};

/// Height in logical px of the Handle — the full-width band at the bottom edge,
/// always drawn, whose tap toggles Bar visibility. It is the bar's entire height
/// when hidden, and is added below the aspect-locked keyboard when shown.
pub const HANDLE_HEIGHT: u32 = 40;

#[derive(Default)]
pub struct WaylandGlobals {
    pub compositor: Option<Handle<WlCompositor>>,
    pub output: Option<Handle<WlOutput>>,
    pub layer_shell: Option<Handle<ZwlrLayerShellV1>>,
    pub dmabuf: Option<Handle<ZwpLinuxDmabufV1>>,
    pub seat: Option<Handle<WlSeat>>,
    pub pointer: Option<Handle<WlPointer>>,
    pub keyboard: Option<Handle<WlKeyboard>>,
    pub touch: Option<Handle<WlTouch>>,
    pub virtual_keyboard_manager: Option<Handle<ZwpVirtualKeyboardManagerV1>>,
    pub virtual_keyboard: Option<Handle<ZwpVirtualKeyboardV1>>,
}

pub struct WindowState {
    pub surface: Handle<WlSurface>,
    pub layer_surface: Handle<ZwlrLayerSurfaceV1>,
    pub slots: Option<[render::Slot; 2]>,
    pub back: usize,
    pub physical_width: u32,
    pub physical_height: u32,

    pub logical_width: u32,
    pub logical_height: u32,
    /// The logical height we last asked the compositor for, so we only re-request
    /// (and wait for another `Configure`) when the aspect-derived height changes.
    pub requested_height: u32,
    /// A frame callback fired while the back buffer was still in flight; draw as
    /// soon as its `wl_buffer.release` lands.
    pub pending_frame: bool,
    /// Bar visibility: `true` shows the keyboard above the Handle, `false` shows
    /// only the Handle. Flipped by a Handle tap; drives the surface height. Starts
    /// hidden.
    pub visible: bool,
}

/// Kick off the registry roundtrip that discovers the globals.
fn on_start(s: &mut MechanixKeyboardState, _: &app::Start) {
    s.wayland.display().get_registry();
    s.wayland.display().sync();
}

/// Push queued requests to the compositor each poll.
fn on_pre_poll(s: &mut MechanixKeyboardState, _: &app::PrePoll) {
    s.wayland.proxy().flush();
}

/// Bind the globals the bar needs as the registry advertises them.
fn on_registry(s: &mut MechanixKeyboardState, event: &WlRegistryEvent) {
    let WlRegistryEvent::Global {
        sender,
        name,
        interface,
        version,
    } = event
    else {
        return;
    };
    match interface.as_str() {
        WlCompositor::NAME => s.globals.compositor = Some(sender.bind(*name, *version)),
        ZwlrLayerShellV1::NAME => s.globals.layer_shell = Some(sender.bind(*name, *version)),
        WlOutput::NAME => s.globals.output = Some(sender.bind(*name, *version)),
        ZwpLinuxDmabufV1::NAME => s.globals.dmabuf = Some(sender.bind(*name, *version)),
        WlSeat::NAME => s.globals.seat = Some(sender.bind(*name, *version)),
        ZwpVirtualKeyboardManagerV1::NAME => {
            s.globals.virtual_keyboard_manager = Some(sender.bind(*name, *version))
        }
        _ => {}
    }
}

/// Bind keyboard/pointer/touch as the seat reports having them.
fn on_seat(s: &mut MechanixKeyboardState, event: &WlSeatEvent) {
    let WlSeatEvent::Capabilities { capabilities, .. } = event else {
        return;
    };
    let Some(seat) = s.globals.seat.clone() else {
        return;
    };
    if capabilities.contains(WlSeatCapability::Keyboard) && s.globals.keyboard.is_none() {
        s.globals.keyboard = Some(seat.get_keyboard());
    }
    if capabilities.contains(WlSeatCapability::Pointer) && s.globals.pointer.is_none() {
        s.globals.pointer = Some(seat.get_pointer());
    }
    if capabilities.contains(WlSeatCapability::Touch) && s.globals.touch.is_none() {
        s.globals.touch = Some(seat.get_touch());
    }
}

/// Track the output's buffer-scale factor (HiDPI). Drives the physical buffer
/// size and `wl_surface.set_buffer_scale` so text stays crisp on a 2× display.
fn on_output(s: &mut MechanixKeyboardState, event: &WlOutputEvent) {
    let WlOutputEvent::Scale { factor, .. } = event else {
        return;
    };
    if *factor > 0 {
        s.scale = *factor;
        tracing::info!("output buffer-scale: {factor}");
    }
}

/// Registry roundtrip done: globals are in, so create the layer surface and
/// commit it (no buffer yet — that waits for the first `configure`).
fn create_window(s: &mut MechanixKeyboardState) {
    if s.window.is_some() {
        return;
    }
    let (Some(compositor), Some(layer_shell)) = (&s.globals.compositor, &s.globals.layer_shell)
    else {
        return;
    };

    let surface = compositor.create_surface();
    let layer_surface = layer_shell.get_layer_surface(
        &surface,
        None,
        ZwlrLayerShellV1Layer::Top,
        "mechanix-keyboard",
    );
    // Start hidden: the bar maps as just the Handle. Its height is fixed (no
    // aspect dance needed), so the first Configure matches this request directly.
    layer_surface.set_size(0, HANDLE_HEIGHT);
    layer_surface.set_anchor(
        ZwlrLayerSurfaceV1Anchor::Bottom
            | ZwlrLayerSurfaceV1Anchor::Left
            | ZwlrLayerSurfaceV1Anchor::Right,
    );
    // Reserve a constant Handle-height zone in both states, so toggling never
    // reflows other clients; a shown keyboard overlaps the app's bottom content.
    layer_surface.set_exclusive_zone(HANDLE_HEIGHT as i32);
    layer_surface.set_keyboard_interactivity(ZwlrLayerSurfaceV1KeyboardInteractivity::None);
    surface.commit();

    s.window = Some(WindowState {
        surface,
        layer_surface,
        slots: None,
        back: 0,
        physical_width: 0,
        physical_height: 0,
        logical_width: 0,
        logical_height: 0,
        requested_height: HANDLE_HEIGHT,
        pending_frame: false,
        visible: false,
    });
}

/// One `wl_callback.done`: either a frame callback we requested (repaint) or the
/// initial registry roundtrip (create the surface).
fn on_callback(s: &mut MechanixKeyboardState, event: &WlCallbackEvent) {
    let WlCallbackEvent::Done { sender, .. } = event;
    let Some(id) = sender.object_id() else {
        return;
    };
    if s.frame_callbacks.remove(&id) {
        // Frame callback: repaint (static colour today, ready for a live UI
        // once there are keys to draw).
        render::render(s);
    } else {
        create_window(s);
    }
}

/// Compositor sized the surface. The layer-shell grants the width (we anchor
/// left+right); we infer the height from it, aspect-locked to the rendered
/// view. On the first `Configure` the granted height won't match that inference,
/// so we re-request the derived height and wait for the next `Configure`; once
/// it matches, we allocate physical-resolution slots and present the first frame.
fn on_configure(s: &mut MechanixKeyboardState, event: &ZwlrLayerSurfaceV1Event) {
    let ZwlrLayerSurfaceV1Event::Configure {
        serial,
        width,
        height: _,
        ..
    } = event
    else {
        return;
    };
    let Some(dmabuf) = s.globals.dmabuf.clone() else {
        return;
    };
    let scale = s.scale.max(1);

    // The current view's intrinsic logical size drives the keyboard's aspect
    // ratio. All views share it, so the keyboard height is sized from one view
    // and never resized on a view switch. Bar visibility is a separate axis that
    // *does* resize: the Handle height is added when shown and is the whole bar
    // when hidden.
    let (view_w, view_h) = {
        let Some(view) = s.current_view() else {
            return;
        };
        (view.width(), view.height())
    };
    if view_w <= 0.0 {
        return;
    }

    // Resolve width, ack, and decide whether we still need to re-request height.
    let ready = {
        let Some(window) = s.window.as_mut() else {
            return;
        };
        window.layer_surface.ack_configure(*serial);

        let logical_w = if *width == 0 {
            window.logical_width.max(1)
        } else {
            *width
        };
        window.logical_width = logical_w;

        // Shown: aspect-locked keyboard height plus the Handle. Hidden: Handle
        // only, a fixed height that never depends on the granted width.
        let desired_h = if window.visible {
            let kb_h = (view_h * logical_w as f32 / view_w).round() as u32;
            if kb_h == 0 {
                return;
            }
            kb_h + HANDLE_HEIGHT
        } else {
            HANDLE_HEIGHT
        };

        if window.requested_height != desired_h {
            // Ask for the height this visibility state wants and wait for the
            // next Configure.
            window.requested_height = desired_h;
            window.layer_surface.set_size(0, desired_h);
            window.surface.commit();
            None
        } else {
            Some((logical_w, desired_h))
        }
    };
    let Some((logical_w, logical_h)) = ready else {
        return;
    };

    // (Re)allocate physical-resolution slots whenever the buffer size changes —
    // at first map, and on every visibility toggle that resizes the bar.
    let (buf_w, buf_h) = (logical_w * scale as u32, logical_h * scale as u32);
    let needs_alloc = s.window.as_ref().is_some_and(|w| {
        w.slots.is_none() || w.physical_width != buf_w || w.physical_height != buf_h
    });
    if needs_alloc {
        {
            let window = s.window.as_mut().expect("window exists");
            window.logical_height = logical_h;
            window.physical_width = buf_w;
            window.physical_height = buf_h;
            window.surface.set_buffer_scale(scale);
        }
        let slots = render::alloc_slots(&mut s.renderer, &dmabuf, buf_w, buf_h);
        s.window.as_mut().expect("window exists").slots = Some(slots);
    }

    render::render(s);
}

/// The compositor handed a buffer back; mark it drawable and service any frame
/// that was waiting on it.
fn on_buffer_release(s: &mut MechanixKeyboardState, event: &WlBufferEvent) {
    let WlBufferEvent::Release { sender } = event;
    let Some(id) = sender.object_id() else {
        return;
    };
    if let Some(slots) = s.window.as_mut().and_then(|w| w.slots.as_mut()) {
        for slot in slots.iter_mut() {
            if slot.buffer_id == id {
                slot.released = true;
            }
        }
    }
    if s.window.as_ref().map_or(false, |w| w.pending_frame) {
        render::render(s);
    }
}

// ── input → interactivity ──────────────────────────────────────────────────

fn on_keyboard(s: &mut MechanixKeyboardState, event: &WlKeyboardEvent) {
    s.interactivity.call_before_frame();
    s.interactivity.process_keyboard(event);
    tracing::debug!(
        just_pressed = ?s.interactivity.keyboard.just_pressed_keys(),
        just_released = ?s.interactivity.keyboard.just_released_keys(),
        modifiers = ?s.interactivity.keyboard.modifiers(),
        "keyboard input",
    );
}

fn on_pointer(s: &mut MechanixKeyboardState, event: &WlPointerEvent) {
    s.interactivity.call_before_frame();
    s.interactivity.process_pointer(event);

    // Copy the surface-local points out before the keymap borrow, so the
    // interactivity borrow is released for the hit-test below.
    let position = s.interactivity.pointer.position();
    let pressed = s
        .interactivity
        .pointer
        .just_pressed_position(MouseButton::Left)
        .copied();

    // A click on the Handle toggles Bar visibility, in either state. The Handle
    // sits below the keys, so it never overlaps a key's touch area.
    if let Some(hr) = handle_rect(s) {
        if pressed.is_some_and(|p| hr.contains_point(p)) {
            toggle_visibility(s);
            return;
        }
    }

    // Keys are only live while shown; when hidden, clear any stale hover.
    if !s.window.as_ref().is_some_and(|w| w.visible) {
        if s.last_hover.take().is_some() {
            tracing::info!("hover: none");
        }
        return;
    }

    // Resolve the hover label and the clicked key's action while the keymap is
    // borrowed, then act after the borrow ends (emitting needs `&mut s`).
    let (hover, clicked) = {
        let Some((view, f)) = view_and_factor(s) else {
            return;
        };
        let hover = key_at(view, f, position);
        let clicked = pressed.and_then(|p| action_at(view, f, p));
        (hover, clicked)
    };

    // Click: type the key the left button went down on this frame.
    if let Some(action) = clicked {
        dispatch_action(s, &action);
    }

    // Hover: print only when the key under the pointer changes.
    if hover != s.last_hover {
        match &hover {
            Some(label) => tracing::info!(key = %label, "hover"),
            None => tracing::info!("hover: none"),
        }
        s.last_hover = hover;
    }
}

fn on_touch(s: &mut MechanixKeyboardState, event: &WlTouchEvent) {
    s.interactivity.call_before_frame();
    s.interactivity.process_touch(event);

    // A tap on the Handle toggles Bar visibility, in either state. Check it
    // first; the Handle sits below the keys, so it never overlaps a key.
    if let Some(hr) = handle_rect(s) {
        if s.interactivity.touch.tapped(hr) {
            toggle_visibility(s);
            return;
        }
    }

    // Keys are only live while shown.
    if !s.window.as_ref().is_some_and(|w| w.visible) {
        return;
    }

    // Probe each key's touch area for a tap that landed and completed this frame,
    // cloning the tapped key's action out so the keymap borrow ends before we
    // emit (which needs `&mut s`).
    let tapped = {
        let Some((view, f)) = view_and_factor(s) else {
            return;
        };
        view.keys()
            .find(|key| s.interactivity.touch.tapped(scale_rect(key.touch_area, f)))
            .map(|key| key.action.clone())
    };

    if let Some(action) = tapped {
        dispatch_action(s, &action);
    }
}

/// Route a tapped key's action. A view switch mutates the Current view; a
/// modifier latch arms/disarms; both repaint here. Every other action is a
/// keystroke the virtual keyboard emits (which also repaints if it consumes a
/// latch, so the armed highlight clears).
fn dispatch_action(s: &mut MechanixKeyboardState, action: &KeyAction) {
    let target = match action {
        KeyAction::SetView(name) => name.as_str(),
        KeyAction::ToggleView { lock, unlock } => {
            // Toggle by current view: if the lock view is already showing, go
            // back to `unlock`; otherwise switch to `lock`.
            let current = s.current_view().map(|v| v.name.as_str());
            if current == Some(lock.as_str()) {
                unlock.as_str()
            } else {
                lock.as_str()
            }
        }
        KeyAction::LatchModifier(name) => {
            // Arm/disarm the modifier and repaint so its key shows the change.
            virtual_keyboard::toggle_latch(s, name);
            render::render(s);
            return;
        }
        _ => {
            // Emit; if a latch was armed, it's now consumed, so repaint to drop
            // the highlight. Compare the armed count across the emit.
            let armed_before = s.virtual_keyboard_state.latched.len();
            virtual_keyboard::emit_action(s, action);
            if s.virtual_keyboard_state.latched.len() != armed_before {
                render::render(s);
            }
            return;
        }
    };
    switch_view(s, target);
}

/// Switch the Current view to the named one and repaint. A no-op (no repaint) if
/// the name is unknown or already current.
fn switch_view(s: &mut MechanixKeyboardState, name: &str) {
    let Some(idx) = s.keymap.as_ref().and_then(|km| km.index_of(name)) else {
        tracing::warn!(view = %name, "view switch to unknown view; ignored");
        return;
    };
    if idx == s.current_view {
        return;
    }
    s.current_view = idx;
    tracing::info!(view = %name, "switched view");
    render::render(s);
}

/// The Handle's rect in surface-local coordinates: the bottom `HANDLE_HEIGHT`
/// band, full width. `None` until the surface size is known. When hidden the
/// bar is only this tall, so the Handle is the whole surface.
fn handle_rect(s: &MechanixKeyboardState) -> Option<Rect> {
    let window = s.window.as_ref()?;
    if window.logical_width == 0 || window.logical_height == 0 {
        return None;
    }
    let h = HANDLE_HEIGHT as f32;
    Some(Rect::new(
        0.0,
        window.logical_height as f32 - h,
        window.logical_width as f32,
        h,
    ))
}

/// Flip Bar visibility and re-request the matching bar height. The resulting
/// `Configure` reallocates slots at the new size and repaints.
fn toggle_visibility(s: &mut MechanixKeyboardState) {
    let logical_w = match s.window.as_ref() {
        Some(w) if w.logical_width > 0 => w.logical_width,
        _ => return,
    };
    let (view_w, view_h) = match s.current_view() {
        Some(v) if v.width() > 0.0 => (v.width(), v.height()),
        _ => return,
    };
    let window = s.window.as_mut().expect("window exists");
    window.visible = !window.visible;
    let target = if window.visible {
        (view_h * logical_w as f32 / view_w).round() as u32 + HANDLE_HEIGHT
    } else {
        HANDLE_HEIGHT
    };
    window.requested_height = target;
    window.layer_surface.set_size(0, target);
    window.surface.commit();
    tracing::info!(visible = window.visible, target, "toggled bar visibility");
}

/// The rendered view and the factor mapping its logical layout units onto
/// surface-local (input) coordinates: `f = logical_width / view_width`. Returns
/// `None` until the keymap and surface width are known.
fn view_and_factor(s: &MechanixKeyboardState) -> Option<(&View, f32)> {
    let view = s.current_view()?;
    let view_w = view.width();
    let logical_w = s.window.as_ref()?.logical_width;
    if view_w <= 0.0 || logical_w == 0 {
        return None;
    }
    Some((view, logical_w as f32 / view_w))
}

/// Label of the first key whose (scaled) touch area contains `p`, else `None`.
fn key_at(view: &View, f: f32, p: Point) -> Option<String> {
    view.keys()
        .find(|k| scale_rect(k.touch_area, f).contains_point(p))
        .map(|k| k.display_label().to_string())
}

/// Action of the first key whose (scaled) touch area contains `p`, cloned.
fn action_at(view: &View, f: f32, p: Point) -> Option<KeyAction> {
    view.keys()
        .find(|k| scale_rect(k.touch_area, f).contains_point(p))
        .map(|k| k.action.clone())
}

/// Scale a layout-unit rect into surface-local coordinates.
fn scale_rect(r: Rect, f: f32) -> Rect {
    Rect::new(r.x() * f, r.y() * f, r.width() * f, r.height() * f)
}

pub fn module<S>() -> impl app::RegisteredModule<MechanixKeyboardState, S> {
    app::Module::new()
        .on(on_start)
        .on(on_pre_poll)
        .on(on_registry)
        .on(on_seat)
        .on(on_output)
        .on(on_callback)
        .on(on_configure)
        .on(on_buffer_release)
        .on(on_keyboard)
        .on(on_pointer)
        .on(on_touch)
}
