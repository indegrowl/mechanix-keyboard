use std::os::fd::AsFd;

use assets::BakedFont;
use renderer::commands::{
    ClearColor, Color, DrawMonochromeSprite, DrawRect, DrawText, Point, Rect, Size,
};
use renderer::{DmaBuf, RenderableSurface, Renderer, TextureId};
use wayland::{Handle, ObjectId, WlBuffer, ZwpLinuxBufferParamsV1Flags, ZwpLinuxDmabufV1};

use crate::layout::KeyFace;
use crate::{MechanixKeyboardState, RENDER_VIEW, atlas, layout};

/// Background the keyboard bar clears to, behind the keys.
const CLEAR: Color = Color::from_rgb8(24, 24, 32);
/// Fill colour of each key box.
const KEY_BG: Color = Color::from_rgb8(46, 52, 72);
/// Colour of the key label text.
const KEY_LABEL: Color = Color::from_rgb8(220, 226, 240);
/// The baked font used for labels (ASCII 32..126 only).
const FONT: &BakedFont = &atlas::KEYBOARD_FONT_ROBOTO_64;

/// Logical inset applied to every key box so adjacent keys — which butt together
/// in the IR — show a visible gap. Scaled to pixels alongside the layout.
const KEY_INSET: f32 = 2.0;
/// Depth of the key box (far) and its label (near). `GEQUAL` depth test: larger
/// z wins, so the label draws over its box.
const Z_BOX: f32 = 0.10;
const Z_TEXT: f32 = 0.20;

/// Icon side length as a fraction of the key's inner height. Icons are square and
/// geometrically centred; expect to tune this once it's visible on the Comet.
const ICON_SCALE: f32 = 0.5;

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

/// Renderer setup: bring the GPU pipelines up and upload the glyph atlas before
/// any surface is drawn. The GL context is current from `Renderer::new`, so the
/// texture upload is safe here at `Start`.
pub fn module<S>() -> impl app::RegisteredModule<MechanixKeyboardState, S> {
    app::Module::new().on(|s: &mut MechanixKeyboardState, _: &app::Start| {
        s.renderer.init_pipelines();
        match s.renderer.upload_atlas(&atlas::KEYBOARD) {
            Ok(id) => s.atlas_texture = Some(id),
            Err(e) => tracing::error!("atlas upload failed: {e:?}"),
        }
    })
}

/// Draw one view's keys into the active surface: a filled box per key, its face
/// (text label or icon) centred on top. `buffer_w` is the physical buffer width;
/// the whole view is
/// scaled uniformly to fill it (`k = buffer_w / view_width`), which keeps the
/// layout's aspect ratio — the bar's height was sized to match (see `window`).
fn draw_view(renderer: &mut Renderer, view: &layout::View, tex: Option<TextureId>, buffer_w: f32) {
    let view_w = view.width();
    if view_w <= 0.0 {
        return;
    }
    let k = buffer_w / view_w;
    let inset = KEY_INSET * k;

    for key in view.keys() {
        let r = key.rect;
        let bx = r.x() * k + inset;
        let by = r.y() * k + inset;
        let bw = (r.width() * k - 2.0 * inset).max(0.0);
        let bh = (r.height() * k - 2.0 * inset).max(0.0);

        renderer.send_command(DrawRect {
            color: KEY_BG,
            origin: Point::new(bx, by),
            z: Z_BOX,
            size: Size::new(bw, bh),
        });

        // The face draws over the box. Text still only covers ASCII 32..126 (the
        // baked font); an icon is a baked sprite tinted with the label colour.
        let Some(tex) = tex else { continue };
        match &key.face {
            KeyFace::Text(text) => {
                if text.trim().is_empty() {
                    continue;
                }
                let text_w = FONT.measure_width(text);
                let tx = bx + (bw - text_w) / 2.0;
                let baseline = by + FONT.get_baseline_offset(bh);
                renderer.send_command(DrawText {
                    font: FONT,
                    texture_id: tex,
                    text: text.clone(),
                    origin: Point::new(tx, baseline),
                    z: Z_TEXT,
                    color: KEY_LABEL,
                    background: KEY_BG,
                    is_opaque: true,
                });
            }
            KeyFace::Icon(region) => {
                // Square, sized to a fraction of the key's inner height, centred
                // on both axes (an icon has no text baseline to sit on).
                let side = bh * ICON_SCALE;
                let ix = bx + (bw - side) / 2.0;
                let iy = by + (bh - side) / 2.0;
                renderer.send_command(DrawMonochromeSprite {
                    texture_id: tex,
                    region: Rect::new(region.x, region.y, region.w, region.h),
                    origin: Point::new(ix, iy),
                    z: Z_TEXT,
                    size: Size::new(side, side),
                    color: KEY_LABEL,
                    background: KEY_BG,
                    is_opaque: true,
                });
            }
        }
    }
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
    // Physical buffer dims for scissor/scaling; logical dims for surface damage
    // (damage is in surface-local coordinates, which are pre-buffer-scale).
    let buffer_w = window.width;
    let (lw, lh) = (window.logical_width, window.logical_height);

    s.renderer.active_surface(&slots[back].surface);
    s.renderer.set_scissor(None);
    s.renderer.send_command(ClearColor(CLEAR));

    if let Some(view) = s.keymap.as_ref().and_then(|km| km.view(RENDER_VIEW)) {
        draw_view(&mut s.renderer, view, s.atlas_texture, buffer_w as f32);
    }

    s.renderer.render_frame();
    s.renderer.finish();

    let window = s.window.as_mut().expect("window still present");
    let slots = window.slots.as_mut().expect("slots still present");
    window.surface.attach(Some(&slots[back].buffer), 0, 0);
    window.surface.damage(0, 0, lw as i32, lh as i32);
    let cb = window.surface.frame();
    window.surface.commit();
    slots[back].released = false;
    window.back ^= 1;
    window.pending_frame = false;

    if let Some(id) = cb.object_id() {
        s.frame_callbacks.insert(id);
    }
}
