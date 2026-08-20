use std::collections::HashSet;

use app::prelude::*;
use interactivity::InteractivityState;
use io_ring::Ring;
use renderer::Renderer;
use wayland::*;

mod layout;
mod render;
mod virtual_keyboard;
mod window;

use window::{WaylandGlobals, WindowState};

use crate::virtual_keyboard::VirtualKeyboardState;

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
    virtual_keyboard_state: Option<VirtualKeyboardState>,
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
            virtual_keyboard_state: None,
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
        .mount(layout::module())
        .mount(virtual_keyboard::module());

    app.dispatch(&app::Start);
    loop {
        app.dispatch(&app::PrePoll);
        app.dispatch(&app::Poll);
    }
}
