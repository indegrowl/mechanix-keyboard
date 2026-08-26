use interactivity::pointer::MouseButton;
use utils::{Point, Rect};
use wayland::*;

use crate::layout::{KeyAction, View};
use crate::render;
use crate::{MechanixKeyboardState, RENDER_VIEW, virtual_keyboard};

/// Placeholder height requested before the first `Configure` reveals the width.
/// The real height is derived from the granted width (aspect-locked) and
/// re-requested; no buffer is attached until then, so this is never shown.
const INITIAL_HEIGHT: u32 = 100;

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
    layer_surface.set_size(0, INITIAL_HEIGHT);
    layer_surface.set_anchor(
        ZwlrLayerSurfaceV1Anchor::Bottom
            | ZwlrLayerSurfaceV1Anchor::Left
            | ZwlrLayerSurfaceV1Anchor::Right,
    );
    layer_surface.set_exclusive_zone(0);
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
        requested_height: INITIAL_HEIGHT,
        pending_frame: false,
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

    // The rendered view's intrinsic logical size drives the aspect ratio.
    let (view_w, view_h) = {
        let Some(view) = s.keymap.as_ref().and_then(|km| km.view(RENDER_VIEW)) else {
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

        let desired_h = (view_h * logical_w as f32 / view_w).round() as u32;
        if desired_h == 0 {
            return;
        }

        if window.requested_height != desired_h {
            // Ask for the aspect-correct height and wait for the next Configure.
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

    // Height now matches our request — allocate physical-resolution slots once.
    if s.window.as_ref().is_some_and(|w| w.slots.is_none()) {
        let (buf_w, buf_h) = (logical_w * scale as u32, logical_h * scale as u32);
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
        virtual_keyboard::emit_action(s, &action);
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
        virtual_keyboard::emit_action(s, &action);
    }
}

/// The rendered view and the factor mapping its logical layout units onto
/// surface-local (input) coordinates: `f = logical_width / view_width`. Returns
/// `None` until the keymap and surface width are known.
fn view_and_factor(s: &MechanixKeyboardState) -> Option<(&View, f32)> {
    let view = s.keymap.as_ref()?.view(RENDER_VIEW)?;
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
