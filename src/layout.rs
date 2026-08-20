use serde::Deserialize;
use std::collections::BTreeMap;
use std::{env, fs};
use tracing::{info, warn};
use utils::Rect;

use crate::MechanixKeyboardState;

// ── Squeekboard-format parse ────────────────────────────────────────────────
//
// The raw shape of a squeekboard `.yaml` layout. We only pull out what a
// render-only IR needs: outline sizes, view rows, and each button's outline +
// label. `action`/`keysym`/`modifier`/`text`/`icon` are deserialized-over —
// serde ignores fields we don't name — and belong to the deferred action model.

#[derive(Debug, Deserialize)]
struct Layout {
    outlines: BTreeMap<String, Outline>,
    views: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    buttons: BTreeMap<String, Button>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct Outline {
    width: f32,
    height: f32,
}

#[derive(Debug, Default, Deserialize)]
struct Button {
    outline: Option<String>,
    label: Option<String>,
}

/// Size used when a button names an outline that isn't defined (and no
/// `default` outline exists to fall back to). Keeps conversion total.
const FALLBACK_OUTLINE: Outline = Outline {
    width: 50.0,
    height: 50.0,
};

#[derive(Debug)]
pub struct Keymap {
    pub views: Vec<View>,
}

/// One selectable arrangement of keys (e.g. `base`, `upper`).
#[derive(Debug)]
pub struct View {
    pub name: String,
    pub rows: Vec<Row>,
}

/// One horizontal line of keys within a view.
#[derive(Debug)]
pub struct Row {
    pub keys: Vec<Key>,
}

/// One drawable key: what to draw, and where (logical units).
#[derive(Debug)]
pub struct Key {
    pub label: String,
    pub rect: Rect,
}

impl Row {
    /// The row's keys.
    pub fn keys(&self) -> &[Key] {
        &self.keys
    }
}

impl View {
    /// Every key in the view, flattened across its rows.
    pub fn keys(&self) -> impl Iterator<Item = &Key> {
        self.rows.iter().flat_map(|row| row.keys.iter())
    }

    /// The view's intrinsic logical width: the right edge of its widest row.
    pub fn width(&self) -> f32 {
        self.keys().map(|k| k.rect.right()).fold(0.0_f32, f32::max)
    }

    /// The view's intrinsic logical height: the bottom edge of its last row.
    pub fn height(&self) -> f32 {
        self.keys().map(|k| k.rect.bottom()).fold(0.0_f32, f32::max)
    }
}

impl Keymap {
    /// Every key in the keymap, flattened across every view.
    pub fn keys(&self) -> impl Iterator<Item = &Key> {
        self.views.iter().flat_map(|view| view.keys())
    }

    /// Look a view up by name (e.g. the initial `base` view to render).
    pub fn view(&self, name: &str) -> Option<&View> {
        self.views.iter().find(|v| v.name == name)
    }

    /// Convert a parsed squeekboard layout into the IR, resolving each key's
    /// label and geometry.
    fn from_layout(layout: &Layout) -> Self {
        let views = layout
            .views
            .iter()
            .map(|(name, rows)| View::resolve(layout, name, rows))
            .collect();
        Keymap { views }
    }
}

impl View {
    /// Resolve one view's rows into laid-out keys.
    ///
    /// Two passes: first size every key (label + outline), then flow them.
    /// Keys butt together left-to-right with no gap; each row's height is the
    /// max key height in it; rows stack top-down; and each row is centred
    /// within the view's width (the widest row), matching squeekboard's look.
    fn resolve(layout: &Layout, name: &str, rows: &[String]) -> View {
        // Pass 1: resolve label + size for every key, grouped by row.
        let sized: Vec<Vec<(String, Outline)>> = rows
            .iter()
            .map(|row| {
                row.split_whitespace()
                    .map(|token| {
                        let button = layout.buttons.get(token);
                        let outline = resolve_outline(layout, button);
                        let label = button
                            .and_then(|b| b.label.clone())
                            .unwrap_or_else(|| token.to_string());
                        (label, outline)
                    })
                    .collect()
            })
            .collect();

        // The view is as wide as its widest row.
        let view_width = sized
            .iter()
            .map(|row| row.iter().map(|(_, o)| o.width).sum::<f32>())
            .fold(0.0_f32, f32::max);

        // Pass 2: flow each row, centred, stacking downward.
        let mut y = 0.0_f32;
        let mut out_rows = Vec::with_capacity(sized.len());
        for row in &sized {
            let row_width: f32 = row.iter().map(|(_, o)| o.width).sum();
            let row_height = row.iter().map(|(_, o)| o.height).fold(0.0_f32, f32::max);
            let mut x = (view_width - row_width) / 2.0;
            let keys = row
                .iter()
                .map(|(label, o)| {
                    let key = Key {
                        label: label.clone(),
                        rect: Rect::new(x, y, o.width, o.height),
                    };
                    x += o.width;
                    key
                })
                .collect();
            out_rows.push(Row { keys });
            y += row_height;
        }

        View {
            name: name.to_string(),
            rows: out_rows,
        }
    }
}

/// Resolve a button's outline size: the button's named outline, else the
/// `default` outline, else a hard fallback. Warns on a dangling outline name.
fn resolve_outline(layout: &Layout, button: Option<&Button>) -> Outline {
    let name = button
        .and_then(|b| b.outline.as_deref())
        .unwrap_or("default");
    if let Some(outline) = layout.outlines.get(name) {
        return *outline;
    }
    warn!("outline {name:?} not defined; falling back to `default`");
    layout
        .outlines
        .get("default")
        .copied()
        .unwrap_or(FALLBACK_OUTLINE)
}

static FALLBACK_LAYOUT: &str = include_str!("../resources/layout.yaml");

/// Load and convert the layout on startup, storing the IR on state.
pub fn module<S>() -> impl app::RegisteredModule<MechanixKeyboardState, S> {
    app::Module::new().on(|s: &mut MechanixKeyboardState, _: &app::Start| {
        let contents = match env::var("MECHA_KBD_LAYOUT") {
            Ok(path) => fs::read_to_string(path).expect("Error: Failed to read layout file"),
            Err(_) => {
                let local = std::path::Path::new("layout.yaml");
                if local.is_file() {
                    info!("Using layout.yaml from working directory");
                    fs::read_to_string(local).expect("Error: Failed to read layout file")
                } else {
                    warn!("Warning: No config path provided! Using fallback...");
                    FALLBACK_LAYOUT.into()
                }
            }
        };

        let layout: Layout = yaml_serde::from_str(&contents).expect("Error: Failed to parse yaml");
        let keymap = Keymap::from_layout(&layout);

        info!("loaded keymap: {} view(s)", keymap.views.len());
        for view in &keymap.views {
            info!("  view {}: {} key(s)", view.name, view.keys().count());
        }

        s.keymap = Some(keymap);
    })
}
