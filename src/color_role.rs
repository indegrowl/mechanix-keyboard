//! Names for `MechanixTheme` colour roles, so `layout.yaml` can refer to a
//! role (e.g. `surface-container-low`) instead of a hard-coded colour. The
//! names are the kebab-case Figma `Comet/sys/*` tokens.

use mecha_wayland::prelude::ColorRole;

/// The `ColorRole` for a kebab-case role name, or `None` if it isn't one.
pub fn from_name(name: &str) -> Option<ColorRole> {
    use ColorRole::*;
    Some(match name {
        "primary" => Primary,
        "on-primary" => OnPrimary,
        "primary-container" => PrimaryContainer,
        "on-primary-container" => OnPrimaryContainer,
        "secondary" => Secondary,
        "on-secondary" => OnSecondary,
        "secondary-container" => SecondaryContainer,
        "on-secondary-container" => OnSecondaryContainer,
        "error" => Error,
        "on-error" => OnError,
        "error-container" => ErrorContainer,
        "on-error-container" => OnErrorContainer,
        "success" => Success,
        "on-success" => OnSuccess,
        "success-container" => SuccessContainer,
        "on-success-container" => OnSuccessContainer,
        "primary-fixed" => PrimaryFixed,
        "primary-fixed-dim" => PrimaryFixedDim,
        "on-primary-fixed" => OnPrimaryFixed,
        "on-primary-fixed-variant" => OnPrimaryFixedVariant,
        "secondary-fixed" => SecondaryFixed,
        "secondary-fixed-dim" => SecondaryFixedDim,
        "on-secondary-fixed" => OnSecondaryFixed,
        "on-secondary-fixed-variant" => OnSecondaryFixedVariant,
        "surface-dim" => SurfaceDim,
        "surface" => Surface,
        "surface-bright" => SurfaceBright,
        "surface-container-lowest" => SurfaceContainerLowest,
        "surface-container-low" => SurfaceContainerLow,
        "surface-container" => SurfaceContainer,
        "surface-container-high" => SurfaceContainerHigh,
        "surface-container-highest" => SurfaceContainerHighest,
        "on-surface" => OnSurface,
        "on-surface-variant" => OnSurfaceVariant,
        "outline" => Outline,
        "outline-variant" => OutlineVariant,
        "inverse-surface" => InverseSurface,
        "inverse-on-surface" => InverseOnSurface,
        "inverse-primary" => InversePrimary,
        "scrim" => Scrim,
        "shadow" => Shadow,
        "background" => Background,
        "on-background" => OnBackground,
        "surface-tint" => SurfaceTint,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_names_map_to_roles() {
        assert_eq!(
            from_name("surface-container-low"),
            Some(ColorRole::SurfaceContainerLow)
        );
        assert_eq!(from_name("on-surface"), Some(ColorRole::OnSurface));
    }

    #[test]
    fn unknown_name_is_none() {
        assert_eq!(from_name("surface-contianer-low"), None);
        assert_eq!(from_name("SurfaceContainerLow"), None);
    }
}
