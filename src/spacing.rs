//! `MechanixTheme` spacing tokens, so `layout.yaml` can set gaps and padding
//! by token. A token is written as its px value, like Figma's `space/8` → `8`.

use mecha_wayland::prelude::Spacing;

/// Every spacing token, smallest first.
const TOKENS: [Spacing; 18] = [
    Spacing::Space0,
    Spacing::Space25,
    Spacing::Space50,
    Spacing::Space75,
    Spacing::Space100,
    Spacing::Space125,
    Spacing::Space150,
    Spacing::Space175,
    Spacing::Space200,
    Spacing::Space250,
    Spacing::Space300,
    Spacing::Space400,
    Spacing::Space450,
    Spacing::Space500,
    Spacing::Space600,
    Spacing::Space700,
    Spacing::Space800,
    Spacing::Space900,
];

/// The spacing token whose px value is exactly `px`, or `None` if there isn't one.
pub fn from_px(px: f32) -> Option<Spacing> {
    TOKENS.into_iter().find(|s| s.dp() == px)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_values_map_to_spacing() {
        assert_eq!(from_px(8.0), Some(Spacing::Space100));
        assert_eq!(from_px(12.0), Some(Spacing::Space150));
    }

    #[test]
    fn non_token_value_is_none() {
        assert_eq!(from_px(5.0), None);
        assert_eq!(from_px(8.5), None);
    }
}
