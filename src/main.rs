#![recursion_limit = "1024"]
use std::collections::HashSet;

use app::prelude::*;
use interactivity::InteractivityState;
use io_ring::Ring;
use renderer::{Renderer, TextureId};
use wayland::*;

mod layout;
mod render;
mod window;

/// The one Keymap view we render this pass. View switching is a later task.
pub const RENDER_VIEW: &str = "base";

/// Baked glyph atlas + font consts generated at build time by `assets::builder`.
pub mod atlas {
    include!(concat!(env!("OUT_DIR"), "/keyboard_gen.rs"));
}

/// The icon dictionary — `icon name -> SpriteRegion` — generated at build time
/// from `config.toml`. References the sprite consts in `atlas`.
pub mod icons {
    include!(concat!(env!("OUT_DIR"), "/icons_gen.rs"));
}

use window::{WaylandGlobals, WindowState};

#[derive(State)]
pub struct MechanixKeyboardState {
    ring: Ring,
    wayland: Wayland,
    #[lens(skip)]
    globals: WaylandGlobals,
    #[lens(skip)]
    renderer: Renderer,
    #[lens(skip)]
    interactivity: InteractivityState,
    #[lens(skip)]
    window: Option<WindowState>,
    #[lens(skip)]
    frame_callbacks: HashSet<ObjectId>,
    #[lens(skip)]
    keymap: Option<layout::Keymap>,
    /// The uploaded glyph atlas, resolved once the renderer has a GL context.
    #[lens(skip)]
    atlas_texture: Option<TextureId>,
    /// Output buffer-scale factor (HiDPI); read from `wl_output.scale`.
    #[lens(skip)]
    scale: i32,
    #[lens(skip)]
    last_hover: Option<String>,
}

impl MechanixKeyboardState {
    fn new() -> Self {
        let ring = Ring::default();
        let wayland = Wayland::new(ring.proxy());
        let renderer = Renderer::new().expect("renderer init failed");
        Self {
            ring,
            wayland,
            globals: WaylandGlobals::default(),
            renderer,
            interactivity: InteractivityState::new(),
            window: None,
            frame_callbacks: HashSet::new(),
            keymap: None,
            atlas_texture: None,
            scale: 1,
            last_hover: None,
        }
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
        )
        .init();

    let state = MechanixKeyboardState::new();
    let mut app = App::new(state)
        .mount(io_ring::module())
        .mount(wayland::module())
        .mount(render::module())
        .mount(window::module())
        .mount(layout::module());

    app.dispatch(&app::Start);
    loop {
        app.dispatch(&app::PrePoll);
        app.dispatch(&app::Poll);
    }
}
