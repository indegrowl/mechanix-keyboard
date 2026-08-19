use std::os::fd::AsFd;

use renderer::commands::{ClearColor, Color};
use renderer::{DmaBuf, RenderableSurface, Renderer};
use wayland::{Handle, ObjectId, WlBuffer, ZwpLinuxBufferParamsV1Flags, ZwpLinuxDmabufV1};

use crate::MechanixKeyboardState;

/// Background the keyboard bar clears to. No keys are drawn yet — this solid
/// fill is only what maps the surface so it can take focus and input.
const CLEAR: Color = Color::from_rgb8(24, 24, 32);
/// DRM fourcc for ARGB8888, matching the renderer's DmaBuf format.
const DRM_FORMAT_ARGB8888: u32 = 0x3432_5241;

/// One dmabuf-backed buffer the bar renders into. Two of these are swapped
/// front/back; `released` tracks whether the compositor has handed the buffer
/// back so we can draw into it again.
pub struct Slot {
    pub surface: RenderableSurface<DmaBuf>,
    pub buffer: Handle<WlBuffer>,
    pub buffer_id: ObjectId,
    pub released: bool,
}

/// Renderer setup: bring the GPU pipelines up before any surface is drawn.
pub fn module<S>() -> impl app::RegisteredModule<MechanixKeyboardState, S> {
    app::Module::new().on(|s: &mut MechanixKeyboardState, _: &app::Start| {
        s.renderer.init_pipelines();
    })
}

/// Allocate the two dmabuf-backed slots the bar double-buffers between.
pub fn alloc_slots(
    renderer: &mut Renderer,
    dmabuf: &Handle<ZwpLinuxDmabufV1>,
    width: u32,
    height: u32,
) -> [Slot; 2] {
    [
        alloc_slot(renderer, dmabuf, width, height),
        alloc_slot(renderer, dmabuf, width, height),
    ]
}

/// Bridge a renderer DmaBuf surface to a `wl_buffer` via `zwp_linux_dmabuf`.
fn alloc_slot(
    renderer: &mut Renderer,
    dmabuf: &Handle<ZwpLinuxDmabufV1>,
    width: u32,
    height: u32,
) -> Slot {
    let surface = renderer
        .create_surface::<DmaBuf>(width, height)
        .expect("DmaBuf surface allocation failed");

    let buffer = {
        let fd = surface.backend.prime_fd.as_fd();
        let stride = surface.backend.stride;
        let modifier = surface.backend.modifier;
        let params = dmabuf.create_params();
        params.add(
            fd,
            0,
            0,
            stride,
            (modifier >> 32) as u32,
            (modifier & 0xffff_ffff) as u32,
        );
        params.create_immed(
            width as i32,
            height as i32,
            DRM_FORMAT_ARGB8888,
            ZwpLinuxBufferParamsV1Flags::empty(),
        )
    };

    let buffer_id = buffer.object_id().expect("live buffer");
    Slot {
        surface,
        buffer,
        buffer_id,
        released: true,
    }
}

/// Draw the clear colour into the back slot and present it, then request the
/// next frame callback. If the back buffer is still held by the compositor,
/// mark the frame pending so `window` can retry it on the next buffer release.
pub fn render(s: &mut MechanixKeyboardState) {
    let Some(window) = s.window.as_mut() else {
        return;
    };
    let Some(slots) = window.slots.as_mut() else {
        return;
    };
    let back = window.back;
    if !slots[back].released {
        window.pending_frame = true;
        return;
    }
    let (w, h) = (window.width, window.height);

    s.renderer.active_surface(&slots[back].surface);
    s.renderer.set_scissor(None);
    s.renderer.send_command(ClearColor(CLEAR));
    s.renderer.render_frame();
    s.renderer.finish();

    window.surface.attach(Some(&slots[back].buffer), 0, 0);
    window.surface.damage(0, 0, w as i32, h as i32);
    let cb = window.surface.frame();
    window.surface.commit();
    slots[back].released = false;
    window.back ^= 1;
    window.pending_frame = false;

    if let Some(id) = cb.object_id() {
        s.frame_callbacks.insert(id);
    }
}
