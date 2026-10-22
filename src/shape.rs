//! Names for `MechanixTheme` shape tokens, so `layout.yaml` can set a corner
//! radius by token (e.g. `none`, `small`) instead of a hard-coded number.

use mecha_wayland::prelude::Shape;

/// The `Shape` for a kebab-case token name, or `None` if it isn't one.
///
/// `full` (a pill, half the component height) is left out: keys are sized by
/// the flex layout, so their height isn't known when the radius is set.
pub fn from_name(name: &str) -> Option<Shape> {
    use Shape::*;
    Some(match name {
        "none" => None,
        "extra-small" => ExtraSmall,
        "small" => Small,
        "medium" => Medium,
        "large" => Large,
        "large-increased" => LargeIncreased,
        "extra-large" => ExtraLarge,
        "extra-large-increased" => ExtraLargeIncreased,
        "extra-extra-large" => ExtraExtraLarge,
        _ => return Option::None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_names_map_to_shapes() {
        assert_eq!(from_name("none"), Some(Shape::None));
        assert_eq!(from_name("small"), Some(Shape::Small));
    }

    #[test]
    fn unknown_or_full_is_none() {
        assert_eq!(from_name("round"), Option::None);
        assert_eq!(from_name("full"), Option::None);
    }
}
