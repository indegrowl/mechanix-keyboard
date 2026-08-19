use serde::Deserialize;
use std::collections::BTreeMap;
use std::{env, fs};
use tracing::{info, warn};

use crate::MechanixKeyboardState;

#[derive(Debug, Deserialize)]
struct Layout {
    outlines: BTreeMap<String, Outline>,
    views: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct Outline {
    width: f32,
    height: f32,
}

static FALLBACK_LAYOUT: &str = include_str!("../resources/layout.yaml");

/// Load layout on startup.
pub fn module<S>() -> impl app::RegisteredModule<MechanixKeyboardState, S> {
    app::Module::new().on(|_s: &mut MechanixKeyboardState, _: &app::Start| {
        let contents: String;
        match env::var("MECHA_KBD_LAYOUT") {
            Ok(path) => {
                contents = fs::read_to_string(path).expect("Error: Failed to read layout file")
            }
            Err(_) => {
                warn!("Warning: No config path provided! Using fallback...");
                contents = FALLBACK_LAYOUT.into();
            }
        }

        let layout: Layout = yaml_serde::from_str(&contents).expect("Error: Failed to parse yaml");

        for (view_name, rows) in &layout.views {
            info!("view: {view_name}");
            for row in rows {
                info!("  {row}");
            }
        }
    })
}
